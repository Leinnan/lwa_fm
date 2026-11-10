use std::path::Path;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Weak};

use crux_core::Request;
use crux_core::effects::ResolveSink;
use dirfleet_core::app::{
    ListDir, ListDirOutput, ListTrash, ListView, OrderListing, Ordered, TrashDone, TrashOp,
};
use dirfleet_core::listing::{Listing, is_shown};
use dirfleet_core::search::filter::CompiledQuery;
use rayon::iter::ParallelIterator;
use rayon::slice::ParallelSlice;

use crate::listing_io::read_listing;
use crate::open_history::OpenHistory;
use crate::sort::{AccessTimes, OpeningHistory, sort_order};

enum Job {
    List(Request<ListDir>),
    Order(Request<OrderListing>),
    ListTrash(Request<ListTrash>),
    Trash(Request<TrashOp>),
}

/// Lists and re-orders folders on one background thread, in the order asked.
pub struct ListHandler {
    jobs: Sender<Job>,
}

impl ListHandler {
    pub fn new<R>(sink: Weak<R>, history: Arc<OpenHistory>) -> Self
    where
        R: ResolveSink<ListDir>
            + ResolveSink<OrderListing>
            + ResolveSink<ListTrash>
            + ResolveSink<TrashOp>
            + Send
            + Sync
            + 'static,
    {
        let (jobs, requests) = channel::<Job>();
        std::thread::Builder::new()
            .name("dirfleet_list".into())
            .spawn(move || {
                while let Ok(job) = requests.recv() {
                    let Some(sink) = sink.upgrade() else {
                        return;
                    };
                    let delivered = match job {
                        Job::List(mut request) => {
                            let operation = &request.operation;
                            let output = list_dir_with(
                                Path::new(&operation.path),
                                operation.view.clone(),
                                &history,
                            );
                            sink.resolve_request(&mut request, output)
                        }
                        Job::ListTrash(mut request) => {
                            let output = list_trash(request.operation.view.clone());
                            sink.resolve_request(&mut request, output)
                        }
                        Job::Trash(mut request) => {
                            let output = change_trash(&request.operation);
                            sink.resolve_request(&mut request, output)
                        }
                        Job::Order(mut request) => {
                            let operation = &request.operation;
                            let output = Ordered {
                                listing: Arc::clone(&operation.listing),
                                order: display_order_with(
                                    &operation.listing,
                                    &operation.view,
                                    history.as_ref(),
                                ),
                                view: operation.view.clone(),
                            };
                            sink.resolve_request(&mut request, output)
                        }
                    };
                    if let Err(error) = delivered {
                        log::warn!("listing not delivered: {error}");
                    }
                }
            })
            .expect("start the listing thread");
        Self { jobs }
    }

    pub fn process(&self, request: Request<ListDir>) {
        self.send(Job::List(request));
    }

    pub fn order(&self, request: Request<OrderListing>) {
        self.send(Job::Order(request));
    }

    pub fn list_trash(&self, request: Request<ListTrash>) {
        self.send(Job::ListTrash(request));
    }

    pub fn trash(&self, request: Request<TrashOp>) {
        self.send(Job::Trash(request));
    }

    fn send(&self, job: Job) {
        if self.jobs.send(job).is_err() {
            log::warn!("listing thread stopped");
        }
    }
}

/// The entries of `path` shown with `view`, in display order, with no
/// open history.
pub fn list_dir(path: &Path, view: ListView) -> ListDirOutput {
    list_dir_with(path, view, &OpenHistory::default())
}

/// The entries of `path` shown with `view`, in display order, with when
/// each was opened from `history`.
pub fn list_dir_with(path: &Path, view: ListView, history: &OpenHistory) -> ListDirOutput {
    let Some(listing) = read_listing(path) else {
        let error = std::fs::read_dir(path).err();
        let reason = error
            .as_ref()
            .map_or_else(|| "can't be read".to_owned(), ToString::to_string);
        let message = format!("{}: {reason}", path.display());
        return if error.as_ref().is_some_and(is_denied) {
            ListDirOutput::Denied(message)
        } else {
            ListDirOutput::Failed(message)
        };
    };
    let order = display_order_with(&listing, &view, history);
    ListDirOutput::Listed {
        opened: Arc::new(history.keys_if_any(&listing)),
        listing: Arc::new(listing),
        order,
        view,
    }
}

/// The Trash's entries shown with `view`, in display order.
pub fn list_trash(view: ListView) -> ListDirOutput {
    match crate::trash_bin::listing() {
        Ok(listing) => {
            // Hidden items are shown: the Trash has no other way to reach them.
            let view = ListView {
                show_hidden: true,
                ..view
            };
            let order = display_order(&listing, &view);
            ListDirOutput::Listed {
                listing: Arc::new(listing),
                order,
                view,
                opened: Arc::default(),
            }
        }
        Err(error)
            if error
                .chain()
                .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
                .any(is_denied) =>
        {
            ListDirOutput::Denied(error.to_string())
        }
        Err(error) => ListDirOutput::Failed(error.to_string()),
    }
}

/// Reading was refused: no permission, or (macOS) privacy protection
/// without Full Disk Access, which reports "Operation not permitted".
fn is_denied(error: &std::io::Error) -> bool {
    const EPERM: i32 = 1;
    error.kind() == std::io::ErrorKind::PermissionDenied
        || (cfg!(unix) && error.raw_os_error() == Some(EPERM))
}

fn change_trash(op: &TrashOp) -> TrashDone {
    let done = match op {
        TrashOp::Empty => crate::trash_bin::empty(),
        TrashOp::Purge(keys) => crate::trash_bin::purge(keys),
        TrashOp::Restore(keys) => crate::trash_bin::restore(keys).map(|_| ()),
    };
    TrashDone(done.map_err(|error| error.to_string()))
}

/// Listings this large are filtered in parallel.
const PARALLEL_FILTER_THRESHOLD: usize = 16 * 1024;

/// The entries of `listing` shown with `view`, in display order, with no
/// open history.
pub fn display_order(listing: &Listing, view: &ListView) -> Vec<u32> {
    display_order_with(listing, view, &AccessTimes)
}

/// The entries of `listing` shown with `view`, in display order; "Date
/// opened", "Opens" and `opened:` filters use `history`.
pub fn display_order_with(
    listing: &Listing,
    view: &ListView,
    history: &dyn OpeningHistory,
) -> Vec<u32> {
    let order = sort_order(listing, view.sort, view.invert, history);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let query = CompiledQuery::new(view.filter.as_deref(), now);
    let folded = query
        .as_ref()
        .filter(|query| query.needs_folded)
        .map(|_| listing.folded_names());
    let opened: Vec<u32> = if query.as_ref().is_some_and(|query| query.needs_opened) {
        history
            .keys(listing)
            .into_iter()
            .map(|(_, last)| last)
            .collect()
    } else {
        Vec::new()
    };
    let keep = |index: u32| {
        let index = index as usize;
        is_shown(listing.name(index), listing.meta(index), view.show_hidden)
            && query
                .as_ref()
                .is_none_or(|query| query.matches(listing, folded, &opened, index))
    };
    if order.len() < PARALLEL_FILTER_THRESHOLD {
        return order.into_iter().filter(|&index| keep(index)).collect();
    }
    order
        .par_chunks(4096)
        .map(|chunk| {
            chunk
                .iter()
                .copied()
                .filter(|&index| keep(index))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .concat()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dirfleet_core::Sort;

    fn names(output: &ListDirOutput) -> Vec<String> {
        let ListDirOutput::Listed { listing, order, .. } = output else {
            panic!("listing failed: {output:?}");
        };
        order
            .iter()
            .map(|&index| listing.name(index as usize).to_owned())
            .collect()
    }

    #[test]
    fn lists_folders_first_with_sizes() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("b.txt"), b"hello").expect("write file");
        std::fs::create_dir(dir.path().join("Sub")).expect("create folder");
        let output = list_dir(dir.path(), ListView::default());
        assert_eq!(names(&output), ["Sub", "b.txt"]);
        let ListDirOutput::Listed { listing, order, .. } = output else {
            unreachable!()
        };
        let file = listing.meta(order[1] as usize);
        assert_eq!(file.size, 5);
        assert!(*file.modified_at > 0);
    }

    #[test]
    fn hidden_items_show_only_when_asked_and_clutter_never() {
        let dir = tempfile::tempdir().expect("temp dir");
        for name in ["visible.txt", ".hidden", ".DS_Store"] {
            std::fs::write(dir.path().join(name), b"x").expect("write file");
        }
        assert_eq!(
            names(&list_dir(dir.path(), ListView::default())),
            ["visible.txt"]
        );
        let shown = ListView {
            show_hidden: true,
            ..ListView::default()
        };
        let mut expected = vec![".hidden", "visible.txt"];
        if !cfg!(target_os = "macos") {
            expected.insert(0, ".DS_Store");
        }
        assert_eq!(names(&list_dir(dir.path(), shown)), expected);
    }

    #[test]
    fn the_view_sorts_and_reverses() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("small.txt"), b"x").expect("write file");
        std::fs::write(dir.path().join("big.txt"), vec![b'x'; 100]).expect("write file");
        std::fs::create_dir(dir.path().join("folder")).expect("create folder");
        let by_size = ListView {
            sort: Sort::Size,
            invert: true,
            ..ListView::default()
        };
        assert_eq!(
            names(&list_dir(dir.path(), by_size)),
            ["folder", "big.txt", "small.txt"]
        );
    }

    #[test]
    fn a_missing_folder_fails_with_its_path() {
        let output = list_dir(Path::new("/definitely/not/here"), ListView::default());
        assert!(
            matches!(output, ListDirOutput::Failed(message) if message.contains("/definitely/not/here"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_without_read_permission_is_denied() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("temp dir");
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).expect("create");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("lock");
        let output = list_dir(&locked, ListView::default());
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("unlock");
        assert!(
            matches!(&output, ListDirOutput::Denied(message) if message.contains("locked")),
            "{output:?}"
        );
    }

    #[test]
    fn a_search_filters_what_is_listed_and_a_walk_finds_it_below() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("deep/inner")).expect("create folders");
        std::fs::write(dir.path().join("notes.txt"), b"x").expect("write file");
        std::fs::write(dir.path().join("other.md"), b"x").expect("write file");
        std::fs::write(dir.path().join("deep/inner/notes.md"), b"x").expect("write file");
        let filter = dirfleet_core::search::Search {
            value: "NOTES".into(),
            ..Default::default()
        };
        let view = ListView {
            filter: Some(Arc::new(filter)),
            ..ListView::default()
        };
        assert_eq!(names(&list_dir(dir.path(), view.clone())), ["notes.txt"]);

        let cancel = std::sync::atomic::AtomicBool::new(false);
        let found = crate::walk::walk(&[dir.path().to_path_buf()], 3, false, &cancel, &|_| {});
        let order = display_order(&found.listing, &view);
        let mut names: Vec<_> = order
            .iter()
            .map(|&index| found.listing.name(index as usize).to_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["notes.md", "notes.txt"]);
        for &index in &order {
            let meta = found.listing.meta(index as usize);
            assert_eq!(
                (meta.entry_type, meta.size),
                (dirfleet_core::listing::EntryType::File, 1),
                "a walk reads sizes like a listing"
            );
        }
    }

    #[test]
    fn listings_carry_the_open_history_and_sort_by_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        for name in ["a.txt", "b.txt"] {
            std::fs::write(dir.path().join(name), b"x").expect("write file");
        }
        let history = OpenHistory::load(None);
        history.record(&[dir.path().join("b.txt"), dir.path().join("b.txt")]);
        let view = ListView {
            sort: Sort::MostOpened,
            invert: true,
            ..ListView::default()
        };
        let output = list_dir_with(dir.path(), view, &history);
        assert_eq!(names(&output), ["b.txt", "a.txt"], "the most opened first");
        let ListDirOutput::Listed {
            listing, opened, ..
        } = output
        else {
            unreachable!()
        };
        let b = (0..listing.len())
            .find(|&i| listing.name(i) == "b.txt")
            .expect("b");
        assert_eq!(opened[b].0, 2);
    }
}
