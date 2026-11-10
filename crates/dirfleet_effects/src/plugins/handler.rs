//! The core's plugin effects: loading plugins, a worker per running plugin,
//! and plugin threads' messages back to the core.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use crux_core::Request as CruxRequest;
use crux_core::effects::ResolveSink;
use dirfleet_core::app::{
    LoadPlugins, PluginControl, PluginFolderChanged, PluginMessages, PluginNotice, PluginsFound,
    WatchPlugins,
};

use super::jobs::{self, JobSpec};
use super::permissions::PermissionSet;
use super::runtime::Worker;
use super::{JobEvent, PluginCode, PluginMsg, Source, discovery};

/// How long a plugin folder rests after a change before it is reloaded.
const RELOAD_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(300);

/// Plugins as of the last load, and the workers of the running ones.
#[derive(Default)]
pub struct PluginHandler {
    codes: Mutex<HashMap<String, Arc<PluginCode>>>,
    workers: Mutex<HashMap<String, Worker>>,
    /// Each running job's Cancel flag.
    jobs: Mutex<HashMap<u64, Arc<AtomicBool>>>,
}

/// Every plugin: the user's folders first; a user plugin with a bundled
/// plugin's id replaces it, and two user plugins can't share an id.
fn find() -> PluginsFound {
    let (user, broken) = discovery::plugins_dir()
        .map(|dir| {
            discovery::prepare_dir(&dir);
            discovery::scan(&dir)
        })
        .unwrap_or_default();
    let mut broken: Vec<(String, String)> = broken
        .into_iter()
        .map(|broken| {
            (
                broken.folder.to_string_lossy().into_owned(),
                broken.diagnostic.to_string(),
            )
        })
        .collect();
    let mut plugins: Vec<Arc<PluginCode>> = Vec::new();
    for code in user {
        if plugins.iter().any(|plugin| plugin.id() == code.id()) {
            let folder = match &code.source {
                Source::User(folder) => folder.clone(),
                Source::Bundled => PathBuf::new(),
            };
            broken.push((
                folder.to_string_lossy().into_owned(),
                format!("another plugin already has the id \"{}\"", code.id()),
            ));
            continue;
        }
        plugins.push(code);
    }
    for code in discovery::bundled() {
        if !plugins.iter().any(|plugin| plugin.id() == code.id()) {
            plugins.push(code);
        }
    }
    let ffmpeg = plugins
        .iter()
        .any(|code| code.manifest.permissions.iter().any(|p| p == "ffmpeg"))
        .then(crate::media::video::installed_ffmpeg)
        .flatten()
        .map(|path| path.to_string_lossy().into_owned());
    PluginsFound {
        plugins,
        broken,
        ffmpeg,
    }
}

impl PluginHandler {
    /// Finds the plugins, on a thread of its own: it reads the plugin folders.
    pub fn load<R>(self: &Arc<Self>, mut request: CruxRequest<LoadPlugins>, sink: Weak<R>)
    where
        R: ResolveSink<LoadPlugins> + Send + Sync + 'static,
    {
        let handler = Arc::clone(self);
        std::thread::spawn(move || {
            let found = find();
            *handler.codes.lock().unwrap_or_else(PoisonError::into_inner) = found
                .plugins
                .iter()
                .map(|code| (code.id().to_owned(), Arc::clone(code)))
                .collect();
            if let Some(sink) = sink.upgrade() {
                let _ = sink.resolve_request(&mut request, found);
            }
        });
    }

    /// Starts, stops or asks a worker.
    pub fn control(&self, op: &PluginControl) {
        let mut workers = self.workers.lock().unwrap_or_else(PoisonError::into_inner);
        match op {
            PluginControl::Start { id, generation } => {
                let code = self
                    .codes
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(id)
                    .cloned();
                if let Some(code) = code {
                    let permissions = PermissionSet::from_manifest(&code.manifest.permissions);
                    // The old worker, if any, ends as it drops.
                    workers.insert(id.clone(), Worker::spawn(code, permissions, *generation));
                }
            }
            PluginControl::Stop { id } => {
                workers.remove(id);
            }
            PluginControl::Send { id, request } => {
                if let Some(worker) = workers.get(id) {
                    worker.request(request.clone());
                }
            }
            PluginControl::StartJob {
                job,
                id,
                action,
                context,
                per_item,
            } => {
                let code = self
                    .codes
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(id)
                    .cloned();
                let Some(code) = code else {
                    super::send(PluginMsg::Job {
                        id: *job,
                        event: JobEvent::Finished {
                            done: 0,
                            skipped: 0,
                            failures: vec![format!("no plugin {id}")],
                            cancelled: false,
                        },
                    });
                    return;
                };
                let cancel = Arc::new(AtomicBool::new(false));
                self.jobs
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(*job, Arc::clone(&cancel));
                jobs::spawn(JobSpec {
                    id: *job,
                    permissions: PermissionSet::from_manifest(&code.manifest.permissions),
                    code,
                    action: action.clone(),
                    context: context.clone(),
                    per_item: *per_item,
                    cancel,
                });
            }
            PluginControl::CancelJob { job } => {
                if let Some(cancel) = self
                    .jobs
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(job)
                {
                    cancel.store(true, Ordering::Release);
                }
            }
        }
    }

    /// Reports each plugin folder whose files change, once they rest for a
    /// moment, as egui's host does; for as long as the core is there.
    pub fn watch<R>(mut request: CruxRequest<WatchPlugins>, sink: Weak<R>)
    where
        R: ResolveSink<WatchPlugins> + Send + Sync + 'static,
    {
        use notify::Watcher as _;
        let Some(dir) = discovery::plugins_dir() else {
            return;
        };
        discovery::prepare_dir(&dir);
        // Events name real paths (`/private/var`, not `/var`).
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        let spawned = std::thread::Builder::new()
            .name("dirfleet_plugin_folder".into())
            .spawn(move || {
                let (sender, events) = std::sync::mpsc::channel();
                let watcher = notify::recommended_watcher(move |event| {
                    let _ = sender.send(event);
                })
                .and_then(|mut watcher| {
                    watcher.watch(&dir, notify::RecursiveMode::Recursive)?;
                    Ok(watcher)
                });
                let _watcher = match watcher {
                    Ok(watcher) => watcher,
                    Err(error) => {
                        log::warn!("Can't watch plugins: {error}");
                        return;
                    }
                };
                // How each folder looked, to tell real changes from events
                // about writes made before the watch began.
                let mut stamps: HashMap<PathBuf, _> = std::fs::read_dir(&dir)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|entry| entry.path())
                    .map(|folder| {
                        let stamp = discovery::stamp(&folder);
                        (folder, stamp)
                    })
                    .collect();
                let folders = |event: notify::Result<notify::Event>| -> Vec<PathBuf> {
                    event
                        .map(|event| event.paths)
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|path| discovery::folder_of(&dir, path))
                        .filter(|folder| {
                            !folder
                                .file_name()
                                .is_some_and(|name| name.to_string_lossy().starts_with('.'))
                        })
                        .collect()
                };
                while let Ok(event) = events.recv() {
                    let mut changed: Vec<PathBuf> = folders(event);
                    // A save is a burst of events: wait for it to end.
                    while let Ok(event) = events.recv_timeout(RELOAD_DEBOUNCE) {
                        changed.extend(folders(event));
                    }
                    changed.sort();
                    changed.dedup();
                    changed.retain(|folder| {
                        let now = discovery::stamp(folder);
                        stamps.insert(folder.clone(), now) != Some(now)
                    });
                    for folder in changed {
                        let Some(sink) = sink.upgrade() else {
                            return;
                        };
                        let folder = PluginFolderChanged(folder.to_string_lossy().into_owned());
                        if sink.resolve_request(&mut request, folder).is_err() {
                            return;
                        }
                    }
                }
            });
        if let Err(error) = spawned {
            log::warn!("Can't watch plugins: {error}");
        }
    }

    /// Sends plugin threads' messages to the core from now on; a finished
    /// job's Cancel flag goes.
    pub fn messages<R>(self: &Arc<Self>, request: CruxRequest<PluginMessages>, sink: Weak<R>)
    where
        R: ResolveSink<PluginMessages> + Send + Sync + 'static,
    {
        let request = Mutex::new(request);
        let handler = Arc::downgrade(self);
        super::set_sink(move |message| {
            if let PluginMsg::Job {
                id,
                event: JobEvent::Finished { .. },
            } = &message
                && let Some(handler) = handler.upgrade()
            {
                handler
                    .jobs
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(id);
            }
            let Some(sink) = sink.upgrade() else {
                return;
            };
            let mut request = request.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = sink.resolve_request(&mut request, PluginNotice(message));
        });
    }
}
