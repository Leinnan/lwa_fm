//! Compares two folders: which files are in one of them only, and which
//! are in both but not alike.
//!
//! Both folders are walked without following links, and their files are
//! paired by their path below each folder: first as written, then, where
//! disks usually take one spelling for another, without regard to case.
//! Two files are alike when they are of one size and were changed at the
//! same time; when asked to, files of one size are read instead and are
//! alike when they hold the same bytes. Each scan runs on its own thread
//! and reads on the threads that search file contents.
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::{MAIN_SEPARATOR, MAIN_SEPARATOR_STR, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant, UNIX_EPOCH};

use crux_core::Request;
use crux_core::effects::ResolveSink;
use dirfleet_core::app::{
    CompareFolders, CompareUpdate, Difference, Differences, ListDirOutput, Recompare, ScanProgress,
};
use dirfleet_core::listing::{EntryFlags, EntryType, Listing, ListingBuilder};
use rayon::prelude::*;

use crate::access_time::PreserveAccessTime;
use crate::list::display_order_with;
use crate::open_history::OpenHistory;
use crate::paths::{canonical_string, normalize_path};
use crate::searching::scan_pool;
use crate::walk::{WalkOptions, walk};

/// Files are read in pieces of this size, so progress and stopping stay live.
const CHUNK: usize = 256 * 1024;
/// How often a scan says how far it is.
const REPORT_INTERVAL: Duration = Duration::from_millis(200);
/// Deep enough for any folder: the walk stops where the folders do.
const EVERY_LEVEL: usize = usize::MAX / 2;
/// Whether the disks here usually take one spelling of a name for another.
const FOLDS_NAMES: bool = cfg!(any(windows, target_os = "macos"));

/// What differs between two folders.
pub struct Compared {
    /// One file for each path that differs, by path: the left folder's
    /// where it is the only or the newer one, or as old as the other; the
    /// right folder's otherwise.
    pub listing: Listing,
    /// How each entry of `listing` differs.
    pub kinds: Vec<Difference>,
    /// For each folder of `listing`, the folder at the same place on the
    /// other side, and whether the folder itself is on the left.
    pub other_dirs: Vec<String>,
    pub on_left: Vec<bool>,
    /// How many files are in both folders and alike.
    pub alike: u64,
    /// How many files were to be read and could not be.
    pub unread: u32,
}

/// One folder's files, by their path below it.
struct Side {
    listing: Arc<Listing>,
    /// The folder as the walk writes it.
    root: String,
    /// Each file's path below the folder, with `/` between its parts, and
    /// its entry in `listing`.
    files: Vec<(String, u32)>,
}

/// `dir`'s path below `root`, with `/` between its parts; "" for `root`
/// itself. `None` when it is not below it.
fn below(root: &str, dir: &str) -> Option<String> {
    let rest = dir.strip_prefix(root)?;
    if !(rest.is_empty() || rest.starts_with(MAIN_SEPARATOR) || root.ends_with(MAIN_SEPARATOR)) {
        return None;
    }
    Some(
        rest.trim_start_matches(MAIN_SEPARATOR)
            .replace(MAIN_SEPARATOR, "/"),
    )
}

/// The folder `under` of `root`, as rows write folders.
fn folder_at(root: &str, under: &str) -> String {
    if under.is_empty() {
        return root.to_owned();
    }
    Path::new(root)
        .join(under.replace('/', MAIN_SEPARATOR_STR))
        .to_string_lossy()
        .into_owned()
}

impl Side {
    fn read(
        root: &Path,
        show_hidden: bool,
        cancel: &AtomicBool,
        on_count: &(dyn Fn(usize) + Sync),
    ) -> Self {
        let options = WalkOptions {
            skip_links: true,
            on_count: Some(on_count),
            ..WalkOptions::default()
        };
        let walked = walk(
            &[root.to_path_buf()],
            EVERY_LEVEL,
            show_hidden,
            cancel,
            options,
            &|_| {},
        );
        let listing = walked.listing;
        let root = canonical_string(&normalize_path(root));
        // Each folder's path below the root, worked out once.
        let under: Vec<Option<String>> =
            listing.dirs().iter().map(|dir| below(&root, dir)).collect();
        let files = (0..listing.len())
            .filter(|&index| {
                let meta = listing.meta(index);
                meta.entry_type == EntryType::File && !meta.flags.contains(EntryFlags::SYMLINK)
            })
            .filter_map(|index| {
                let under = under.get(listing.dir_index(index))?.as_ref()?;
                let name = listing.name(index);
                let path = if under.is_empty() {
                    name.to_owned()
                } else {
                    format!("{under}/{name}")
                };
                Some((path, index as u32))
            })
            .collect();
        Self {
            listing,
            root,
            files,
        }
    }

    /// A file's size and when it was changed, in seconds.
    fn size_time(&self, entry: u32) -> (u64, u64) {
        let meta = self.listing.meta(entry as usize);
        (meta.size, u64::from(*meta.modified_at))
    }

    fn path(&self, entry: u32) -> PathBuf {
        self.listing.entry(entry as usize).get_path()
    }
}

/// A path written the way names are told apart where case does not count.
fn folded(path: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    path.nfc().flat_map(char::to_lowercase).collect()
}

/// Each path with the left folder's file and the right folder's, where
/// there is one.
fn pair<'a>(left: &'a Side, right: &'a Side) -> Vec<(&'a str, Option<u32>, Option<u32>)> {
    let exact: HashMap<&str, usize> = right
        .files
        .iter()
        .enumerate()
        .map(|(at, (path, _))| (path.as_str(), at))
        .collect();
    let mut taken = vec![false; right.files.len()];
    let mut pairs = Vec::with_capacity(left.files.len().max(right.files.len()));
    let mut alone = Vec::new();
    for (path, entry) in &left.files {
        match exact.get(path.as_str()) {
            Some(&at) => {
                taken[at] = true;
                pairs.push((path.as_str(), Some(*entry), Some(right.files[at].1)));
            }
            None => alone.push((path.as_str(), *entry)),
        }
    }
    // What has no namesake may have one written another way.
    let mut respelled: HashMap<String, usize> = HashMap::new();
    if FOLDS_NAMES && !alone.is_empty() {
        for (at, (path, _)) in right.files.iter().enumerate() {
            if !taken[at] {
                respelled.entry(folded(path)).or_insert(at);
            }
        }
    }
    for (path, entry) in alone {
        let other = respelled.remove(&folded(path)).map(|at| {
            taken[at] = true;
            right.files[at].1
        });
        pairs.push((path, Some(entry), other));
    }
    pairs.extend(
        right
            .files
            .iter()
            .zip(&taken)
            .filter(|&(_, &taken)| !taken)
            .map(|((path, entry), _)| (path.as_str(), None, Some(*entry))),
    );
    pairs
}

/// `FILE_ATTRIBUTE_OFFLINE`, `_RECALL_ON_OPEN` and `_RECALL_ON_DATA_ACCESS`.
#[cfg(windows)]
fn kept_online(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    path.symlink_metadata()
        .is_ok_and(|meta| meta.file_attributes() & crate::walk::ATTR_KEPT_ONLINE != 0)
}

#[cfg(not(windows))]
const fn kept_online(_: &Path) -> bool {
    false
}

/// Reads until `buffer` is full or the file ends; how much was read.
fn fill(file: &mut File, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

/// Whether the two files hold the same bytes, read without counting as
/// opening them; `on_read` hears of every piece compared. `None` when
/// `cancel` was set meanwhile.
///
/// # Errors
/// A file could not be read, or is kept online and would have to be
/// fetched first.
pub fn same_bytes(
    left: &Path,
    right: &Path,
    cancel: &AtomicBool,
    mut on_read: impl FnMut(u64),
) -> io::Result<Option<bool>> {
    if kept_online(left) || kept_online(right) {
        return Err(io::Error::other("kept online"));
    }
    let _left_accessed = PreserveAccessTime::new(left);
    let _right_accessed = PreserveAccessTime::new(right);
    let (mut a, mut b) = (File::open(left)?, File::open(right)?);
    let (mut one, mut other) = (vec![0; CHUNK], vec![0; CHUNK]);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let (read, also) = (fill(&mut a, &mut one)?, fill(&mut b, &mut other)?);
        if read != also || one[..read] != other[..read] {
            return Ok(Some(false));
        }
        if read == 0 {
            return Ok(Some(true));
        }
        on_read(read as u64);
    }
}

/// What differs between `left` and `right`, hidden files too when
/// `show_hidden`; files of one size are read when `contents`. `progress`
/// hears how far the scan is. `None` when `cancel` was set meanwhile.
pub fn compare(
    left: &Path,
    right: &Path,
    contents: bool,
    show_hidden: bool,
    cancel: &AtomicBool,
    progress: &(dyn Fn(ScanProgress) + Sync),
) -> Option<Compared> {
    let said = |found: u64| {
        progress(ScanProgress {
            found,
            ..ScanProgress::default()
        });
    };
    let left = Side::read(left, show_hidden, cancel, &|count| said(count as u64));
    let before = left.listing.len() as u64;
    let right = Side::read(right, show_hidden, cancel, &|count| {
        said(before + count as u64);
    });
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let found = before + right.listing.len() as u64;
    let pairs = pair(&left, &right);
    // The files to read: in both folders, of one size, holding something.
    let to_read: Vec<usize> = if contents {
        pairs
            .iter()
            .enumerate()
            .filter(|(_, pair)| match pair {
                (_, Some(a), Some(b)) => {
                    let (size, other) = (left.size_time(*a).0, right.size_time(*b).0);
                    size == other && size > 0
                }
                _ => false,
            })
            .map(|(at, _)| at)
            .collect()
    } else {
        Vec::new()
    };
    let size_of = |at: usize| pairs[at].1.map_or(0, |entry| left.size_time(entry).0);
    let to_compare: u64 = to_read.iter().map(|&at| size_of(at)).sum();
    let compared = AtomicU64::new(0);
    let unread = AtomicU32::new(0);
    let last_report = Mutex::new(Instant::now());
    let report = |now: bool| {
        if !now {
            let Ok(mut last) = last_report.try_lock() else {
                return;
            };
            if last.elapsed() < REPORT_INTERVAL {
                return;
            }
            *last = Instant::now();
        }
        progress(ScanProgress {
            found,
            candidates: to_read.len() as u64,
            compared: compared.load(Ordering::Relaxed),
            to_compare,
        });
    };
    if !to_read.is_empty() {
        report(true);
    }
    let read: HashMap<usize, Option<bool>> = to_read
        .par_iter()
        .map(|&at| {
            let (_, Some(a), Some(b)) = pairs[at] else {
                return (at, None);
            };
            let mut done = 0;
            let alike = same_bytes(&left.path(a), &right.path(b), cancel, |read| {
                done += read;
                compared.fetch_add(read, Ordering::Relaxed);
                report(false);
            })
            .unwrap_or_else(|_| {
                unread.fetch_add(1, Ordering::Relaxed);
                None
            });
            // What is not read to its end counts as gone through.
            compared.fetch_add(size_of(at).saturating_sub(done), Ordering::Relaxed);
            (at, alike)
        })
        .collect();
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let mut found = differing(&left, &right, &pairs, &read, contents);
    found.unread = unread.load(Ordering::Relaxed);
    Some(found)
}

/// What differs among `pairs`, as a listing. `read` says whether the files
/// that were read hold the same bytes, by their pair's place.
fn differing(
    left: &Side,
    right: &Side,
    pairs: &[(&str, Option<u32>, Option<u32>)],
    read: &HashMap<usize, Option<bool>>,
    contents: bool,
) -> Compared {
    let mut differing: Vec<(&str, Difference, bool, u32)> = Vec::new();
    let mut alike = 0_u64;
    for (at, &(path, a, b)) in pairs.iter().enumerate() {
        let kind = match (a, b) {
            (Some(a), Some(b)) => Difference::of(
                left.size_time(a),
                right.size_time(b),
                // Files of no size hold the same nothing.
                read.get(&at)
                    .copied()
                    .flatten()
                    .or_else(|| (contents && left.size_time(a).0 == 0).then_some(true)),
            ),
            (Some(_), None) => Difference::OnlyLeft,
            (None, Some(_)) => Difference::OnlyRight,
            (None, None) => Difference::Same,
        };
        // The newer file of the two is the one shown.
        let shown = match kind {
            Difference::Same => {
                alike += 1;
                continue;
            }
            Difference::OnlyRight | Difference::NewerRight => (false, b),
            Difference::OnlyLeft | Difference::NewerLeft | Difference::Different => (true, a),
        };
        if let (on_left, Some(entry)) = shown {
            differing.push((path, kind, on_left, entry));
        }
    }
    differing.sort_by(|a, b| a.0.cmp(b.0));
    let mut builder = ListingBuilder::with_capacity(differing.len());
    for &(_, _, on_left, entry) in &differing {
        let from = if on_left {
            &left.listing
        } else {
            &right.listing
        };
        let entry = entry as usize;
        builder.push(from.dir(entry), from.name(entry), *from.meta(entry));
    }
    let listing = builder.finish();
    // A folder of the listing is on the side its files are on.
    let mut on_left = vec![true; listing.dirs().len()];
    for (entry, &(_, _, left, _)) in differing.iter().enumerate() {
        on_left[listing.dir_index(entry)] = left;
    }
    let other_dirs = listing
        .dirs()
        .iter()
        .zip(&on_left)
        .map(|(dir, &on_left)| {
            let (this, other) = if on_left {
                (&left.root, &right.root)
            } else {
                (&right.root, &left.root)
            };
            folder_at(other, &below(this, dir).unwrap_or_default())
        })
        .collect();
    Compared {
        kinds: differing.iter().map(|&(_, kind, ..)| kind).collect(),
        listing,
        other_dirs,
        on_left,
        alike,
        unread: 0,
    }
}

/// How the file at each left path differs from the one at its right path,
/// as things are now.
pub fn recompare(request: &Recompare) -> Vec<Difference> {
    let size_time = |path: &str| {
        let meta = Path::new(path).symlink_metadata().ok()?;
        let modified = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_secs());
        meta.is_file().then_some((meta.len(), modified))
    };
    let never = AtomicBool::new(false);
    request
        .pairs
        .iter()
        .map(|(left, right)| match (size_time(left), size_time(right)) {
            (None, None) => Difference::Same,
            (Some(_), None) => Difference::OnlyLeft,
            (None, Some(_)) => Difference::OnlyRight,
            (Some(a), Some(b)) => {
                let alike = if request.contents && a.0 == b.0 {
                    same_bytes(Path::new(left), Path::new(right), &never, |_| {})
                        .ok()
                        .flatten()
                } else {
                    None
                };
                Difference::of(a, b, alike)
            }
        })
        .collect()
}

type Start = dyn Fn(Request<CompareFolders>, Arc<AtomicBool>) + Send + Sync;

/// Runs comparisons until they finish or are stopped.
pub struct CompareHandler {
    running: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    start: Box<Start>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl CompareHandler {
    pub fn new<R>(sink: Weak<R>, history: Arc<OpenHistory>) -> Self
    where
        R: ResolveSink<CompareFolders> + Send + Sync + 'static,
    {
        let running: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>> = Arc::default();
        let finished = Arc::downgrade(&running);
        Self {
            running,
            start: Box::new(move |request, cancel| {
                spawn(
                    request,
                    cancel,
                    sink.clone(),
                    finished.clone(),
                    Arc::clone(&history),
                );
            }),
        }
    }

    pub fn start(&self, request: Request<CompareFolders>) {
        let cancel = Arc::new(AtomicBool::new(false));
        lock(&self.running).insert(request.operation.id, Arc::clone(&cancel));
        (self.start)(request, cancel);
    }

    pub fn stop(&self, id: u64) {
        let cancel = lock(&self.running).remove(&id);
        if let Some(cancel) = cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// How many comparisons are running.
    pub fn count(&self) -> usize {
        lock(&self.running).len()
    }
}

fn spawn<R>(
    mut request: Request<CompareFolders>,
    cancel: Arc<AtomicBool>,
    sink: Weak<R>,
    running: Weak<Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    history: Arc<OpenHistory>,
) where
    R: ResolveSink<CompareFolders> + Send + Sync + 'static,
{
    let id = request.operation.id;
    let spawned = std::thread::Builder::new()
        .name(format!("dirfleet_compare_{id}"))
        .spawn(move || {
            let operation = request.operation.clone();
            // Progress goes out from the scan's threads, one at a time.
            let request = Mutex::new(&mut request);
            let resolve = |update: CompareUpdate| {
                sink.upgrade()
                    .is_some_and(|sink| sink.resolve_request(&mut **lock(&request), update).is_ok())
            };
            let progress = |progress: ScanProgress| {
                if !cancel.load(Ordering::Relaxed) && !resolve(CompareUpdate::Progress(progress)) {
                    cancel.store(true, Ordering::Relaxed);
                }
            };
            let run = || {
                compare(
                    Path::new(&operation.left),
                    Path::new(&operation.right),
                    operation.contents,
                    operation.view.show_hidden,
                    &cancel,
                    &progress,
                )
            };
            let found = scan_pool().map_or_else(run, |pool| pool.install(run));
            if let Some(found) = found.filter(|_| !cancel.load(Ordering::Relaxed)) {
                let listing = Arc::new(found.listing);
                resolve(CompareUpdate::Found(Differences {
                    output: ListDirOutput::Listed {
                        order: display_order_with(&listing, &operation.view, history.as_ref()),
                        opened: Arc::new(history.keys_if_any(&listing)),
                        listing,
                        view: operation.view.clone(),
                    },
                    kinds: found.kinds,
                    other_dirs: Arc::new(found.other_dirs),
                    on_left: Arc::new(found.on_left),
                    alike: found.alike,
                    unread: found.unread,
                }));
            }
            if let Some(running) = running.upgrade() {
                lock(&running).remove(&id);
            }
        });
    if let Err(error) = spawned {
        log::warn!("could not start comparing folders: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn write(root: &Path, file: &str, bytes: &[u8], modified: u64) {
        let path = root.join(file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("folders");
        }
        std::fs::write(&path, bytes).expect("write");
        let file = File::options().write(true).open(&path).expect("open");
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(modified))
            .expect("time");
    }

    fn compared(left: &Path, right: &Path, contents: bool) -> Compared {
        let cancel = AtomicBool::new(false);
        compare(left, right, contents, false, &cancel, &|_| {}).expect("not stopped")
    }

    /// Each difference as its kind, the side of the file shown, and the
    /// file's path below its folder.
    fn found(compared: &Compared, left: &Path, right: &Path) -> Vec<(Difference, char, String)> {
        let (left, right) = (
            canonical_string(&normalize_path(left)),
            canonical_string(&normalize_path(right)),
        );
        (0..compared.listing.len())
            .map(|entry| {
                let dir = compared.listing.dir(entry);
                let at = compared.listing.dir_index(entry);
                let (side, root) = if compared.on_left[at] {
                    ('L', &left)
                } else {
                    ('R', &right)
                };
                let under = below(root, dir).expect("below its folder");
                let name = compared.listing.name(entry);
                (
                    compared.kinds[entry],
                    side,
                    if under.is_empty() {
                        name.to_owned()
                    } else {
                        format!("{under}/{name}")
                    },
                )
            })
            .collect()
    }

    #[test]
    fn files_in_one_folder_only_or_not_alike_are_what_differs() {
        use Difference::{Different, NewerLeft, NewerRight, OnlyLeft, OnlyRight};
        let (a, b) = (
            tempfile::tempdir().expect("temp dir"),
            tempfile::tempdir().expect("temp dir"),
        );
        let (left, right) = (a.path(), b.path());
        write(left, "same.txt", b"same", 1000);
        write(right, "same.txt", b"same", 1001);
        write(left, "deep/down/same too.txt", b"x", 1000);
        write(right, "deep/down/same too.txt", b"x", 1000);
        write(left, "only here.txt", b"left", 1000);
        write(right, "deep/only there.txt", b"right", 1000);
        write(left, "deep/newer here.txt", b"new words", 5000);
        write(right, "deep/newer here.txt", b"old", 1000);
        write(left, "newer there.txt", b"old", 1000);
        write(right, "newer there.txt", b"new words", 5000);
        write(left, "odd.txt", b"one", 1000);
        write(right, "odd.txt", b"other", 1000);
        // A folder with nothing in it is no file to compare.
        std::fs::create_dir_all(left.join("empty")).expect("folder");

        let compared = compared(left, right, false);
        assert_eq!(
            found(&compared, left, right),
            [
                (NewerLeft, 'L', "deep/newer here.txt".to_owned()),
                (OnlyRight, 'R', "deep/only there.txt".to_owned()),
                (NewerRight, 'R', "newer there.txt".to_owned()),
                (Different, 'L', "odd.txt".to_owned()),
                (OnlyLeft, 'L', "only here.txt".to_owned()),
            ]
        );
        assert_eq!((compared.alike, compared.unread), (2, 0));
        // Each folder of the listing knows its place on the other side.
        for (entry, (_, side, path)) in found(&compared, left, right).into_iter().enumerate() {
            let other = if side == 'L' { right } else { left };
            let name = compared.listing.name(entry);
            let there = Path::new(&compared.other_dirs[compared.listing.dir_index(entry)]);
            assert_eq!(
                normalize_path(&there.join(name)),
                normalize_path(&other.join(&path)),
                "{path}"
            );
        }
    }

    #[test]
    fn files_of_one_size_are_read_when_contents_count() {
        use Difference::{NewerLeft, NewerRight};
        let (a, b) = (
            tempfile::tempdir().expect("temp dir"),
            tempfile::tempdir().expect("temp dir"),
        );
        let (left, right) = (a.path(), b.path());
        // The same bytes, saved at different times.
        write(left, "copied later.txt", b"the same words", 9000);
        write(right, "copied later.txt", b"the same words", 1000);
        // Other bytes of the same length, saved at the same time.
        write(left, "edited.txt", b"abcd", 1000);
        write(right, "edited.txt", b"abXd", 1000);
        write(left, "nothing.txt", b"", 9000);
        write(right, "nothing.txt", b"", 1000);

        let by_time = compared(left, right, false);
        assert_eq!(
            found(&by_time, left, right),
            [
                (NewerLeft, 'L', "copied later.txt".to_owned()),
                (NewerLeft, 'L', "nothing.txt".to_owned()),
            ],
            "by size and time, the edited file passes for the same"
        );
        let by_bytes = compared(left, right, true);
        assert_eq!(
            found(&by_bytes, left, right),
            [(Difference::Different, 'L', "edited.txt".to_owned())]
        );
        assert_eq!(by_bytes.alike, 2);
        // Larger than one piece, differing in the last byte.
        let large: Vec<u8> = (0..CHUNK as u32 + 10).map(|i| (i % 251) as u8).collect();
        let mut other = large.clone();
        *other.last_mut().expect("bytes") ^= 1;
        write(left, "big.bin", &large, 1000);
        write(right, "big.bin", &other, 4000);
        assert_eq!(
            found(&compared(left, right, true), left, right)[0],
            (NewerRight, 'R', "big.bin".to_owned())
        );
    }

    #[test]
    fn names_written_in_another_case_are_one_file_where_disks_say_so() {
        let (a, b) = (
            tempfile::tempdir().expect("temp dir"),
            tempfile::tempdir().expect("temp dir"),
        );
        write(a.path(), "Notes/Read Me.txt", b"hello", 1000);
        write(b.path(), "notes/read me.TXT", b"hello", 1000);
        let differing = compared(a.path(), b.path(), false).listing.len();
        assert_eq!(differing, if FOLDS_NAMES { 0 } else { 2 });
    }

    #[test]
    fn a_stopped_comparison_finds_nothing_and_a_running_one_says_how_far_it_is() {
        let (a, b) = (
            tempfile::tempdir().expect("temp dir"),
            tempfile::tempdir().expect("temp dir"),
        );
        write(a.path(), "a.txt", b"twice", 1000);
        write(b.path(), "a.txt", b"twice", 2000);
        let stopped = AtomicBool::new(true);
        assert!(compare(a.path(), b.path(), true, false, &stopped, &|_| {}).is_none());

        let said = Mutex::new(Vec::new());
        let cancel = AtomicBool::new(false);
        let found = compare(a.path(), b.path(), true, false, &cancel, &|progress| {
            lock(&said).push(progress);
        });
        assert!(found.is_some());
        let said = lock(&said).clone();
        assert!(
            said.iter().any(|progress| progress.found == 2
                && progress.candidates == 1
                && progress.to_compare == 5),
            "{said:?}"
        );
    }

    #[test]
    fn paths_are_compared_again_as_they_are_now() {
        use Difference::{NewerRight, OnlyLeft, OnlyRight, Same};
        let (a, b) = (
            tempfile::tempdir().expect("temp dir"),
            tempfile::tempdir().expect("temp dir"),
        );
        write(a.path(), "both.txt", b"abcd", 1000);
        write(b.path(), "both.txt", b"abcd", 1000);
        write(a.path(), "left.txt", b"x", 1000);
        write(b.path(), "right.txt", b"x", 1000);
        write(a.path(), "touched.txt", b"same", 1000);
        write(b.path(), "touched.txt", b"same", 7000);
        let pairs = ["both.txt", "left.txt", "right.txt", "touched.txt", "none"]
            .iter()
            .map(|name| {
                let text = |root: &Path| root.join(name).to_string_lossy().into_owned();
                (text(a.path()), text(b.path()))
            })
            .collect::<Vec<_>>();
        let again = |contents| {
            recompare(&Recompare {
                pairs: pairs.clone(),
                contents,
            })
        };
        assert_eq!(again(false), [Same, OnlyLeft, OnlyRight, NewerRight, Same]);
        // Read, the file saved again without a change is the same.
        assert_eq!(again(true), [Same, OnlyLeft, OnlyRight, Same, Same]);
    }
}
