use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
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
    watchers: HashMap<PathBuf, DirectoryWatcher>,
    receivers: Option<Receiver<DirectoryWatcher>>,
}

impl DirectoryWatchers {
    #[inline]
    pub fn stop(&mut self, path: &PathBuf) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::DirectoryWatchers::stop");
        let Some(mut watcher) = self.watchers.remove(path) else {
            return;
        };
        std::thread::spawn(move || {
            watcher.stop_watching();
        });
    }

    pub fn start(&mut self, path: PathBuf) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::DirectoryWatchers::start");

        let (tx, rx) = mpsc::channel();

        std::thread::spawn(move || {
            let Ok(mut watcher) = DirectoryWatcher::new() else {
                return;
            };
            if let Err(err) = watcher.watch_directory(&path) {
                eprintln!("Failed to watch directory: {err}");
                return;
            }
            _ = tx.send(watcher);
        });
        self.receivers = Some(rx);
    }

    pub fn check_for_new_watchers(&mut self) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::DirectoryWatchers::check_for_new_watchers");
        let remove = if let Some(receiver) = &self.receivers
            && let Ok(watcher) = receiver.try_recv()
        {
            if let Some(path) = watcher.current_path.as_ref() {
                self.watchers.insert(path.clone(), watcher);
            }
            true
        } else {
            false
        };
        if remove {
            self.receivers = None;
        }
    }

    pub fn check_for_file_system_events(&mut self) -> bool {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::DirectoryWatchers::check_for_file_system_events");
        let mut should_refresh = false;
        for watcher in self.watchers.values_mut() {
            // Process all pending events
            while let Some(event) = watcher.try_recv_event() {
                should_refresh = true;
                match event {
                    FileSystemEvent::Created(path) => {
                        if let Some(file_name) = path.file_name() {
                            toast!(Info, "File created: {}", file_name.to_string_lossy());
                        }
                    }
                    FileSystemEvent::Modified(path) => {
                        if let Some(file_name) = path.file_name() {
                            toast!(Info, "File modified: {}", file_name.to_string_lossy());
                        }
                    }
                    FileSystemEvent::Deleted(path) => {
                        if let Some(file_name) = path.file_name() {
                            toast!(Warning, "File deleted: {}", file_name.to_string_lossy());
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
                    }
                    FileSystemEvent::Error(err) => {
                        toast!(Error, "File system error: {}", err);
                    }
                }
            }
        }
        should_refresh
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

    pub fn watch_directory<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::watch_directory");
        // self.stop_watching();
        let path = path.as_ref().to_path_buf();

        // Start watching the new path
        self.watcher
            .watch(&path, RecursiveMode::NonRecursive)
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

impl Default for DirectoryWatcher {
    fn default() -> Self {
        Self::new().expect("Failed to create directory watcher")
    }
}
