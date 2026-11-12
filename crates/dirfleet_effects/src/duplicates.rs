//! Finds the files under some folders that hold the same bytes.
//!
//! The folders are walked without following links. Files of a size no other
//! file has cannot be copies and are dropped unread; so are further names of
//! a file already counted (hard links). What is left is told apart first by
//! its opening bytes, then by a BLAKE3 of the whole file, so most files are
//! never read to their end. Each scan runs on its own thread and reads on
//! the threads that search file contents.
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use crux_core::Request;
use crux_core::effects::ResolveSink;
use dirfleet_core::app::{
    DuplicateSets, DuplicatesUpdate, FindDuplicates, ListDirOutput, ScanProgress, StillThere,
};
use dirfleet_core::listing::{EntryFlags, EntryType, Listing, ListingBuilder};
use rayon::prelude::*;

use crate::access_time::PreserveAccessTime;
use crate::list::display_order_with;
use crate::open_history::OpenHistory;
use crate::searching::scan_pool;
use crate::walk::{WalkOptions, walk};

/// Files are first told apart by this many of their opening bytes.
const HEAD: u64 = 64 * 1024;
/// Files are read in pieces of this size, so progress and stopping stay live.
const CHUNK: u64 = 1024 * 1024;
/// How often a scan says how far it is.
const REPORT_INTERVAL: Duration = Duration::from_millis(200);
/// Deep enough for any folder: the walk stops where the folders do.
const EVERY_LEVEL: usize = usize::MAX / 2;

/// The files that hold the same bytes as another, set by set.
pub struct Sets {
    /// The files, each set's together: the set that wastes the most space
    /// first, and a set's files by their paths.
    pub listing: Listing,
    /// The set each entry of `listing` is in.
    pub set_of: Vec<u32>,
    /// The size of one file of each set.
    pub sizes: Vec<u64>,
    /// How many files could not be read, and so were left out.
    pub unread: u32,
    /// How many files were looked at.
    pub files: u64,
}

/// One scan's reading of the files a walk found.
struct Scan<'a> {
    listing: &'a Listing,
    cancel: &'a AtomicBool,
    progress: &'a (dyn Fn(ScanProgress) + Sync),
    /// What the walk found, and how many of its files may be copies.
    found: u64,
    candidates: u64,
    compared: AtomicU64,
    to_compare: AtomicU64,
    unread: AtomicU32,
    last_report: Mutex<Instant>,
}

impl Scan<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn size(&self, entry: u32) -> u64 {
        self.listing.meta(entry as usize).size
    }

    fn path(&self, entry: u32) -> PathBuf {
        self.listing.entry(entry as usize).get_path()
    }

    /// Says how far the scan is, unless it just did.
    fn report(&self, now: bool) {
        if !now {
            let Ok(mut last) = self.last_report.try_lock() else {
                return;
            };
            if last.elapsed() < REPORT_INTERVAL {
                return;
            }
            *last = Instant::now();
        }
        (self.progress)(ScanProgress {
            found: self.found,
            candidates: self.candidates,
            compared: self.compared.load(Ordering::Relaxed),
            to_compare: self.to_compare.load(Ordering::Relaxed),
        });
    }

    /// The BLAKE3 of the first `limit` bytes of `entry`'s file, read without
    /// counting as opening it. `None` when the scan was stopped, or the file
    /// could not be read: it is then counted as unread.
    fn digest(&self, entry: u32, limit: u64) -> Option<blake3::Hash> {
        let path = self.path(entry);
        let read = || -> io::Result<Option<blake3::Hash>> {
            let _accessed = PreserveAccessTime::new(&path);
            let mut file = File::open(&path)?.take(limit);
            let mut hasher = blake3::Hasher::new();
            let piece = limit.min(CHUNK).min(self.size(entry).max(1));
            let piece = usize::try_from(piece).unwrap_or(usize::MAX);
            let mut buffer = vec![0; piece];
            loop {
                if self.cancelled() {
                    return Ok(None);
                }
                let read = match file.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                };
                hasher.update(&buffer[..read]);
                if limit > HEAD {
                    self.compared.fetch_add(read as u64, Ordering::Relaxed);
                    self.report(false);
                }
            }
            Ok(Some(hasher.finalize()))
        };
        read().unwrap_or_else(|_| {
            self.unread.fetch_add(1, Ordering::Relaxed);
            None
        })
    }

    /// Splits each of `groups` by the first `limit` bytes of its files,
    /// keeping what still has two files or more.
    fn split(&self, groups: &[Vec<u32>], limit: u64) -> Vec<Vec<u32>> {
        let files: Vec<(usize, u32)> = groups
            .iter()
            .enumerate()
            .flat_map(|(group, files)| files.iter().map(move |&entry| (group, entry)))
            .collect();
        let read: Vec<(usize, u32, Option<blake3::Hash>)> = files
            .par_iter()
            .map(|&(group, entry)| (group, entry, self.digest(entry, limit)))
            .collect();
        let mut split: HashMap<(usize, [u8; 32]), Vec<u32>> = HashMap::new();
        for (group, entry, hash) in read {
            if let Some(hash) = hash {
                split
                    .entry((group, *hash.as_bytes()))
                    .or_default()
                    .push(entry);
            }
        }
        split
            .into_values()
            .filter(|files| files.len() >= 2)
            .collect()
    }
}

/// What a file's id is counted within: its drive, where paths name one.
fn volume_of(dir: &str) -> &str {
    dir.split(['\\', '/']).next().unwrap_or_default()
}

/// The files of `listing` that may be copies of another, by size: files that
/// hold something, are no links, and share their size with another file.
/// Of a file with several names (hard links) only the first is kept.
fn same_sized(listing: &Listing) -> Vec<Vec<u32>> {
    let mut by_size: HashMap<u64, Vec<u32>> = HashMap::new();
    for index in 0..listing.len() {
        let meta = listing.meta(index);
        if meta.entry_type == EntryType::File
            && meta.size > 0
            && !meta.flags.contains(EntryFlags::SYMLINK)
        {
            by_size.entry(meta.size).or_default().push(index as u32);
        }
    }
    by_size
        .into_values()
        .filter_map(|files| {
            let mut seen: HashSet<(&str, u64)> = HashSet::new();
            let files: Vec<u32> = files
                .into_iter()
                .filter(|&entry| {
                    let id = listing.meta(entry as usize).file_id;
                    id == 0 || seen.insert((volume_of(listing.dir(entry as usize)), id))
                })
                .collect();
            (files.len() >= 2).then_some(files)
        })
        .collect()
}

/// The files under `roots` that hold the same bytes as another, hidden ones
/// too when `show_hidden`. `progress` hears how far the scan is. `None`
/// when `cancel` was set meanwhile.
pub fn find(
    roots: &[PathBuf],
    show_hidden: bool,
    cancel: &AtomicBool,
    progress: &(dyn Fn(ScanProgress) + Sync),
) -> Option<Sets> {
    let on_count = |count: usize| {
        progress(ScanProgress {
            found: count as u64,
            ..ScanProgress::default()
        });
    };
    let options = WalkOptions {
        for_reading: true,
        on_count: Some(&on_count),
        ..WalkOptions::default()
    };
    let walked = walk(roots, EVERY_LEVEL, show_hidden, cancel, options, &|_| {});
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let listing = &*walked.listing;
    let files = (0..listing.len())
        .filter(|&index| listing.meta(index).entry_type == EntryType::File)
        .count() as u64;
    let groups = same_sized(listing);
    let scan = Scan {
        listing,
        cancel,
        progress,
        found: listing.len() as u64,
        candidates: groups.iter().map(|files| files.len() as u64).sum(),
        compared: AtomicU64::new(0),
        to_compare: AtomicU64::new(0),
        unread: AtomicU32::new(0),
        last_report: Mutex::new(Instant::now()),
    };
    scan.report(true);
    // Opening bytes first: files that differ there are never read whole.
    let (small, large): (Vec<_>, Vec<_>) = scan
        .split(&groups, HEAD)
        .into_iter()
        .partition(|files| files.first().is_some_and(|&entry| scan.size(entry) <= HEAD));
    let to_compare = large
        .iter()
        .flatten()
        .map(|&entry| scan.size(entry))
        .sum::<u64>();
    scan.to_compare.store(to_compare, Ordering::Relaxed);
    scan.report(true);
    // A small file's opening bytes are all of it.
    let mut sets = small;
    sets.extend(scan.split(&large, u64::MAX));
    if scan.cancelled() {
        return None;
    }
    let path = |entry: u32| listing.entry(entry as usize).full_path_string();
    for files in &mut sets {
        files.sort_by_cached_key(|&entry| path(entry));
    }
    // What deleting all but one of a set's files would free.
    let wasted = |files: &Vec<u32>| {
        files
            .first()
            .map_or(0, |&entry| scan.size(entry))
            .saturating_mul(files.len() as u64 - 1)
    };
    sets.sort_by_cached_key(|files| {
        (
            std::cmp::Reverse(wasted(files)),
            files.first().map(|&entry| path(entry)),
        )
    });
    let mut builder = ListingBuilder::with_capacity(sets.iter().map(Vec::len).sum());
    let mut set_of = Vec::new();
    let mut sizes = Vec::with_capacity(sets.len());
    for (set, files) in sets.iter().enumerate() {
        sizes.push(files.first().map_or(0, |&entry| scan.size(entry)));
        for &entry in files {
            let entry = entry as usize;
            builder.push(
                listing.dir(entry),
                listing.name(entry),
                *listing.meta(entry),
            );
            set_of.push(set as u32);
        }
    }
    Some(Sets {
        listing: builder.finish(),
        set_of,
        sizes,
        unread: scan.unread.load(Ordering::Relaxed),
        files,
    })
}

/// Whether each of the paths is still there.
pub fn still_there(request: &StillThere) -> Vec<bool> {
    request
        .paths
        .iter()
        .map(|path| Path::new(path).symlink_metadata().is_ok())
        .collect()
}

type Start = dyn Fn(Request<FindDuplicates>, Arc<AtomicBool>) + Send + Sync;

/// Runs scans for duplicates until they finish or are stopped.
pub struct DuplicateHandler {
    running: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    start: Box<Start>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl DuplicateHandler {
    pub fn new<R>(sink: Weak<R>, history: Arc<OpenHistory>) -> Self
    where
        R: ResolveSink<FindDuplicates> + Send + Sync + 'static,
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

    pub fn start(&self, request: Request<FindDuplicates>) {
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

    /// How many scans are running.
    pub fn count(&self) -> usize {
        lock(&self.running).len()
    }
}

fn spawn<R>(
    mut request: Request<FindDuplicates>,
    cancel: Arc<AtomicBool>,
    sink: Weak<R>,
    running: Weak<Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    history: Arc<OpenHistory>,
) where
    R: ResolveSink<FindDuplicates> + Send + Sync + 'static,
{
    let id = request.operation.id;
    let spawned = std::thread::Builder::new()
        .name(format!("dirfleet_duplicates_{id}"))
        .spawn(move || {
            let operation = request.operation.clone();
            let roots: Vec<PathBuf> = operation.roots.iter().map(PathBuf::from).collect();
            // Progress goes out from the scan's threads, one at a time.
            let request = Mutex::new(&mut request);
            let resolve = |update: DuplicatesUpdate| {
                sink.upgrade()
                    .is_some_and(|sink| sink.resolve_request(&mut **lock(&request), update).is_ok())
            };
            let progress = |progress: ScanProgress| {
                if !cancel.load(Ordering::Relaxed) && !resolve(DuplicatesUpdate::Progress(progress))
                {
                    cancel.store(true, Ordering::Relaxed);
                }
            };
            let run = || find(&roots, operation.view.show_hidden, &cancel, &progress);
            let found = scan_pool().map_or_else(run, |pool| pool.install(run));
            if let Some(sets) = found.filter(|_| !cancel.load(Ordering::Relaxed)) {
                let listing = Arc::new(sets.listing);
                resolve(DuplicatesUpdate::Found(DuplicateSets {
                    output: ListDirOutput::Listed {
                        order: display_order_with(&listing, &operation.view, history.as_ref()),
                        opened: Arc::new(history.keys_if_any(&listing)),
                        listing,
                        view: operation.view.clone(),
                    },
                    set_of: Arc::new(sets.set_of),
                    sizes: Arc::new(sets.sizes),
                    unread: sets.unread,
                    files: sets.files,
                }));
            }
            if let Some(running) = running.upgrade() {
                lock(&running).remove(&id);
            }
        });
    if let Err(error) = spawned {
        log::warn!("could not start looking for duplicates: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(root: &Path, show_hidden: bool) -> Sets {
        let cancel = AtomicBool::new(false);
        find(&[root.to_path_buf()], show_hidden, &cancel, &|_| {}).expect("not stopped")
    }

    /// Each set's size and the names of its files, as the listing has them.
    fn sets(found: &Sets) -> Vec<(u64, Vec<String>)> {
        let mut sets: Vec<(u64, Vec<String>)> =
            found.sizes.iter().map(|&size| (size, Vec::new())).collect();
        for (entry, &set) in found.set_of.iter().enumerate() {
            sets[set as usize]
                .1
                .push(found.listing.name(entry).to_owned());
        }
        sets
    }

    fn write(root: &Path, file: &str, bytes: &[u8]) {
        let path = root.join(file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("folders");
        }
        std::fs::write(path, bytes).expect("write");
    }

    #[test]
    fn files_holding_the_same_bytes_are_found_whatever_they_are_called() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        // Larger than the opening bytes, so they are read whole.
        let large: Vec<u8> = (0..200_000_u32).map(|i| (i % 251) as u8).collect();
        let mut other_end = large.clone();
        *other_end.last_mut().expect("bytes") ^= 1;
        let mut other_start = large.clone();
        other_start[0] ^= 1;
        write(root, "a/film.bin", &large);
        write(root, "b/deep/copy of film.bin", &large);
        write(root, "c/same size, other end.bin", &other_end);
        write(root, "c/same size, other start.bin", &other_start);
        write(root, "notes.txt", b"hello");
        write(root, "a/notes again.txt", b"hello");
        write(root, "b/notes thrice.txt", b"hello");
        write(root, "five.txt", b"HELLO");
        write(root, "alone.txt", b"no other file is this long");
        write(root, "empty one", b"");
        write(root, "empty two", b"");

        let found = found(root, false);
        // The set that wastes the most comes first; empty files are no set.
        assert_eq!(
            sets(&found),
            [
                (
                    200_000,
                    vec!["film.bin".to_owned(), "copy of film.bin".to_owned()]
                ),
                (
                    5,
                    vec![
                        "notes again.txt".to_owned(),
                        "notes thrice.txt".to_owned(),
                        "notes.txt".to_owned()
                    ]
                ),
            ]
        );
        assert_eq!(found.set_of, [0, 0, 1, 1, 1]);
        assert_eq!((found.unread, found.files), (0, 11));
        // Entries know their folders, so their paths can be acted on.
        let first = found.listing.entry(0).get_path();
        assert!(first.ends_with("a/film.bin"), "{}", first.display());
        assert!(first.is_file());
    }

    #[test]
    fn hidden_files_count_only_when_they_are_shown() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "seen.txt", b"the same words");
        write(dir.path(), ".hidden.txt", b"the same words");
        assert_eq!(found(dir.path(), false).set_of, Vec::<u32>::new());
        assert_eq!(found(dir.path(), true).set_of, [0, 0]);
    }

    #[test]
    fn a_file_with_two_names_is_not_a_copy_of_itself() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "one.txt", b"one file, two names");
        if std::fs::hard_link(dir.path().join("one.txt"), dir.path().join("two.txt")).is_err() {
            return;
        }
        // Where the file system gives files no ids, the names can't be told
        // to be one file.
        let mut ids = Vec::new();
        let _ = crate::fs_enum::enumerate(dir.path(), &mut |entry| ids.push(entry.file_id));
        if ids.contains(&0) || ids.is_empty() {
            return;
        }
        assert_eq!(found(dir.path(), false).set_of, Vec::<u32>::new());
        // A real copy is still found, once.
        write(dir.path(), "copy.txt", b"one file, two names");
        assert_eq!(found(dir.path(), false).set_of, [0, 0]);
    }

    #[test]
    fn a_stopped_scan_finds_nothing_and_says_how_far_a_running_one_is() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "a.txt", b"twice");
        write(dir.path(), "b.txt", b"twice");
        let stopped = AtomicBool::new(true);
        assert!(find(&[dir.path().to_path_buf()], false, &stopped, &|_| {}).is_none());

        let said = Mutex::new(Vec::new());
        let cancel = AtomicBool::new(false);
        let found = find(&[dir.path().to_path_buf()], false, &cancel, &|progress| {
            lock(&said).push(progress);
        });
        assert!(found.is_some());
        let said = lock(&said).clone();
        assert!(
            said.iter()
                .any(|progress| progress.found == 2 && progress.candidates == 2),
            "{said:?}"
        );
    }

    #[test]
    fn paths_are_checked_for_still_being_there() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "here.txt", b"x");
        let text = |name: &str| dir.path().join(name).to_string_lossy().into_owned();
        let request = StillThere {
            paths: vec![text("here.txt"), text("gone.txt")],
        };
        assert_eq!(still_there(&request), [true, false]);
    }
}
