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

use crate::toast;

#[derive(Debug, Clone)]
pub enum FileSystemEvent {
    Created(PathBuf),
    Modified(PathBuf),
    Deleted(PathBuf),
    Renamed { from: PathBuf, to: PathBuf },
    Error(String),
}

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
        let Some(path) = watcher.current_path.as_ref().cloned() else {
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
            while let Some(event) = watched.watcher.try_recv_event() {
                match event {
                    FileSystemEvent::Created(path) => {
                        if let Some(file_name) = path.file_name() {
                            toast!(Info, "File created: {}", file_name.to_string_lossy());
                        }
                        if let Some(parent) = parent_dir_for_event_path(&path) {
                            _ = changed_directories.insert(parent);
                        }
                    }
                    FileSystemEvent::Modified(path) => {
                        if let Some(file_name) = path.file_name() {
                            toast!(Info, "File modified: {}", file_name.to_string_lossy());
                        }
                        if let Some(parent) = parent_dir_for_event_path(&path) {
                            _ = changed_directories.insert(parent);
                        }
                    }
                    FileSystemEvent::Deleted(path) => {
                        if let Some(file_name) = path.file_name() {
                            toast!(Warning, "File deleted: {}", file_name.to_string_lossy());
                        }
                        if let Some(parent) = parent_dir_for_event_path(&path) {
                            _ = changed_directories.insert(parent);
                        }
                    }
                    FileSystemEvent::Renamed { from, to } => {
                        if let (Some(from_name), Some(to_name)) = (from.file_name(), to.file_name())
                        {
                            toast!(
                                Info,
                                "File renamed: {} → {}",
                                from_name.to_string_lossy(),
                                to_name.to_string_lossy()
                            );
                        }
                        if let Some(parent) = parent_dir_for_event_path(&from) {
                            _ = changed_directories.insert(parent);
                        }
                        if let Some(parent) = parent_dir_for_event_path(&to) {
                            _ = changed_directories.insert(parent);
                        }
                    }
                    FileSystemEvent::Error(err) => {
                        toast!(Error, "File system error: {}", err);
                    }
                }
            }
        }
        changed_directories
    }
}

#[derive(Debug)]
pub struct DirectoryWatcher {
    watcher: RecommendedWatcher,
    receiver: Receiver<FileSystemEvent>,
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
            let mut rename_from: Option<PathBuf> = None;

            for res in internal_rx {
                match res {
                    Ok(event) => {
                        let fs_events = Self::process_event(event, &mut rename_from);
                        for fs_event in fs_events {
                            if let Err(e) = event_tx.send(fs_event) {
                                eprintln!("Failed to send processed file system event: {e}");
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        let _ = event_tx.send(FileSystemEvent::Error(e.to_string()));
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
    pub fn try_recv_event(&self) -> Option<FileSystemEvent> {
        self.receiver.try_recv().ok()
    }

    #[allow(dead_code)]
    pub fn recv_event_timeout(&self, timeout: Duration) -> Option<FileSystemEvent> {
        self.receiver.recv_timeout(timeout).ok()
    }

    fn process_event(event: Event, rename_from: &mut Option<PathBuf>) -> Vec<FileSystemEvent> {
        let mut events = Vec::new();

        match event.kind {
            EventKind::Create(CreateKind::File | CreateKind::Folder) => {
                for path in event.paths {
                    events.push(FileSystemEvent::Created(path));
                }
            }
            EventKind::Modify(ModifyKind::Data(DataChange::Content)) => {
                for path in event.paths {
                    events.push(FileSystemEvent::Modified(path));
                }
            }
            EventKind::Remove(RemoveKind::File | RemoveKind::Folder) => {
                for path in event.paths {
                    events.push(FileSystemEvent::Deleted(path));
                }
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                if let Some(path) = event.paths.into_iter().next() {
                    *rename_from = Some(path);
                }
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
                if let (Some(from), Some(to)) = (rename_from.take(), event.paths.into_iter().next())
                {
                    events.push(FileSystemEvent::Renamed { from, to });
                }
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
                if event.paths.len() >= 2 {
                    let mut paths = event.paths.into_iter();
                    if let (Some(from), Some(to)) = (paths.next(), paths.next()) {
                        events.push(FileSystemEvent::Renamed { from, to });
                    }
                }
            }
            _ => {
                // Handle other events or ignore them
            }
        }

        events
    }
}

fn parent_dir_for_event_path(path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        Some(path.to_path_buf())
    } else {
        path.parent().map(Path::to_path_buf)
    }
}

impl Default for DirectoryWatcher {
    fn default() -> Self {
        Self::new().expect("Failed to create directory watcher")
    }
}
