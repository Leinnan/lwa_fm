use std::path::{Path, PathBuf};
use std::sync::Weak;
use std::sync::mpsc::{Receiver, Sender, channel};

use crux_core::Request;
use crux_core::effects::ResolveSink;
use dirfleet_core::app::{LoadState, LoadedState, PersistedState};

/// Loads and saves the app's state as JSON in one file, off the caller's
/// thread. Without a file nothing is loaded or saved.
pub struct StorageHandler {
    load: Box<dyn Fn(Request<LoadState>) + Send + Sync>,
    saves: Option<Sender<PersistedState>>,
}

impl StorageHandler {
    pub fn new<R>(sink: Weak<R>, file: Option<PathBuf>) -> Self
    where
        R: ResolveSink<LoadState> + Send + Sync + 'static,
    {
        let saves = file.clone().map(|file| {
            let (saves, states) = channel();
            std::thread::Builder::new()
                .name("dirfleet_save".into())
                .spawn(move || write_latest(&file, &states))
                .expect("start the save thread");
            saves
        });
        let load = move |mut request: Request<LoadState>| {
            let file = file.clone();
            let sink = sink.clone();
            let spawned = std::thread::Builder::new()
                .name("dirfleet_load".into())
                .spawn(move || {
                    let loaded = file.as_deref().map_or(LoadedState::Missing, load);
                    if let Some(sink) = sink.upgrade()
                        && let Err(error) = sink.resolve_request(&mut request, loaded)
                    {
                        log::warn!("saved state not delivered: {error}");
                    }
                });
            if let Err(error) = spawned {
                log::warn!("could not start loading the saved state: {error}");
            }
        };
        Self {
            load: Box::new(load),
            saves,
        }
    }

    pub fn load(&self, request: Request<LoadState>) {
        (self.load)(request);
    }

    pub fn save(&self, state: PersistedState) {
        if let Some(saves) = &self.saves
            && saves.send(state).is_err()
        {
            log::warn!("save thread stopped");
        }
    }
}

/// The state saved in `file`.
pub fn load(file: &Path) -> LoadedState {
    match std::fs::read(file) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_or_else(
            |error| LoadedState::Failed(format!("{}: {error}", file.display())),
            LoadedState::Found,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => LoadedState::Missing,
        Err(error) => LoadedState::Failed(format!("{}: {error}", file.display())),
    }
}

/// Writes each state as it arrives, skipping to the newest when several wait.
fn write_latest(file: &Path, states: &Receiver<PersistedState>) {
    while let Ok(mut state) = states.recv() {
        while let Ok(newer) = states.try_recv() {
            state = newer;
        }
        if let Err(error) = save(file, &state) {
            log::warn!("could not save {}: {error}", file.display());
        }
    }
}

/// Replaces `file` with `state` in one step: a crash leaves the old file or
/// the new one, never half of one.
///
/// # Errors
/// When the folder can't be made or the file can't be written or replaced.
pub fn save(file: &Path, state: &PersistedState) -> std::io::Result<()> {
    let text = serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?;
    write_atomically(file, &text)
}

/// Writes `bytes` to `file` through a partial file, so a crash leaves the
/// old contents or the new ones.
pub fn write_atomically(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(folder) = file.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let partial = file.with_extension("json.partial");
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dirfleet_core::app::{PersistedWindow, STATE_VERSION};

    #[test]
    fn a_saved_state_loads_back_and_a_missing_file_is_missing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("nested").join("state.json");
        assert_eq!(load(&file), LoadedState::Missing);
        let state = PersistedState {
            version: STATE_VERSION,
            windows: vec![PersistedWindow {
                tabs: vec!["/a".into(), "/b".into()],
                active: 1,
                ..PersistedWindow::default()
            }],
            ..PersistedState::default()
        };
        save(&file, &state).expect("save");
        assert_eq!(load(&file), LoadedState::Found(state));
        assert!(!file.with_extension("json.partial").exists());
    }

    #[test]
    fn a_broken_file_fails_with_its_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("state.json");
        std::fs::write(&file, b"{ not json").expect("write");
        assert!(
            matches!(load(&file), LoadedState::Failed(message) if message.contains("state.json"))
        );
    }

    #[test]
    fn state_from_an_older_build_without_windows_loads() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("state.json");
        std::fs::write(&file, br#"{ "version": 0 }"#).expect("write");
        assert_eq!(
            load(&file),
            LoadedState::Found(PersistedState {
                version: 0,
                ..PersistedState::default()
            })
        );
    }
}
