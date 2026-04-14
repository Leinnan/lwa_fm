use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use notify::{
    Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{CreateKind, DataChange, ModifyKind, RemoveKind, RenameMode},
};

use crate::helper::normalize_path;
use crate::toast;

#[derive(Debug, Default)]
pub struct DirectoryWatchers {
    watchers: HashMap<PathBuf, WatchedDirectory>,
    pending_paths: HashMap<PathBuf, usize>,
    receivers: Vec<PendingWatcherReceiver>,
}

#[derive(Debug)]
struct PendingWatcherReceiver {
    path: PathBuf,
    mode: RecursiveMode,
    receiver: Receiver<DirectoryWatcher>,
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

    pub fn start(&mut self, path: PathBuf, mode: RecursiveMode) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::DirectoryWatchers::start");
        let path = normalize_path(&path);
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
            let Ok(mut watcher) = DirectoryWatcher::new() else {
                return;
            };
            if let Err(err) = watcher.watch_directory(&path_for_thread, mode) {
                eprintln!("Failed to watch directory: {err}");
                return;
            }
            _ = tx.send(watcher);
        });
        self.receivers.push(PendingWatcherReceiver {
            path,
            mode,
            receiver: rx,
        });
    }

    pub fn start_many(&mut self, paths: impl IntoIterator<Item = (PathBuf, RecursiveMode)>) {
        for (path, mode) in paths {
            self.start(path, mode);
        }
    }

    pub fn check_for_new_watchers(&mut self) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::DirectoryWatchers::check_for_new_watchers");
        let mut ready_watchers = Vec::new();
        let mut disconnected_paths = Vec::new();
        self.receivers
            .retain(|pending| match pending.receiver.try_recv() {
                Ok(watcher) => {
                    ready_watchers.push((watcher, pending.mode));
                    false
                }
                Err(TryRecvError::Empty) => true,
                Err(TryRecvError::Disconnected) => {
                    disconnected_paths.push(pending.path.clone());
                    false
                }
            });
        for path in disconnected_paths {
            self.pending_paths.remove(&path);
        }
        for (watcher, mode) in ready_watchers {
            self.insert_started_watcher(watcher, mode);
        }
    }

    pub fn check_for_file_system_events(&mut self) -> BTreeSet<PathBuf> {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::DirectoryWatchers::check_for_file_system_events");
        let mut changed_directories = BTreeSet::new();
        for watched in self.watchers.values_mut() {
            while let Some(directories) = watched.watcher.try_recv_event() {
                changed_directories.extend(directories);
            }
        }
        changed_directories
    }
}

#[derive(Debug)]
pub struct DirectoryWatcher {
    watcher: RecommendedWatcher,
    receiver: Receiver<BTreeSet<PathBuf>>,
    current_path: Option<PathBuf>,
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

        // Spawn a thread to process raw notify events and convert them to our custom events
        let event_tx = tx;
        thread::spawn(move || {
            let mut pending_renames: HashMap<usize, BTreeSet<PathBuf>> = HashMap::new();

            for res in internal_rx {
                match res {
                    Ok(event) => {
                        let changed_dirs = Self::process_event(event, &mut pending_renames);
                        if changed_dirs.is_empty() {
                            continue;
                        }
                        if let Err(err) = event_tx.send(changed_dirs) {
                            eprintln!("Failed to send processed file system event: {err}");
                            break;
                        }
                    }
                    Err(e) => {
                        toast!(Error, "File system error: {e}");
                    }
                }
            }
        });

        Ok(Self {
            watcher,
            receiver: rx,
            current_path: None,
        })
    }

    pub fn watch_directory<P: AsRef<Path>>(&mut self, path: P, mode: RecursiveMode) -> Result<()> {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::watch_directory");
        let path = path.as_ref().to_path_buf();
        let path = normalize_path(&path);

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
    pub fn try_recv_event(&self) -> Option<BTreeSet<PathBuf>> {
        self.receiver.try_recv().ok()
    }

    #[allow(dead_code)]
    pub fn recv_event_timeout(&self, timeout: Duration) -> Option<BTreeSet<PathBuf>> {
        self.receiver.recv_timeout(timeout).ok()
    }

    fn process_event(
        event: Event,
        pending_renames: &mut HashMap<usize, BTreeSet<PathBuf>>,
    ) -> BTreeSet<PathBuf> {
        let tracker = event.attrs.tracker();
        let Event {
            kind,
            paths,
            attrs: _,
        } = event;

        match kind {
            EventKind::Create(CreateKind::File | CreateKind::Folder) => {
                parent_dirs_for_paths(paths)
            }
            EventKind::Remove(RemoveKind::File | RemoveKind::Folder) => {
                parent_dirs_for_paths(paths)
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                let directories = parent_dirs_for_paths(paths);
                if let Some(tracker) = tracker {
                    pending_renames
                        .entry(tracker)
                        .or_default()
                        .extend(directories.iter().cloned());
                    if pending_renames.len() > 256 {
                        pending_renames.clear();
                    }
                }
                directories
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
                let mut directories = parent_dirs_for_paths(paths);
                if let Some(tracker) = tracker
                    && let Some(pending) = pending_renames.remove(&tracker)
                {
                    directories.extend(pending);
                }
                directories
            }
            EventKind::Modify(ModifyKind::Name(_))
            | EventKind::Modify(ModifyKind::Data(DataChange::Content))
            | EventKind::Modify(ModifyKind::Data(_))
            | EventKind::Modify(ModifyKind::Metadata(_))
            | EventKind::Modify(ModifyKind::Any)
            | EventKind::Modify(ModifyKind::Other)
            | EventKind::Any => parent_dirs_for_paths(paths),
            _ => BTreeSet::new(),
        }
    }
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

impl Default for DirectoryWatcher {
    fn default() -> Self {
        Self::new().expect("Failed to create directory watcher")
    }
}

#[cfg(test)]
mod tests {
    use super::{DirectoryWatcher, parent_dir_for_event_path};
    use crate::helper::normalize_path;
    use notify::{
        Event, EventKind,
        event::{ModifyKind, RenameMode},
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
            attrs: Default::default(),
        };

        let mut pending = HashMap::new();
        let directories = DirectoryWatcher::process_event(event, &mut pending);

        assert_eq!(
            directories,
            BTreeSet::from([normalize_path(Path::new("/tmp"))])
        );
    }

    #[test]
    fn rename_mode_to_without_from_still_refreshes_parent() {
        let event = Event {
            kind: EventKind::Modify(ModifyKind::Name(RenameMode::To)),
            paths: vec![Path::new("/tmp/to.txt").into()],
            attrs: Default::default(),
        };

        let mut pending = HashMap::new();
        let directories = DirectoryWatcher::process_event(event, &mut pending);

        assert_eq!(
            directories,
            BTreeSet::from([normalize_path(Path::new("/tmp"))])
        );
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

        let from_directories = DirectoryWatcher::process_event(from, &mut pending);
        let to_directories = DirectoryWatcher::process_event(to, &mut pending);

        assert_eq!(
            from_directories,
            BTreeSet::from([normalize_path(Path::new("/tmp/source"))])
        );
        assert_eq!(
            to_directories,
            BTreeSet::from([
                normalize_path(Path::new("/tmp/source")),
                normalize_path(Path::new("/tmp/dest"))
            ])
        );
    }

    #[test]
    fn directory_event_invalidates_parent_directory() {
        let parent = parent_dir_for_event_path(Path::new("/tmp/example/subdir"));

        assert_eq!(parent.as_deref(), Some(Path::new("/tmp/example")));
    }
}
