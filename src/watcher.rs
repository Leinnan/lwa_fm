use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use notify::{
    event::{CreateKind, DataChange, ModifyKind, RemoveKind, RenameMode},
    Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
};

#[derive(Debug, Clone)]
pub enum FileSystemEvent {
    Created(PathBuf),
    Modified(PathBuf),
    Deleted(PathBuf),
    Renamed { from: PathBuf, to: PathBuf },
    Error(String),
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
        let path = path.as_ref().to_path_buf();

        // Stop watching the current path if there is one
        if let Some(current) = &self.current_path {
            let _ = self.watcher.unwatch(current);
        }

        // Start watching the new path
        self.watcher
            .watch(&path, RecursiveMode::NonRecursive)
            .with_context(|| format!("Failed to watch directory: {}", path.display()))?;

        self.current_path = Some(path);
        Ok(())
    }

    pub fn stop_watching(&mut self) {
        if let Some(path) = &self.current_path {
            let _ = self.watcher.unwatch(path);
            self.current_path = None;
        }
    }

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
