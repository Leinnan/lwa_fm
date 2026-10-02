use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use notify::{
    Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{CreateKind, ModifyKind, RemoveKind, RenameMode},
};

use crate::helper::normalize_path;
use crate::toast;

#[derive(Debug, Default)]
pub struct DirectoryWatchers {
    watchers: HashMap<PathBuf, WatchedDirectory>,
    pending_paths: HashMap<PathBuf, usize>,
    receivers: Vec<PendingWatcherReceiver>,
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
    pub fn is_empty(&self) -> bool {
        self.file_changes.is_empty() && self.structural_dirs.is_empty()
    }

    #[inline]
    fn extend(&mut self, other: Self) {
        self.file_changes.extend(other.file_changes);
        self.structural_dirs.extend(other.structural_dirs);
    }
}

#[derive(Debug)]
struct PendingWatcherReceiver {
    path: PathBuf,
    mode: RecursiveMode,
    receiver: Receiver<Result<DirectoryWatcher, String>>,
}

#[derive(Debug)]
struct WatchedDirectory {
    watcher: DirectoryWatcher,
    ref_count: usize,
}

impl DirectoryWatchers {
    #[inline]
    pub fn is_active(&self) -> bool {
        !self.watchers.is_empty() || !self.pending_paths.is_empty() || !self.receivers.is_empty()
    }

    #[inline]
    pub fn stop(&mut self, path: &Path) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::DirectoryWatchers::stop");
        let should_remove = if let Some(watched) = self.watchers.get_mut(path) {
            if watched.ref_count > 1 {
                watched.ref_count -= 1;
                false
            } else {
                true
            }
        } else {
            false
        };

        if !should_remove {
            return;
        }

        let Some(mut watched) = self.watchers.remove(path) else {
            return;
        };
        std::thread::spawn(move || {
            watched.watcher.stop_watching();
        });
    }

    #[inline]
    fn stop_pending(&mut self, path: &Path) -> bool {
        if let Some(ref_count) = self.pending_paths.get_mut(path) {
            if *ref_count > 1 {
                *ref_count -= 1;
            } else {
                self.pending_paths.remove(path);
            }
            return true;
        }
        false
    }

    #[inline]
    fn increment_pending(&mut self, path: &Path) {
        let entry = self.pending_paths.entry(path.to_path_buf()).or_insert(0);
        *entry += 1;
    }

    #[inline]
    fn take_pending_ref_count(&mut self, path: &Path) -> usize {
        self.pending_paths.remove(path).unwrap_or(1)
    }

    #[inline]
    fn has_pending(&self, path: &Path) -> bool {
        self.pending_paths.contains_key(path)
    }

    #[inline]
    fn drop_pending_receiver(&mut self, path: &Path) {
        self.receivers.retain(|pending| pending.path != path);
    }

    #[inline]
    fn insert_started_watcher(&mut self, watcher: DirectoryWatcher, _mode: RecursiveMode) {
        let Some(path) = watcher.current_path.clone() else {
            return;
        };
        let ref_count = self.take_pending_ref_count(&path);
        if let Some(existing) = self.watchers.get_mut(&path) {
            existing.ref_count += ref_count;
            return;
        }
        _ = self
            .watchers
            .insert(path, WatchedDirectory { watcher, ref_count });
    }

    pub fn stop_many(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        for path in paths {
            if self.stop_pending(&path) {
                self.drop_pending_receiver(&path);
                continue;
            }
            self.stop(&path);
        }
    }

    pub fn start(&mut self, path: &Path, mode: RecursiveMode) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::DirectoryWatchers::start");
        let path = normalize_path(path);
        if let Some(watched) = self.watchers.get_mut(&path) {
            watched.ref_count += 1;
            return;
        }
        if self.has_pending(&path) {
            self.increment_pending(&path);
            return;
        }

        let (tx, rx) = mpsc::channel();
        let path_for_thread = path.clone();
        self.increment_pending(&path);

        std::thread::spawn(move || {
            // Report both failure modes (watcher creation and watch setup) back to
            // the UI thread instead of dying silently. A silent failure would leave
            // this directory unwatched with no indication that live updates (file
            // change auto-refresh) are no longer working for it.
            match DirectoryWatcher::new() {
                Ok(mut watcher) => match watcher.watch_directory(&path_for_thread, mode) {
                    Ok(()) => {
                        _ = tx.send(Ok(watcher));
                    }
                    Err(err) => {
                        _ = tx.send(Err(format!("watch setup failed: {err:#}")));
                    }
                },
                Err(err) => {
                    _ = tx.send(Err(format!("watcher creation failed: {err:#}")));
                }
            }
        });
        self.receivers.push(PendingWatcherReceiver {
            path,
            mode,
            receiver: rx,
        });
    }

    pub fn start_many(&mut self, paths: impl IntoIterator<Item = (PathBuf, RecursiveMode)>) {
        for (path, mode) in paths {
            self.start(&path, mode);
        }
    }

    pub fn check_for_new_watchers(&mut self) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::DirectoryWatchers::check_for_new_watchers");
        let mut ready_watchers = Vec::new();
        let mut failed_paths = Vec::new();
        self.receivers
            .retain(|pending| match pending.receiver.try_recv() {
                Ok(Ok(watcher)) => {
                    ready_watchers.push((watcher, pending.mode));
                    false
                }
                Ok(Err(error)) => {
                    failed_paths.push((pending.path.clone(), error));
                    false
                }
                Err(TryRecvError::Empty) => true,
                Err(TryRecvError::Disconnected) => {
                    // The worker thread ended without reporting back — treat it as
                    // a failure so the user is told live updates are unavailable.
                    failed_paths.push((
                        pending.path.clone(),
                        "watcher thread exited unexpectedly".to_string(),
                    ));
                    false
                }
            });
        for (path, error) in &failed_paths {
            self.pending_paths.remove(path);
            log::warn!("File system watcher failed for {}: {error}", path.display());
            toast!(Warning, "Live updates unavailable for {}", path.display());
        }
        for (watcher, mode) in ready_watchers {
            self.insert_started_watcher(watcher, mode);
        }
    }

    pub fn check_for_file_system_events(&mut self) -> FileSystemChanges {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::DirectoryWatchers::check_for_file_system_events");
        let mut changes = FileSystemChanges::default();
        let mut event_budget = 256usize;
        for watched in self.watchers.values_mut() {
            if watched.watcher.check_rescan() {
                // Rescan detected: we don't know which specific paths changed,
                // so return the watched root as structural to trigger a full refresh.
                if let Some(ref path) = watched.watcher.current_path {
                    log::info!("Rescan triggered full refresh for {}", path.display());
                    changes.structural_dirs.insert(path.clone());
                }
            }
            while event_budget > 0
                && let Some(event_changes) = watched.watcher.try_recv_event()
            {
                changes.extend(event_changes);
                event_budget -= 1;
            }
            if event_budget == 0 {
                break;
            }
        }
        changes
    }
}

#[derive(Debug)]
pub struct DirectoryWatcher {
    watcher: RecommendedWatcher,
    receiver: Receiver<FileSystemChanges>,
    current_path: Option<PathBuf>,
    rescan_detected: Arc<AtomicBool>,
}

impl DirectoryWatcher {
    pub fn new() -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let (internal_tx, internal_rx) = mpsc::channel::<notify::Result<Event>>();

        // Create the notify watcher
        let watcher = notify::recommended_watcher(move |res| {
            if let Err(e) = internal_tx.send(res) {
                eprintln!("Failed to send file system event: {e}");
            }
        })
        .context("Failed to create file system watcher")?;

        let rescan_detected = Arc::new(AtomicBool::new(false));
        let rescan_flag = Arc::clone(&rescan_detected);

        // Spawn a thread to process raw notify events and convert them to our custom events
        let event_tx = tx;
        thread::spawn(move || {
            let mut pending_renames: HashMap<usize, BTreeSet<PathBuf>> = HashMap::new();
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
                                changes.extend(Self::process_event(event, &mut pending_renames));
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
                if !changes.is_empty()
                    && let Err(err) = event_tx.send(changes)
                {
                    eprintln!("Failed to send processed file system event: {err}");
                    break;
                }
                if disconnected {
                    break;
                }
            }
        });

        Ok(Self {
            watcher,
            receiver: rx,
            current_path: None,
            rescan_detected,
        })
    }

    pub fn watch_directory<P: AsRef<Path>>(&mut self, path: P, mode: RecursiveMode) -> Result<()> {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::watch_directory");
        let path = normalize_path(path.as_ref());

        self.watcher
            .watch(&path, mode)
            .with_context(|| format!("Failed to watch directory: {}", path.display()))?;

        self.current_path = Some(path);
        Ok(())
    }

    pub fn stop_watching(&mut self) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::watch_directory::unwatch");
        if let Some(path) = &self.current_path {
            let _ = self.watcher.unwatch(path);
            self.current_path = None;
        }
    }

    #[inline]
    pub fn try_recv_event(&self) -> Option<FileSystemChanges> {
        self.receiver.try_recv().ok()
    }

    /// Returns true if a Rescan (buffer overflow) event was detected since last check.
    /// Atomically clears the flag on read.
    pub fn check_rescan(&self) -> bool {
        self.rescan_detected
            .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    #[allow(dead_code)]
    pub fn recv_event_timeout(&self, timeout: Duration) -> Option<FileSystemChanges> {
        self.receiver.recv_timeout(timeout).ok()
    }

    fn process_event(
        event: Event,
        pending_renames: &mut HashMap<usize, BTreeSet<PathBuf>>,
    ) -> FileSystemChanges {
        let tracker = event.attrs.tracker();
        let Event {
            kind,
            paths,
            attrs: _,
        } = event;

        match kind {
            // A new file: can be inserted surgically once the handler is wired.
            EventKind::Create(CreateKind::File) => FileSystemChanges {
                file_changes: file_changes_from(paths, FileChange::Created),
                ..Default::default()
            },
            // A deleted file: can be removed surgically once the handler is wired.
            EventKind::Remove(RemoveKind::File) => FileSystemChanges {
                file_changes: file_changes_from(paths, FileChange::Removed),
                ..Default::default()
            },
            // Rename "From" half. The source path is gone regardless of whether
            // a matching "To" arrives, so emit a surgical `Removed` now and
            // remember the source parent dirs (keyed by tracker) so the "To"
            // half can decide between a same-dir coalesce and a cross-dir refresh.
            EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                let directories = parent_dirs_for_paths(paths.clone());
                if let Some(tracker) = tracker {
                    pending_renames
                        .entry(tracker)
                        .or_default()
                        .extend(directories.iter().cloned());
                    if pending_renames.len() > 256 {
                        pending_renames.clear();
                    }
                }
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
                let remembered = tracker.and_then(|t| pending_renames.remove(&t));
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
            EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Metadata(_)) => FileSystemChanges {
                file_changes: file_changes_from(paths, FileChange::Metadata),
                ..Default::default()
            },
            // Folder changes and ambiguous events require a structural refresh.
            EventKind::Create(CreateKind::Folder)
            | EventKind::Remove(RemoveKind::Folder)
            | EventKind::Modify(ModifyKind::Name(_) | ModifyKind::Any | ModifyKind::Other)
            | EventKind::Any => FileSystemChanges {
                structural_dirs: parent_dirs_for_paths(paths),
                ..Default::default()
            },
            _ => FileSystemChanges::default(),
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
            *counts.entry(normalize_path(parent)).or_default() += 1;
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
            .map(normalize_path)
            .is_none_or(|parent| !structural.contains(&parent))
    });
    changes.structural_dirs.extend(structural);
}

fn parent_dir_for_event_path(path: &Path) -> Option<PathBuf> {
    path.parent().map(normalize_path)
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
        .map(|path| kind(normalize_path(&path)))
        .collect()
}

impl Default for DirectoryWatcher {
    fn default() -> Self {
        Self::new().expect("Failed to create directory watcher")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DirectoryWatcher, FileChange, FileSystemChanges, collapse_large_parent_bursts,
        parent_dir_for_event_path,
    };
    use crate::helper::normalize_path;
    use notify::{
        Event, EventKind,
        event::{CreateKind, DataChange, ModifyKind, RemoveKind, RenameMode},
    };
    use std::collections::{BTreeSet, HashMap};
    use std::path::Path;

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

        let mut pending = HashMap::new();
        let changes = DirectoryWatcher::process_event(event, &mut pending);

        assert_eq!(
            changes.structural_dirs,
            BTreeSet::from([normalize_path(Path::new("/tmp"))])
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

        let mut pending = HashMap::new();
        let changes = DirectoryWatcher::process_event(event, &mut pending);

        assert_eq!(
            changes.structural_dirs,
            BTreeSet::from([normalize_path(Path::new("/tmp"))])
        );
        assert!(changes.file_changes.is_empty());
    }

    #[test]
    fn tracker_pairs_from_and_to_directories() {
        let mut pending = HashMap::new();
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
        let mut pending = HashMap::new();
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

        let mut pending = HashMap::new();
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

        let mut pending = HashMap::new();
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

        let mut pending = HashMap::new();
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

        let mut pending = HashMap::new();
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
