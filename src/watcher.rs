use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use notify::{
    Event, EventKind, RecursiveMode, Watcher,
    event::{CreateKind, ModifyKind, RemoveKind, RenameMode},
};

use crate::data::files::{DirEntryMetaData, EntryType, ListingChange};
use crate::helper::{canonical_form, normalize_path};
use crate::toast;

/// Context used by watcher threads to wake the UI when events arrive, so the
/// app does not need to poll on a timer while directories are watched.
static REPAINT_CTX: LazyLock<Mutex<Option<egui::Context>>> = LazyLock::new(|| Mutex::new(None));

pub fn set_repaint_context(ctx: &egui::Context) {
    if let Ok(mut repaint) = REPAINT_CTX.lock() {
        *repaint = Some(ctx.clone());
    }
}

pub fn request_repaint() {
    if let Ok(repaint) = REPAINT_CTX.lock()
        && let Some(ctx) = repaint.as_ref()
    {
        ctx.request_repaint();
    }
}

/// Live updates for every watched root through one shared OS watcher, one
/// event-processing thread and one control thread. Previously each root had
/// its own watcher plus two threads, so a favourites search with N roots cost
/// 3N threads.
#[derive(Default)]
pub struct DirectoryWatchers {
    roots: HashMap<PathBuf, usize>,
    shared: Option<SharedWatcher>,
}

impl std::fmt::Debug for DirectoryWatchers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirectoryWatchers")
            .field("roots", &self.roots)
            .finish_non_exhaustive()
    }
}

/// A surgical change to a single file. `structural_dirs` (the coarse
/// "re-read the whole folder" fallback) is kept separate for genuinely
/// ambiguous events.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FileChange {
    /// File content/metadata changed — refresh its metadata only.
    Metadata(PathBuf),
    /// A new file appeared — insert an entry for it.
    Created(PathBuf),
    /// A file disappeared — drop its entry.
    Removed(PathBuf),
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FileSystemChanges {
    pub file_changes: BTreeSet<FileChange>,
    pub structural_dirs: BTreeSet<PathBuf>,
}

impl FileSystemChanges {
    #[inline]
    fn extend(&mut self, other: Self) {
        self.file_changes.extend(other.file_changes);
        self.structural_dirs.extend(other.structural_dirs);
    }
}

/// What a change does to the row for its path, decided from the path's state
/// when the batch was resolved (on the watcher thread).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    /// The path was created (or replaced): insert or overwrite its row.
    Upsert(DirEntryMetaData),
    /// Only metadata changed: update the row if it is listed.
    Update(DirEntryMetaData),
    /// The path no longer exists.
    Remove,
}

/// A watcher change with its metadata already read, so consumers on the UI
/// thread never touch the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedChange {
    pub path: PathBuf,
    pub kind: ChangeKind,
}

impl ResolvedChange {
    /// Read the current state of `path`. `created` marks a create or rename
    /// target (the row may be new); otherwise only metadata changed.
    pub fn from_disk(path: PathBuf, created: bool) -> Self {
        let kind = match entry_metadata(&path) {
            Some(meta) if created => ChangeKind::Upsert(meta),
            Some(meta) => ChangeKind::Update(meta),
            None => ChangeKind::Remove,
        };
        Self { path, kind }
    }

    #[must_use]
    pub fn listing_change(&self) -> Option<ListingChange<'_>> {
        let name = self.path.file_name()?.to_str()?;
        Some(match self.kind {
            ChangeKind::Upsert(meta) => ListingChange::Upsert(name, meta),
            ChangeKind::Update(meta) => ListingChange::Update(name, meta),
            ChangeKind::Remove => ListingChange::Remove(name),
        })
    }
}

/// Metadata for a listing row, matching what a directory read reports: the
/// link itself on Windows (where the bulk reader sees reparse points), the
/// target elsewhere.
fn entry_metadata(path: &Path) -> Option<DirEntryMetaData> {
    #[cfg(windows)]
    let meta = std::fs::symlink_metadata(path);
    #[cfg(not(windows))]
    let meta = std::fs::metadata(path).or_else(|_| std::fs::symlink_metadata(path));
    let mut meta: DirEntryMetaData = meta.ok()?.into();
    if meta.entry_type == EntryType::Directory {
        meta.entry_type = EntryType::of_dir(path);
    }
    Some(meta)
}

/// A delivered watcher batch.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ChangeBatch {
    /// Surgical changes in the order they must be applied.
    pub changes: Vec<ResolvedChange>,
    /// Directories that need a full re-read.
    pub structural_dirs: BTreeSet<PathBuf>,
}

impl ChangeBatch {
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.structural_dirs.is_empty()
    }

    fn extend(&mut self, other: Self) {
        self.changes.extend(other.changes);
        self.structural_dirs.extend(other.structural_dirs);
    }
}

/// Resolve a coalesced batch: one decision per path, made from the path's
/// current state (event order within a batch is not preserved by the set,
/// and the filesystem is the ground truth anyway).
fn resolve(changes: FileSystemChanges) -> ChangeBatch {
    let mut created = std::collections::BTreeMap::<PathBuf, bool>::new();
    for change in changes.file_changes {
        let (is_created, path) = match change {
            FileChange::Metadata(path) => (false, path),
            FileChange::Created(path) | FileChange::Removed(path) => (true, path),
        };
        *created.entry(path).or_default() |= is_created;
    }
    ChangeBatch {
        changes: created
            .into_iter()
            .map(|(path, created)| ResolvedChange::from_disk(path, created))
            .collect(),
        structural_dirs: changes.structural_dirs,
    }
}

/// Pairs the two halves of a rename. Backends with a tracker (inotify) pair
/// by it; the Windows backend sets none but delivers `OLD_NAME` immediately
/// followed by `NEW_NAME`, so an untracked "from" pairs with the next event
/// if that is an untracked "to".
#[derive(Debug, Default)]
struct RenamePairing {
    by_tracker: HashMap<usize, BTreeSet<PathBuf>>,
    last_untracked_from: Option<BTreeSet<PathBuf>>,
}

impl RenamePairing {
    fn remember(&mut self, tracker: Option<usize>, parents: BTreeSet<PathBuf>) {
        match tracker {
            Some(tracker) => {
                self.by_tracker.entry(tracker).or_default().extend(parents);
                if self.by_tracker.len() > 256 {
                    self.by_tracker.clear();
                }
            }
            None => self.last_untracked_from = Some(parents),
        }
    }

    fn take(&mut self, tracker: Option<usize>) -> Option<BTreeSet<PathBuf>> {
        match tracker {
            Some(tracker) => self.by_tracker.remove(&tracker),
            None => self.last_untracked_from.take(),
        }
    }
}

enum WatchCommand {
    Watch(PathBuf, RecursiveMode),
    Unwatch(PathBuf),
}

struct SharedWatcher {
    control: mpsc::Sender<WatchCommand>,
    events: Receiver<ChangeBatch>,
    failures: Receiver<(PathBuf, String)>,
    rescan_detected: Arc<AtomicBool>,
}

impl SharedWatcher {
    fn new() -> Result<Self> {
        let (event_tx, events) = mpsc::channel();
        let (internal_tx, internal_rx) = mpsc::channel::<notify::Result<Event>>();
        let watcher = notify::recommended_watcher(move |res| {
            if let Err(e) = internal_tx.send(res) {
                eprintln!("Failed to send file system event: {e}");
            }
        })
        .context("Failed to create file system watcher")?;

        let rescan_detected = Arc::new(AtomicBool::new(false));
        let rescan_flag = Arc::clone(&rescan_detected);
        thread::spawn(move || process_events(&internal_rx, &event_tx, &rescan_flag));

        // `watch` opens a directory handle, which can block (network drives),
        // so it never runs on the UI thread. One thread keeps watch/unwatch
        // in request order.
        let (control, control_rx) = mpsc::channel::<WatchCommand>();
        let (failure_tx, failures) = mpsc::channel();
        thread::spawn(move || {
            let mut watcher = watcher;
            for command in control_rx {
                match command {
                    WatchCommand::Watch(path, mode) => {
                        #[cfg(feature = "profiling")]
                        puffin::profile_scope!("lwa_fm::dir_handling::watch_directory");
                        if let Err(err) = watcher.watch(&path, mode) {
                            _ = failure_tx.send((path, format!("watch setup failed: {err:#}")));
                            request_repaint();
                        }
                    }
                    WatchCommand::Unwatch(path) => {
                        let _ = watcher.unwatch(&path);
                    }
                }
            }
        });

        Ok(Self {
            control,
            events,
            failures,
            rescan_detected,
        })
    }
}

/// Coalesce raw notify events into ~50 ms batches, then resolve each path's
/// metadata here, off the UI thread.
fn process_events(
    internal_rx: &Receiver<notify::Result<Event>>,
    event_tx: &mpsc::Sender<ChangeBatch>,
    rescan_flag: &AtomicBool,
) {
    let mut pending_renames = RenamePairing::default();
    while let Ok(first) = internal_rx.recv() {
        let deadline = Instant::now() + Duration::from_millis(50);
        let mut changes = FileSystemChanges::default();
        let mut disconnected = false;
        let mut next = Some(first);

        loop {
            if let Some(result) = next.take() {
                match result {
                    Ok(event) if event.kind == EventKind::Other => {
                        log::warn!(
                            "File system watcher overflow detected for {:?}, scheduling full refresh",
                            event.paths
                        );
                        rescan_flag.store(true, Ordering::SeqCst);
                    }
                    Ok(event) => {
                        changes
                            .extend(DirectoryWatcher::process_event(event, &mut pending_renames));
                    }
                    Err(error) => {
                        toast!(Error, "File system error: {error}");
                    }
                }
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match internal_rx.recv_timeout(remaining) {
                Ok(result) => next = Some(result),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        collapse_large_parent_bursts(&mut changes, 128);
        let batch = resolve(changes);
        let delivered = !batch.is_empty();
        if delivered && let Err(err) = event_tx.send(batch) {
            eprintln!("Failed to send processed file system event: {err}");
            break;
        }
        if delivered || rescan_flag.load(Ordering::SeqCst) {
            request_repaint();
        }
        if disconnected {
            break;
        }
    }
}

impl DirectoryWatchers {
    fn shared(&mut self) -> Option<&SharedWatcher> {
        if self.shared.is_none() {
            match SharedWatcher::new() {
                Ok(shared) => self.shared = Some(shared),
                Err(err) => {
                    log::warn!("File system watcher unavailable: {err:#}");
                    toast!(Warning, "Live updates unavailable");
                    return None;
                }
            }
        }
        self.shared.as_ref()
    }

    pub fn stop(&mut self, path: &Path) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::DirectoryWatchers::stop");
        // `start` keys roots by their normalized path.
        let path = if self.roots.contains_key(path) {
            path.to_path_buf()
        } else {
            normalize_path(path)
        };
        let Some(ref_count) = self.roots.get_mut(&path) else {
            return;
        };
        if *ref_count > 1 {
            *ref_count -= 1;
            return;
        }
        self.roots.remove(&path);
        if let Some(shared) = &self.shared {
            _ = shared.control.send(WatchCommand::Unwatch(path));
        }
    }

    /// How many folders are watched, for tests.
    #[cfg(test)]
    pub fn watched_roots(&self) -> usize {
        self.roots.len()
    }

    /// Stop every watch on or below `mount` whatever its reference count,
    /// so no watch handle keeps a drive that is being ejected busy.
    pub fn stop_under(&mut self, mount: &Path) {
        let roots: Vec<PathBuf> = self
            .roots
            .keys()
            .filter(|root| crate::drives::is_under(root, mount))
            .cloned()
            .collect();
        for root in roots {
            self.roots.remove(&root);
            if let Some(shared) = &self.shared {
                _ = shared.control.send(WatchCommand::Unwatch(root));
            }
        }
    }

    pub fn stop_many(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        for path in paths {
            self.stop(&path);
        }
    }

    pub fn start(&mut self, path: &Path, mode: RecursiveMode) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::DirectoryWatchers::start");
        let path = normalize_path(path);
        if let Some(ref_count) = self.roots.get_mut(&path) {
            *ref_count += 1;
            return;
        }
        let Some(shared) = self.shared() else {
            return;
        };
        _ = shared.control.send(WatchCommand::Watch(path.clone(), mode));
        self.roots.insert(path, 1);
    }

    pub fn start_many(&mut self, paths: impl IntoIterator<Item = (PathBuf, RecursiveMode)>) {
        for (path, mode) in paths {
            self.start(&path, mode);
        }
    }

    /// Report roots whose watch could not be set up.
    pub fn check_for_new_watchers(&mut self) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::DirectoryWatchers::check_for_new_watchers");
        let Some(shared) = &self.shared else {
            return;
        };
        let failed: Vec<(PathBuf, String)> = shared.failures.try_iter().collect();
        for (path, error) in failed {
            self.roots.remove(&path);
            log::warn!("File system watcher failed for {}: {error}", path.display());
            // A folder that is gone (its drive was pulled) needs no message.
            if path.exists() {
                toast!(Warning, "Live updates unavailable for {}", path.display());
            }
        }
    }

    pub fn check_for_file_system_events(&self) -> ChangeBatch {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::DirectoryWatchers::check_for_file_system_events");
        let mut changes = ChangeBatch::default();
        let Some(shared) = &self.shared else {
            return changes;
        };
        if shared.rescan_detected.swap(false, Ordering::SeqCst) {
            // Overflow: the specific paths are unknown, so refresh every root.
            log::info!("Rescan triggered full refresh of watched roots");
            changes.structural_dirs.extend(self.roots.keys().cloned());
        }
        for event_changes in shared.events.try_iter().take(256) {
            changes.extend(event_changes);
        }
        changes
    }
}

/// Namespace for event classification.
pub struct DirectoryWatcher;

impl DirectoryWatcher {
    fn process_event(event: Event, renames: &mut RenamePairing) -> FileSystemChanges {
        let tracker = event.attrs.tracker();
        let Event {
            kind,
            paths,
            attrs: _,
        } = event;

        // An untracked rename "from" only pairs with the event right after it.
        if !matches!(kind, EventKind::Modify(ModifyKind::Name(RenameMode::To))) {
            renames.last_untracked_from = None;
        }

        match kind {
            // Windows reports creates and deletes as `Any`; folders are rows
            // too, so all of them are surgical for a flat listing (a deep
            // listing falls back to a refresh when the change is applied).
            EventKind::Create(CreateKind::Any | CreateKind::File | CreateKind::Folder) => {
                FileSystemChanges {
                    file_changes: file_changes_from(paths, FileChange::Created),
                    ..Default::default()
                }
            }
            EventKind::Remove(RemoveKind::Any | RemoveKind::File | RemoveKind::Folder) => {
                FileSystemChanges {
                    file_changes: file_changes_from(paths, FileChange::Removed),
                    ..Default::default()
                }
            }
            // Rename "From" half. The source path is gone regardless of whether
            // a matching "To" arrives, so emit a surgical `Removed` now and
            // remember the source parent dirs so the "To" half can decide
            // between a same-dir coalesce and a cross-dir refresh.
            EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                renames.remember(tracker, parent_dirs_for_paths(paths.clone()));
                FileSystemChanges {
                    file_changes: file_changes_from(paths, FileChange::Removed),
                    ..Default::default()
                }
            }
            // Rename "To" half. If the remembered "From" lived in the same
            // directory (the common atomic-save pattern: write `file.tmp` then
            // rename over `file`), this is a surgical `Created` and no structural
            // refresh is needed. Otherwise fall back to a structural refresh of
            // both directories.
            EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
                let to_parents = parent_dirs_for_paths(paths.clone());
                let remembered = renames.take(tracker);
                let same_dir = remembered
                    .as_ref()
                    .is_some_and(|from_parents| from_parents == &to_parents);
                if same_dir {
                    FileSystemChanges {
                        file_changes: file_changes_from(paths, FileChange::Created),
                        ..Default::default()
                    }
                } else {
                    let mut directories = to_parents;
                    if let Some(from_parents) = remembered {
                        directories.extend(from_parents);
                    }
                    FileSystemChanges {
                        structural_dirs: directories,
                        ..Default::default()
                    }
                }
            }
            // Both halves in one event (`[from, to]`, inotify).
            EventKind::Modify(ModifyKind::Name(RenameMode::Both))
                if paths.len() == 2
                    && parent_dir_for_event_path(&paths[0])
                        == parent_dir_for_event_path(&paths[1]) =>
            {
                let mut paths = paths;
                let to = paths
                    .pop()
                    .map(|path| FileChange::Created(canonical_form(&path)));
                let from = paths
                    .pop()
                    .map(|path| FileChange::Removed(canonical_form(&path)));
                FileSystemChanges {
                    file_changes: from.into_iter().chain(to).collect(),
                    ..Default::default()
                }
            }
            // Windows reports every content or attribute change as `Any`.
            EventKind::Modify(ModifyKind::Any | ModifyKind::Data(_) | ModifyKind::Metadata(_)) => {
                FileSystemChanges {
                    file_changes: file_changes_from(paths, FileChange::Metadata),
                    ..Default::default()
                }
            }
            // Ambiguous events require a structural refresh.
            EventKind::Create(CreateKind::Other)
            | EventKind::Remove(RemoveKind::Other)
            | EventKind::Modify(ModifyKind::Name(_) | ModifyKind::Other)
            | EventKind::Any => FileSystemChanges {
                structural_dirs: parent_dirs_for_paths(paths),
                ..Default::default()
            },
            EventKind::Access(_) | EventKind::Other => FileSystemChanges::default(),
        }
    }
}

fn collapse_large_parent_bursts(changes: &mut FileSystemChanges, surgical_limit: usize) {
    let mut counts = HashMap::<PathBuf, usize>::new();
    for change in &changes.file_changes {
        let path = match change {
            FileChange::Metadata(path) | FileChange::Created(path) | FileChange::Removed(path) => {
                path
            }
        };
        if let Some(parent) = path.parent() {
            *counts.entry(canonical_form(parent)).or_default() += 1;
        }
    }
    let structural = counts
        .into_iter()
        .filter_map(|(parent, count)| (count > surgical_limit).then_some(parent))
        .collect::<BTreeSet<_>>();
    if structural.is_empty() {
        return;
    }
    changes.file_changes.retain(|change| {
        let path = match change {
            FileChange::Metadata(path) | FileChange::Created(path) | FileChange::Removed(path) => {
                path
            }
        };
        path.parent()
            .map(canonical_form)
            .is_none_or(|parent| !structural.contains(&parent))
    });
    changes.structural_dirs.extend(structural);
}

fn parent_dir_for_event_path(path: &Path) -> Option<PathBuf> {
    path.parent().map(canonical_form)
}

fn parent_dirs_for_paths(paths: Vec<PathBuf>) -> BTreeSet<PathBuf> {
    paths
        .into_iter()
        .filter_map(|path| parent_dir_for_event_path(&path))
        .collect()
}

/// Map raw event paths into `FileChange`s of the given kind, normalising each
/// path exactly once.
fn file_changes_from(paths: Vec<PathBuf>, kind: fn(PathBuf) -> FileChange) -> BTreeSet<FileChange> {
    paths
        .into_iter()
        .map(|path| kind(canonical_form(&path)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        ChangeKind, DirectoryWatcher, FileChange, FileSystemChanges, RenamePairing, ResolvedChange,
        collapse_large_parent_bursts, parent_dir_for_event_path, resolve,
    };
    use crate::helper::normalize_path;
    use notify::{
        Event, EventKind,
        event::{CreateKind, DataChange, ModifyKind, RemoveKind, RenameMode},
    };
    use std::collections::BTreeSet;
    use std::path::Path;

    #[test]
    fn shared_watcher_reports_changes_for_every_root() {
        let base = normalize_path(&std::env::temp_dir())
            .join(format!("lwa_fm_shared_watch_{}", std::process::id()));
        let (first, second) = (base.join("first"), base.join("second"));
        std::fs::create_dir_all(&first).expect("create first root");
        std::fs::create_dir_all(&second).expect("create second root");
        let mut watchers = super::DirectoryWatchers::default();
        watchers.start(&first, notify::RecursiveMode::NonRecursive);
        watchers.start(&second, notify::RecursiveMode::NonRecursive);
        // Watch setup runs on the control thread; give it a moment.
        std::thread::sleep(std::time::Duration::from_millis(200));
        std::fs::write(first.join("a.txt"), b"a").expect("write first");
        std::fs::write(second.join("b.txt"), b"b").expect("write second");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut seen = BTreeSet::new();
        while seen.len() < 2 && std::time::Instant::now() < deadline {
            watchers.check_for_new_watchers();
            let changes = watchers.check_for_file_system_events();
            for change in changes.changes {
                seen.insert(change.path.parent().map(Path::to_path_buf));
            }
            seen.extend(changes.structural_dirs.into_iter().map(Some));
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            seen,
            BTreeSet::from([Some(first.clone()), Some(second.clone())])
        );

        // Ref-counted: one stop keeps a doubly started root watched.
        watchers.start(&first, notify::RecursiveMode::NonRecursive);
        watchers.stop(&first);
        assert_eq!(watchers.roots.get(&first), Some(&1));
        watchers.stop_many([first, second]);
        assert!(watchers.roots.is_empty());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn rename_mode_any_invalidates_both_parents() {
        let event = Event {
            kind: EventKind::Modify(ModifyKind::Name(RenameMode::Any)),
            paths: vec![
                Path::new("/tmp/from.txt").into(),
                Path::new("/tmp/to.txt").into(),
            ],
            attrs: notify::event::EventAttributes::default(),
        };

        let mut pending = RenamePairing::default();
        let changes = DirectoryWatcher::process_event(event, &mut pending);

        assert_eq!(
            changes.structural_dirs,
            BTreeSet::from([crate::helper::canonical_form(Path::new("/tmp"))])
        );
        assert!(changes.file_changes.is_empty());
    }

    #[test]
    fn rename_mode_to_without_from_still_refreshes_parent() {
        let event = Event {
            kind: EventKind::Modify(ModifyKind::Name(RenameMode::To)),
            paths: vec![Path::new("/tmp/to.txt").into()],
            attrs: notify::event::EventAttributes::default(),
        };

        let mut pending = RenamePairing::default();
        let changes = DirectoryWatcher::process_event(event, &mut pending);

        assert_eq!(
            changes.structural_dirs,
            BTreeSet::from([crate::helper::canonical_form(Path::new("/tmp"))])
        );
        assert!(changes.file_changes.is_empty());
    }

    #[test]
    fn tracker_pairs_from_and_to_directories() {
        let mut pending = RenamePairing::default();
        let from = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
            .add_path(Path::new("/tmp/source/file.txt").to_path_buf())
            .set_tracker(7);
        let to = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::To)))
            .add_path(Path::new("/tmp/dest/file.txt").to_path_buf())
            .set_tracker(7);

        let from_changes = DirectoryWatcher::process_event(from, &mut pending);
        let to_changes = DirectoryWatcher::process_event(to, &mut pending);

        // Cross-directory rename: From is a surgical `Removed`; To falls back to a
        // structural refresh covering both directories.
        assert_eq!(
            from_changes.file_changes,
            BTreeSet::from([FileChange::Removed(normalize_path(Path::new(
                "/tmp/source/file.txt"
            )))])
        );
        assert!(from_changes.structural_dirs.is_empty());
        assert_eq!(
            to_changes.structural_dirs,
            BTreeSet::from([
                normalize_path(Path::new("/tmp/source")),
                normalize_path(Path::new("/tmp/dest"))
            ])
        );
        assert!(to_changes.file_changes.is_empty());
    }

    #[test]
    fn same_directory_rename_coalesces_to_removed_plus_created() {
        // Atomic-save pattern: write `file.tmp`, rename over `file` in the same
        // directory. From -> Removed(tmp), To -> Created(file), no structural.
        let mut pending = RenamePairing::default();
        let from = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
            .add_path(Path::new("/tmp/file.tmp").to_path_buf())
            .set_tracker(42);
        let to = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::To)))
            .add_path(Path::new("/tmp/file.txt").to_path_buf())
            .set_tracker(42);

        let from_changes = DirectoryWatcher::process_event(from, &mut pending);
        let to_changes = DirectoryWatcher::process_event(to, &mut pending);

        assert_eq!(
            from_changes.file_changes,
            BTreeSet::from([FileChange::Removed(normalize_path(Path::new(
                "/tmp/file.tmp"
            )))])
        );
        assert!(from_changes.structural_dirs.is_empty());
        assert_eq!(
            to_changes.file_changes,
            BTreeSet::from([FileChange::Created(normalize_path(Path::new(
                "/tmp/file.txt"
            )))])
        );
        assert!(to_changes.structural_dirs.is_empty());
    }

    #[test]
    fn data_modify_updates_file_without_structural_refresh() {
        let event = Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
            .add_path(Path::new("/tmp/hot.log").to_path_buf());

        let mut pending = RenamePairing::default();
        let changes = DirectoryWatcher::process_event(event, &mut pending);

        assert_eq!(
            changes.file_changes,
            BTreeSet::from([FileChange::Metadata(normalize_path(Path::new(
                "/tmp/hot.log"
            )))])
        );
        assert!(changes.structural_dirs.is_empty());
    }

    #[test]
    fn directory_event_invalidates_parent_directory() {
        let parent = parent_dir_for_event_path(Path::new("/tmp/example/subdir"));

        assert_eq!(parent, Some(normalize_path(Path::new("/tmp/example"))));
    }

    // The next four tests PIN the current bucket routing for non-rename events
    // so the surgical-update refactor (later phases) can change them deliberately.

    #[test]
    fn create_file_routed_as_created_change() {
        let event = Event::new(EventKind::Create(CreateKind::File))
            .add_path(Path::new("/tmp/new_file.txt").to_path_buf());

        let mut pending = RenamePairing::default();
        let changes = DirectoryWatcher::process_event(event, &mut pending);

        assert_eq!(
            changes.file_changes,
            BTreeSet::from([FileChange::Created(normalize_path(Path::new(
                "/tmp/new_file.txt"
            )))])
        );
        assert!(changes.structural_dirs.is_empty());
    }

    #[test]
    fn remove_file_routed_as_removed_change() {
        let event = Event::new(EventKind::Remove(RemoveKind::File))
            .add_path(Path::new("/tmp/gone_file.txt").to_path_buf());

        let mut pending = RenamePairing::default();
        let changes = DirectoryWatcher::process_event(event, &mut pending);

        assert_eq!(
            changes.file_changes,
            BTreeSet::from([FileChange::Removed(normalize_path(Path::new(
                "/tmp/gone_file.txt"
            )))])
        );
        assert!(changes.structural_dirs.is_empty());
    }

    #[test]
    fn metadata_modify_routed_as_metadata_change() {
        let event = Event::new(EventKind::Modify(ModifyKind::Metadata(
            notify::event::MetadataKind::Any,
        )))
        .add_path(Path::new("/tmp/hot.log").to_path_buf());

        let mut pending = RenamePairing::default();
        let changes = DirectoryWatcher::process_event(event, &mut pending);

        assert_eq!(
            changes.file_changes,
            BTreeSet::from([FileChange::Metadata(normalize_path(Path::new(
                "/tmp/hot.log"
            )))])
        );
        assert!(changes.structural_dirs.is_empty());
    }

    fn route(kind: EventKind, path: &str) -> FileSystemChanges {
        let mut pending = RenamePairing::default();
        DirectoryWatcher::process_event(
            Event::new(kind).add_path(Path::new(path).to_path_buf()),
            &mut pending,
        )
    }

    #[test]
    fn windows_any_kinds_are_surgical() {
        let path = || normalize_path(Path::new("/tmp/w.txt"));
        assert_eq!(
            route(EventKind::Create(CreateKind::Any), "/tmp/w.txt").file_changes,
            BTreeSet::from([FileChange::Created(path())])
        );
        assert_eq!(
            route(EventKind::Remove(RemoveKind::Any), "/tmp/w.txt").file_changes,
            BTreeSet::from([FileChange::Removed(path())])
        );
        let modified = route(EventKind::Modify(ModifyKind::Any), "/tmp/w.txt");
        assert_eq!(
            modified.file_changes,
            BTreeSet::from([FileChange::Metadata(path())])
        );
        assert!(modified.structural_dirs.is_empty());
        let folder = route(EventKind::Remove(RemoveKind::Folder), "/tmp/sub");
        assert_eq!(
            folder.file_changes,
            BTreeSet::from([FileChange::Removed(normalize_path(Path::new("/tmp/sub")))])
        );
        assert!(folder.structural_dirs.is_empty());
        assert!(
            !route(EventKind::Create(CreateKind::Other), "/tmp/w.txt")
                .structural_dirs
                .is_empty()
        );
    }

    #[test]
    fn untracked_from_then_to_same_dir_coalesces() {
        let mut pending = RenamePairing::default();
        let from = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
            .add_path(Path::new("/tmp/old.txt").to_path_buf());
        let to = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::To)))
            .add_path(Path::new("/tmp/new.txt").to_path_buf());
        let from_changes = DirectoryWatcher::process_event(from, &mut pending);
        let to_changes = DirectoryWatcher::process_event(to, &mut pending);
        assert_eq!(
            from_changes.file_changes,
            BTreeSet::from([FileChange::Removed(normalize_path(Path::new(
                "/tmp/old.txt"
            )))])
        );
        assert_eq!(
            to_changes.file_changes,
            BTreeSet::from([FileChange::Created(normalize_path(Path::new(
                "/tmp/new.txt"
            )))])
        );
        assert!(to_changes.structural_dirs.is_empty());
    }

    #[test]
    fn untracked_from_only_pairs_with_the_next_event() {
        let mut pending = RenamePairing::default();
        let from = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
            .add_path(Path::new("/tmp/old.txt").to_path_buf());
        let other = Event::new(EventKind::Modify(ModifyKind::Any))
            .add_path(Path::new("/tmp/other.txt").to_path_buf());
        let to = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::To)))
            .add_path(Path::new("/tmp/new.txt").to_path_buf());
        _ = DirectoryWatcher::process_event(from, &mut pending);
        _ = DirectoryWatcher::process_event(other, &mut pending);
        let to_changes = DirectoryWatcher::process_event(to, &mut pending);
        assert!(to_changes.file_changes.is_empty());
        assert_eq!(
            to_changes.structural_dirs,
            BTreeSet::from([crate::helper::canonical_form(Path::new("/tmp"))])
        );
    }

    #[test]
    fn rename_both_in_one_event_is_surgical_within_a_directory() {
        let mut pending = RenamePairing::default();
        let event = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path(Path::new("/tmp/a.txt").to_path_buf())
            .add_path(Path::new("/tmp/b.txt").to_path_buf());
        let changes = DirectoryWatcher::process_event(event, &mut pending);
        assert_eq!(
            changes.file_changes,
            BTreeSet::from([
                FileChange::Removed(normalize_path(Path::new("/tmp/a.txt"))),
                FileChange::Created(normalize_path(Path::new("/tmp/b.txt"))),
            ])
        );
        assert!(changes.structural_dirs.is_empty());
    }

    #[test]
    fn resolve_reads_current_state_once_per_path() {
        let dir = normalize_path(&std::env::temp_dir())
            .join(format!("lwa_fm_resolve_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create dir");
        let kept = dir.join("kept.txt");
        let touched = dir.join("touched.txt");
        let gone = dir.join("gone.txt");
        std::fs::write(&kept, b"12345").expect("write kept");
        std::fs::write(&touched, b"1").expect("write touched");
        let batch = resolve(FileSystemChanges {
            file_changes: BTreeSet::from([
                // Removed then re-created within one batch: it exists now.
                FileChange::Removed(kept.clone()),
                FileChange::Created(kept.clone()),
                FileChange::Metadata(touched.clone()),
                FileChange::Metadata(gone.clone()),
            ]),
            structural_dirs: BTreeSet::new(),
        });
        let kind_of = |path: &std::path::PathBuf| {
            batch
                .changes
                .iter()
                .find(|change| &change.path == path)
                .map(|change| change.kind.clone())
        };
        assert_eq!(batch.changes.len(), 3);
        assert!(matches!(kind_of(&kept), Some(ChangeKind::Upsert(meta)) if meta.size == 5));
        assert!(matches!(kind_of(&touched), Some(ChangeKind::Update(_))));
        assert_eq!(kind_of(&gone), Some(ChangeKind::Remove));
        assert_eq!(
            ResolvedChange::from_disk(gone, true).kind,
            ChangeKind::Remove
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Changes made by another process reach the app as surgical edits. The
    /// empty-file create (`copy nul`) only produces `FILE_ACTION_ADDED`, which
    /// used to be dropped on Windows.
    #[cfg(windows)]
    #[test]
    fn external_create_delete_and_rename_are_surgical() {
        let dir = normalize_path(&std::env::temp_dir())
            .join(format!("lwa_fm_external_watch_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create watched dir");
        let mut watchers = super::DirectoryWatchers::default();
        watchers.start(&dir, notify::RecursiveMode::NonRecursive);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let mut structural = BTreeSet::new();
        let mut wait_for = |label: &str, want: &dyn Fn(&ResolvedChange) -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                let batch = watchers.check_for_file_system_events();
                structural.extend(batch.structural_dirs);
                if batch.changes.iter().any(want) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            panic!("no change matching {label}");
        };
        let cmd = |args: &[&str]| {
            let status = std::process::Command::new("cmd")
                .arg("/C")
                .args(args)
                .current_dir(&dir)
                .status()
                .expect("run cmd");
            assert!(status.success(), "{args:?}");
        };
        let created = dir.join("created.txt");
        cmd(&["copy", "nul", "created.txt"]);
        wait_for("empty create", &|change| {
            change.path == created
                && matches!(change.kind, ChangeKind::Upsert(meta) if meta.size == 0)
        });
        cmd(&["del", "created.txt"]);
        wait_for("delete", &|change| {
            change.path == created && change.kind == ChangeKind::Remove
        });
        let old = dir.join("old.txt");
        let new = dir.join("new.txt");
        cmd(&["copy", "nul", "old.txt"]);
        wait_for("rename source", &|change| change.path == old);
        cmd(&["ren", "old.txt", "new.txt"]);
        wait_for("rename target", &|change| {
            change.path == new && matches!(change.kind, ChangeKind::Upsert(_))
        });
        assert!(
            structural.is_empty(),
            "unexpected refreshes: {structural:?}"
        );
        watchers.stop(&dir);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn large_burst_becomes_one_structural_refresh_per_parent() {
        let parent = normalize_path(Path::new("/tmp/burst"));
        let mut changes = FileSystemChanges {
            file_changes: (0..1_000)
                .map(|index| FileChange::Created(parent.join(format!("{index}.txt"))))
                .collect(),
            structural_dirs: BTreeSet::new(),
        };

        collapse_large_parent_bursts(&mut changes, 128);

        assert!(changes.file_changes.is_empty());
        assert_eq!(changes.structural_dirs, BTreeSet::from([parent]));
    }
}
