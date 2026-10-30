//! The UI-thread side of plugins: which are loaded and running, the open
//! form, jobs, and the cached bar and inspector contents.

use super::discovery::{self, Broken};
use super::form::{FormState, Outcome, Values};
use super::jobs::{self, JobSpec};
use super::keymap::{self, PluginBinding};
use super::manifest::{FormMode, RunMode, TargetInfo};
use super::permissions::PermissionSet;
use super::runtime::{Request, Worker};
use super::{
    ActionEntry, BarSegment, CallContext, InspectorRow, JobEvent, PluginCode, PluginMsg, Registry,
    Source,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Jobs that may run at once, across plugins.
const MAX_JOBS: usize = 2;
/// How long a folder must be quiet before an edited plugin reloads.
const RELOAD_DEBOUNCE: Duration = Duration::from_millis(300);
/// Errors kept per plugin.
const MAX_ERRORS: usize = 20;

/// What the user chose for a plugin. Saved with the app settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginSettings {
    /// `None` means the default: on for bundled plugins, off for others.
    pub enabled: Option<bool>,
    /// Fingerprint of the permissions the user approved.
    pub approved: Option<u64>,
    /// Shortcut overrides by action id; empty text means none.
    pub shortcuts: BTreeMap<String, String>,
    /// Last form values by action id, for forms with `remember = true`.
    pub remembered: BTreeMap<String, Values>,
}

pub type SettingsMap = BTreeMap<String, PluginSettings>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Off,
    /// Enabled, but the permissions it asks for weren't approved.
    NeedsApproval,
    Loading,
    Running,
    Failed(String),
}

pub struct Plugin {
    pub code: Arc<PluginCode>,
    worker: Option<Worker>,
    /// A new version starting; it replaces `worker` once it loads.
    loading: Option<Worker>,
    pub status: Status,
    pub errors: VecDeque<String>,
    last_toast: Option<Instant>,
    /// `app_started` was sent to it.
    started: bool,
}

impl Plugin {
    const fn new(code: Arc<PluginCode>) -> Self {
        Self {
            code,
            worker: None,
            loading: None,
            status: Status::Off,
            errors: VecDeque::new(),
            last_toast: None,
            started: false,
        }
    }

    pub fn id(&self) -> &str {
        self.code.id()
    }

    pub fn permissions(&self) -> PermissionSet {
        PermissionSet::from_manifest(&self.code.manifest.permissions)
    }

    pub fn is_bundled(&self) -> bool {
        matches!(self.code.source, Source::Bundled)
    }

    pub fn enabled(&self, settings: &SettingsMap) -> bool {
        settings
            .get(self.id())
            .and_then(|settings| settings.enabled)
            .unwrap_or_else(|| self.is_bundled())
    }

    /// Bundled plugins and ones asking for nothing need no approval.
    pub fn approved(&self, settings: &SettingsMap) -> bool {
        let permissions = self.permissions();
        self.is_bundled()
            || permissions.is_empty()
            || settings
                .get(self.id())
                .and_then(|settings| settings.approved)
                == Some(permissions.fingerprint())
    }

    const fn running(&self) -> Option<&Worker> {
        self.worker.as_ref()
    }
}

/// The open form dialog.
pub struct OpenForm {
    plugin: String,
    action: String,
    pub title: String,
    context: CallContext,
    request: u64,
    pub state: Option<FormState>,
    pub error: Option<String>,
}

pub struct Job {
    pub id: u64,
    pub plugin: String,
    pub title: String,
    pub fraction: f32,
    pub label: String,
    pub cancel: Arc<AtomicBool>,
    per_item: bool,
    pub waiting: bool,
}

pub struct FinishedJob {
    pub summary: String,
    pub failures: Vec<String>,
    pub ok: bool,
    pub when: Instant,
}

#[derive(Debug, Default)]
pub struct BarState {
    pub segments: Vec<BarSegment>,
    pub error: Option<String>,
    pub loading: bool,
    fetched: Option<Instant>,
    stale: bool,
}

#[derive(Debug, Default)]
pub struct InspectorState {
    key: u64,
    pub rows: Vec<InspectorRow>,
    pub error: Option<String>,
    pub loading: bool,
    stale: bool,
}

/// A plugin's part of the directory bar.
pub struct BarView<'a> {
    pub plugin: &'a str,
    pub name: &'a str,
    pub state: &'a BarState,
}

/// A plugin's inspector section.
pub struct InspectorView<'a> {
    pub title: &'a str,
    pub state: &'a InspectorState,
}

#[derive(Default)]
pub struct PluginHost {
    pub dir: Option<PathBuf>,
    pub plugins: Vec<Plugin>,
    pub broken: Vec<Broken>,
    watcher: Option<(
        notify::RecommendedWatcher,
        std::sync::mpsc::Receiver<notify::Result<notify::Event>>,
    )>,
    dirty: BTreeMap<PathBuf, Instant>,
    next_id: u64,
    pub form: Option<OpenForm>,
    pub jobs: Vec<Job>,
    queued: VecDeque<JobSpec>,
    pub finished: VecDeque<FinishedJob>,
    bars: HashMap<(String, PathBuf), BarState>,
    activation: HashMap<(String, PathBuf), bool>,
    inspector: HashMap<String, InspectorState>,
    selection: Option<(u64, Instant, bool)>,
    registry_dirty: bool,
    /// Shortcut warnings by full action id, for the settings page.
    pub shortcut_notes: BTreeMap<String, String>,
}

impl PluginHost {
    /// Loads the bundled plugins and the plugins folder, and watches it.
    pub fn start() -> Self {
        let mut host = Self {
            dir: discovery::plugins_dir(),
            ..Self::default()
        };
        if let Some(dir) = host.dir.clone() {
            discovery::prepare_dir(&dir);
            host.watch(&dir);
        }
        host.load_all();
        host
    }

    /// A host with these plugins and no folder, for tests.
    #[cfg(test)]
    pub fn with_plugins(codes: Vec<Arc<PluginCode>>) -> Self {
        Self {
            plugins: codes.into_iter().map(Plugin::new).collect(),
            ..Self::default()
        }
    }

    fn watch(&mut self, dir: &Path) {
        use notify::Watcher as _;
        let (sender, receiver) = std::sync::mpsc::channel();
        let watcher = notify::recommended_watcher(move |event| {
            let _ = sender.send(event);
            crate::watcher::request_repaint();
        });
        match watcher {
            Ok(mut watcher) => match watcher.watch(dir, notify::RecursiveMode::Recursive) {
                Ok(()) => self.watcher = Some((watcher, receiver)),
                Err(error) => log::warn!("Can't watch {}: {error}", dir.display()),
            },
            Err(error) => log::warn!("Can't watch plugins: {error}"),
        }
    }

    /// (Re)reads every plugin. Running ones restart.
    pub fn load_all(&mut self) {
        self.plugins.clear();
        self.bars.clear();
        self.inspector.clear();
        self.activation.clear();
        let (user, broken) = self
            .dir
            .as_deref()
            .map(discovery::scan)
            .unwrap_or_default();
        self.broken = broken;
        for code in user {
            if self.plugins.iter().any(|plugin| plugin.id() == code.id()) {
                self.broken.push(Broken {
                    folder: match &code.source {
                        Source::User(folder) => folder.clone(),
                        Source::Bundled => PathBuf::new(),
                    },
                    diagnostic: super::manifest::Diagnostic {
                        file: super::manifest::MANIFEST_FILE.into(),
                        line: None,
                        message: format!("another plugin already has the id \"{}\"", code.id()),
                    },
                });
                continue;
            }
            self.plugins.push(Plugin::new(code));
        }
        // A user folder with a bundled plugin's id replaces it.
        for code in discovery::bundled() {
            if !self.plugins.iter().any(|plugin| plugin.id() == code.id()) {
                self.plugins.push(Plugin::new(code));
            }
        }
        self.plugins
            .sort_by(|a, b| a.code.manifest.name.cmp(&b.code.manifest.name));
        self.registry_dirty = true;
    }

    /// Reloads one plugin folder after it changed.
    fn reload_folder(&mut self, folder: &Path) {
        let existing = self.plugins.iter().position(
            |plugin| matches!(&plugin.code.source, Source::User(dir) if dir == folder),
        );
        self.broken.retain(|broken| broken.folder != folder);
        if !folder.join(super::manifest::MANIFEST_FILE).is_file() {
            // Removed: fall back to the bundled plugin of that id, if any.
            if existing.is_some() {
                self.load_all();
            }
            return;
        }
        match discovery::load_folder(folder) {
            Ok(code) => {
                let code = Arc::new(code);
                if let Some(index) = existing {
                    let plugin = &mut self.plugins[index];
                    if plugin.id() != code.id() {
                        self.load_all();
                        return;
                    }
                    plugin.code = code;
                    // A running plugin restarts; the old worker serves until
                    // the new one has loaded. One that failed gets a new try.
                    plugin.loading = None;
                    if matches!(plugin.status, Status::Failed(_)) {
                        plugin.status = Status::Off;
                        plugin.errors.clear();
                    }
                    if plugin.worker.is_some() {
                        plugin.status = Status::Loading;
                    }
                    let id = plugin.id().to_owned();
                    self.restart(&id);
                } else {
                    self.load_all();
                }
                crate::toast!(Info, "Plugin reloaded: {}", folder.display());
            }
            Err(diagnostic) => {
                let message = diagnostic.to_string();
                if let Some(index) = existing {
                    let id = self.plugins[index].id().to_owned();
                    self.report_error(&id, &format!("reload failed, still running the last version: {message}"));
                } else {
                    crate::toast!(Error, "Plugin in {} is broken: {message}", folder.display());
                }
                self.broken.push(Broken {
                    folder: folder.to_path_buf(),
                    diagnostic,
                });
            }
        }
    }

    /// Starts a new worker for `id` with its current code.
    fn restart(&mut self, id: &str) {
        self.next_id += 1;
        let generation = self.next_id;
        if let Some(plugin) = self.plugins.iter_mut().find(|plugin| plugin.id() == id)
            && plugin.status != Status::Off
            && plugin.status != Status::NeedsApproval
        {
            plugin.loading = Some(Worker::spawn(
                Arc::clone(&plugin.code),
                plugin.permissions(),
                generation,
            ));
            if plugin.worker.is_none() {
                plugin.status = Status::Loading;
            }
        }
    }

    /// Once a frame: reloads edited plugins, starts and stops workers to
    /// match the settings, starts waiting jobs and expires bar contents.
    pub fn poll(&mut self, settings: &SettingsMap) {
        self.poll_watcher();
        for index in 0..self.plugins.len() {
            let plugin = &mut self.plugins[index];
            let enabled = plugin.enabled(settings);
            let approved = plugin.approved(settings);
            let wanted = enabled && approved;
            if wanted {
                if plugin.worker.is_none() && plugin.loading.is_none() && !matches!(plugin.status, Status::Failed(_)) {
                    plugin.status = Status::Loading;
                    let id = plugin.id().to_owned();
                    self.restart(&id);
                }
            } else if plugin.worker.is_some()
                || plugin.loading.is_some()
                || !matches!(plugin.status, Status::Off | Status::NeedsApproval)
            {
                plugin.worker = None;
                plugin.loading = None;
                plugin.started = false;
                plugin.status = if enabled {
                    Status::NeedsApproval
                } else {
                    Status::Off
                };
                self.registry_dirty = true;
            } else {
                let status = if enabled {
                    Status::NeedsApproval
                } else {
                    Status::Off
                };
                if plugin.status != status {
                    plugin.status = status;
                }
            }
        }
        self.start_jobs();
        let now = Instant::now();
        for ((plugin, _), state) in &mut self.bars {
            let refresh = self
                .plugins
                .iter()
                .find(|p| p.id() == plugin)
                .and_then(|p| p.code.manifest.bar.as_ref())
                .and_then(|bar| bar.refresh_secs)
                .map(Duration::from_secs);
            if let (Some(refresh), Some(fetched)) = (refresh, state.fetched)
                && now.duration_since(fetched) >= refresh
            {
                state.stale = true;
            }
        }
        if self.registry_dirty {
            self.registry_dirty = false;
            self.publish_registry(settings);
        }
    }

    fn poll_watcher(&mut self) {
        let Some(dir) = self.dir.clone() else {
            return;
        };
        if let Some((_, receiver)) = &self.watcher {
            let now = Instant::now();
            for event in receiver.try_iter() {
                let Ok(event) = event else {
                    continue;
                };
                for path in event.paths {
                    if let Some(folder) = discovery::folder_of(&dir, &path) {
                        self.dirty.insert(folder, now);
                    }
                }
            }
        }
        let ready: Vec<PathBuf> = self
            .dirty
            .iter()
            .filter(|(_, at)| at.elapsed() >= RELOAD_DEBOUNCE)
            .map(|(folder, _)| folder.clone())
            .collect();
        if !self.dirty.is_empty() {
            crate::watcher::request_repaint();
        }
        for folder in ready {
            self.dirty.remove(&folder);
            self.reload_folder(&folder);
        }
    }

    /// Rebuilds the action list menus and shortcuts read.
    pub fn publish_registry(&mut self, settings: &SettingsMap) {
        let mut registry = Registry::default();
        self.shortcut_notes.clear();
        for plugin in &self.plugins {
            if plugin.running().is_none() {
                continue;
            }
            let overrides = settings.get(plugin.id()).map(|s| &s.shortcuts);
            for action in &plugin.code.manifest.actions {
                let full_id = format!("{}.{}", plugin.id(), action.id);
                let text = overrides
                    .and_then(|overrides| overrides.get(&action.id))
                    .cloned()
                    .or_else(|| action.shortcut.clone())
                    .filter(|text| !text.trim().is_empty());
                let shortcut = text.and_then(|text| match keymap::parse_shortcut(&text) {
                    Ok(shortcut) => {
                        if let Some(command) = keymap::conflict_with_app(shortcut) {
                            self.shortcut_notes
                                .insert(full_id.clone(), format!("{text} is used by \u{201c}{command}\u{201d}"));
                            None
                        } else if registry.bindings.iter().any(|b| b.shortcut == shortcut) {
                            self.shortcut_notes
                                .insert(full_id.clone(), format!("{text} is used by another plugin action"));
                            None
                        } else {
                            Some(shortcut)
                        }
                    }
                    Err(error) => {
                        self.shortcut_notes.insert(full_id.clone(), error);
                        None
                    }
                });
                if let Some(shortcut) = shortcut {
                    registry.bindings.push(PluginBinding {
                        shortcut,
                        action: full_id.clone(),
                    });
                }
                registry.actions.push(ActionEntry {
                    full_id,
                    plugin_name: plugin.code.manifest.name.clone(),
                    def: action.clone(),
                    shortcut,
                    activation: plugin.code.manifest.activation.clone(),
                });
            }
        }
        super::publish(registry);
    }

    /// Marks the shortcut table for rebuilding, after settings changed.
    pub const fn shortcuts_changed(&mut self) {
        self.registry_dirty = true;
    }

    fn plugin(&self, id: &str) -> Option<&Plugin> {
        self.plugins.iter().find(|plugin| plugin.id() == id)
    }

    fn plugin_mut(&mut self, id: &str) -> Option<&mut Plugin> {
        self.plugins.iter_mut().find(|plugin| plugin.id() == id)
    }

    /// Records an error for the settings page and, at most once a minute
    /// per plugin, shows it.
    pub fn report_error(&mut self, id: &str, message: &str) {
        log::warn!("[plugin {id}] {message}");
        let Some(plugin) = self.plugin_mut(id) else {
            return;
        };
        plugin.errors.push_back(message.to_owned());
        while plugin.errors.len() > MAX_ERRORS {
            plugin.errors.pop_front();
        }
        if plugin
            .last_toast
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(60))
        {
            plugin.last_toast = Some(Instant::now());
            let name = plugin.code.manifest.name.clone();
            crate::toast!(Error, "{name}: {message}");
        }
    }

    /// Splits `<plugin id>.<action id>` at the last dot.
    fn split_action(full_id: &str) -> Option<(&str, &str)> {
        full_id.rsplit_once('.')
    }

    /// Starts an action: shows its form, or runs it. Returns whether the
    /// form dialog should open.
    pub fn start_action(&mut self, full_id: &str, context: CallContext, settings: &SettingsMap) -> bool {
        let Some((plugin_id, action_id)) = Self::split_action(full_id) else {
            return false;
        };
        self.next_id += 1;
        let request = self.next_id;
        let Some(plugin) = self.plugin(plugin_id) else {
            crate::toast!(Error, "No plugin {plugin_id}");
            return false;
        };
        let Some(worker) = plugin.running() else {
            crate::toast!(Info, "{} isn't running", plugin.code.manifest.name);
            return false;
        };
        let Some(action) = plugin.code.action(action_id).cloned() else {
            return false;
        };
        if !action.targets.matches(&context.targets, context.dir.is_some()) {
            crate::toast!(Info, "\u{201c}{}\u{201d} doesn't apply to the selection", action.title);
            return false;
        }
        let mut form = OpenForm {
            plugin: plugin_id.to_owned(),
            action: action_id.to_owned(),
            title: action.title.clone(),
            context,
            request,
            state: None,
            error: None,
        };
        let remembered = settings
            .get(plugin_id)
            .and_then(|settings| settings.remembered.get(action_id));
        match action.form {
            FormMode::Dynamic => {
                worker.request(Request::Form {
                    request,
                    action: action_id.to_owned(),
                    context: form.context.clone(),
                });
            }
            FormMode::Static => {
                let static_form = super::form::Form {
                    title: Some(action.title.clone()),
                    remember: true,
                    fields: action.fields,
                    ..Default::default()
                };
                form.state = Some(FormState::new(static_form, remembered));
            }
            FormMode::None => {
                if let Some(text) = &action.confirm {
                    let confirm = super::form::Form {
                        title: Some(action.title.clone()),
                        submit: Some("Continue".into()),
                        fields: vec![serde_json::from_value(serde_json::json!({
                            "type": "info",
                            "text": text,
                        }))
                        .expect("an info field")],
                        ..Default::default()
                    };
                    form.state = Some(FormState::new(confirm, None));
                } else {
                    let context = form.context;
                    self.dispatch(plugin_id, action_id, context);
                    return false;
                }
            }
        }
        self.form = Some(form);
        true
    }

    /// Runs an action with its (validated) values.
    fn dispatch(&mut self, plugin_id: &str, action_id: &str, context: CallContext) {
        self.next_id += 1;
        let id = self.next_id;
        // The field, not `self.plugin()`, so `self.jobs` stays free.
        let Some(plugin) = self.plugins.iter().find(|plugin| plugin.id() == plugin_id) else {
            return;
        };
        let Some(action) = plugin.code.action(action_id) else {
            return;
        };
        match action.mode {
            RunMode::Quick => {
                if let Some(worker) = plugin.running() {
                    worker.request(Request::Run {
                        action: action_id.to_owned(),
                        context,
                    });
                }
            }
            RunMode::Job => {
                let cancel = Arc::new(AtomicBool::new(false));
                let mut title = action.title.trim_end_matches('…').to_owned();
                if context.targets.len() == 1 {
                    title = format!("{title}: {}", context.targets[0].name());
                } else if context.targets.len() > 1 {
                    title = format!("{title} ({} items)", context.targets.len());
                }
                self.jobs.push(Job {
                    id,
                    plugin: plugin_id.to_owned(),
                    title,
                    fraction: 0.0,
                    label: String::new(),
                    cancel: Arc::clone(&cancel),
                    per_item: action.per_item,
                    waiting: true,
                });
                self.queued.push_back(JobSpec {
                    id,
                    code: Arc::clone(&plugin.code),
                    permissions: plugin.permissions(),
                    action: action_id.to_owned(),
                    context,
                    per_item: action.per_item,
                    cancel,
                });
                self.start_jobs();
            }
        }
    }

    fn start_jobs(&mut self) {
        let mut index = 0;
        while index < self.queued.len() {
            let running = self.jobs.iter().filter(|job| !job.waiting).count();
            if running >= MAX_JOBS {
                return;
            }
            let spec = &self.queued[index];
            let plugin_id = spec.code.id().to_owned();
            let limit = spec.code.manifest.max_jobs;
            let mine = self
                .jobs
                .iter()
                .filter(|job| !job.waiting && job.plugin == plugin_id)
                .count();
            if mine >= limit {
                index += 1;
                continue;
            }
            let Some(spec) = self.queued.remove(index) else {
                return;
            };
            if spec.cancel.load(Ordering::Acquire) {
                continue;
            }
            if let Some(job) = self.jobs.iter_mut().find(|job| job.id == spec.id) {
                job.waiting = false;
            }
            jobs::spawn(spec);
        }
    }

    pub fn cancel_job(&mut self, id: u64) {
        if let Some(job) = self.jobs.iter().find(|job| job.id == id) {
            job.cancel.store(true, Ordering::Release);
            if job.waiting {
                self.queued.retain(|spec| spec.id != id);
                self.jobs.retain(|job| job.id != id);
            }
        }
    }

    /// Handles a message from a plugin thread. App requests are handled by
    /// the app before this.
    pub fn handle(&mut self, message: PluginMsg, settings: &mut SettingsMap) {
        match message {
            PluginMsg::Loaded {
                plugin,
                generation,
                result,
            } => self.loaded(&plugin, generation, result),
            PluginMsg::FormReady { request, result } => {
                let Some(form) = self.form.as_mut().filter(|form| form.request == request) else {
                    return;
                };
                match result {
                    Ok(built) => {
                        let remembered = settings
                            .get(&form.plugin)
                            .and_then(|settings| settings.remembered.get(&form.action));
                        form.state = Some(FormState::new(built, remembered));
                    }
                    Err(error) => form.error = Some(error),
                }
            }
            PluginMsg::Validated { request, result } => self.validated(request, result, settings),
            PluginMsg::Bar {
                plugin,
                dir,
                result,
            } => {
                let state = self.bars.entry((plugin.clone(), dir)).or_default();
                state.loading = false;
                state.fetched = Some(Instant::now());
                match result {
                    Ok(segments) => {
                        state.segments = segments;
                        state.error = None;
                    }
                    Err(error) => {
                        let first = state.error.is_none();
                        state.error = Some(error.clone());
                        if first {
                            self.report_error(&plugin, &format!("bar: {error}"));
                        }
                    }
                }
            }
            PluginMsg::Inspector { plugin, key, result } => {
                let state = self.inspector.entry(plugin.clone()).or_default();
                if state.key != key {
                    return;
                }
                state.loading = false;
                match result {
                    Ok(rows) => {
                        state.rows = rows;
                        state.error = None;
                    }
                    Err(error) => {
                        state.rows.clear();
                        state.error = Some(error.clone());
                        self.report_error(&plugin, &format!("inspector: {error}"));
                    }
                }
            }
            PluginMsg::ActionDone { plugin, result } => {
                self.invalidate_plugin(&plugin);
                if let Err(error) = result {
                    self.report_error(&plugin, &error);
                }
            }
            PluginMsg::Job { id, event } => self.job_event(id, event),
            PluginMsg::Invalidate {
                plugin,
                bar,
                inspector,
            } => {
                if bar {
                    for ((owner, _), state) in &mut self.bars {
                        if *owner == plugin {
                            state.stale = true;
                        }
                    }
                }
                if inspector && let Some(state) = self.inspector.get_mut(&plugin) {
                    state.stale = true;
                }
            }
            PluginMsg::Error { plugin, message } => self.report_error(&plugin, &message),
            PluginMsg::App(_) => {}
        }
    }

    fn loaded(&mut self, id: &str, generation: u64, result: Result<(), String>) {
        let Some(plugin) = self.plugin_mut(id) else {
            return;
        };
        if plugin
            .loading
            .as_ref()
            .is_none_or(|loading| loading.generation != generation)
        {
            return;
        }
        match result {
            Ok(()) => {
                plugin.worker = plugin.loading.take();
                plugin.status = Status::Running;
                let first = !plugin.started;
                plugin.started = true;
                let subscribed = plugin
                    .code
                    .manifest
                    .events
                    .subscribe
                    .iter()
                    .any(|event| event == "app_started");
                if first
                    && subscribed
                    && let Some(worker) = &plugin.worker
                {
                    worker.request(Request::Event {
                        name: "app_started".into(),
                        context: CallContext::default(),
                        data: serde_json::Value::Null,
                    });
                }
                self.bars.retain(|(owner, _), _| owner != id);
                self.inspector.remove(id);
                self.registry_dirty = true;
            }
            Err(error) => {
                plugin.loading = None;
                if plugin.worker.is_some() {
                    plugin.status = Status::Running;
                    self.report_error(id, &format!("reload failed, still running the last version: {error}"));
                } else {
                    plugin.status = Status::Failed(error.clone());
                    self.report_error(id, &error);
                }
            }
        }
    }

    /// Tries a plugin that failed to load again.
    pub fn retry(&mut self, id: &str) {
        if let Some(plugin) = self.plugin_mut(id) {
            plugin.status = Status::Off;
            plugin.errors.clear();
        }
    }

    fn validated(
        &mut self,
        request: u64,
        result: Result<BTreeMap<String, String>, String>,
        settings: &mut SettingsMap,
    ) {
        let Some(form) = self.form.as_mut().filter(|form| form.request == request) else {
            return;
        };
        let Some(state) = form.state.as_mut() else {
            return;
        };
        state.busy = false;
        match result {
            Ok(errors) if errors.is_empty() => {
                if state.form.remember {
                    settings
                        .entry(form.plugin.clone())
                        .or_default()
                        .remembered
                        .insert(form.action.clone(), state.values.clone());
                }
                let mut context = form.context.clone();
                context.values = Some(state.values.clone());
                let (plugin, action) = (form.plugin.clone(), form.action.clone());
                self.form = None;
                self.dispatch(&plugin, &action, context);
            }
            Ok(mut errors) => {
                state.message = errors.remove("");
                state.errors = errors;
            }
            Err(error) => state.message = Some(error),
        }
    }

    /// Draws the open form. Returns `true` once it is closed.
    pub fn form_ui(&mut self, ctx: &egui::Context) -> bool {
        let Some(form) = self.form.as_mut() else {
            return true;
        };
        let mut outcome = Outcome::Open;
        let modal = egui::Modal::new(egui::Id::new("plugin_form")).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 64.0).clamp(320.0, 520.0));
            if let Some(state) = form.state.as_mut() {
                outcome = state.ui(ui, &form.title);
            } else if let Some(error) = &form.error {
                ui.heading(&form.title);
                ui.colored_label(ui.visuals().error_fg_color, error);
                if ui.button("Close").clicked() {
                    outcome = Outcome::Cancel;
                }
            } else {
                ui.heading(&form.title);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Preparing…");
                });
                if ui.button("Cancel").clicked() {
                    outcome = Outcome::Cancel;
                }
            }
        });
        if modal.should_close() {
            outcome = Outcome::Cancel;
        }
        match outcome {
            Outcome::Open => false,
            Outcome::Cancel => {
                self.form = None;
                true
            }
            Outcome::Submit => {
                let Some(state) = form.state.as_mut() else {
                    return false;
                };
                match state.form.validate(&state.values) {
                    Ok(values) => {
                        state.values = values;
                        state.errors.clear();
                        state.busy = true;
                        let mut context = form.context.clone();
                        context.values = Some(state.values.clone());
                        let request = form.request;
                        let worker = self
                            .plugins
                            .iter()
                            .find(|plugin| plugin.id() == form.plugin)
                            .and_then(Plugin::running);
                        if let Some(worker) = worker {
                            worker.request(Request::Validate {
                                request,
                                action: form.action.clone(),
                                context,
                            });
                        } else {
                            state.busy = false;
                            state.message = Some("The plugin stopped running.".into());
                        }
                    }
                    Err(errors) => state.errors = errors,
                }
                false
            }
        }
    }

    fn job_event(&mut self, id: u64, event: JobEvent) {
        match event {
            JobEvent::Progress { fraction, label } => {
                if let Some(job) = self.jobs.iter_mut().find(|job| job.id == id) {
                    job.fraction = fraction;
                    job.label = label;
                }
            }
            JobEvent::Finished {
                done,
                skipped,
                failures,
                cancelled,
            } => {
                let Some(index) = self.jobs.iter().position(|job| job.id == id) else {
                    return;
                };
                let job = self.jobs.remove(index);
                let summary = job_summary(&job, done, skipped, &failures, cancelled);
                let ok = failures.is_empty();
                if cancelled {
                    crate::toast!(Info, "{summary}");
                } else if ok {
                    crate::toast!(Success, "{summary}");
                } else {
                    crate::toast!(Error, "{summary}");
                    for failure in &failures {
                        log::warn!("[plugin {}] {failure}", job.plugin);
                    }
                }
                self.finished.push_back(FinishedJob {
                    summary,
                    failures,
                    ok,
                    when: Instant::now(),
                });
                while self.finished.len() > 20 {
                    self.finished.pop_front();
                }
                self.invalidate_plugin(&job.plugin);
                let data = serde_json::json!({
                    "title": job.title,
                    "done": done,
                    "skipped": skipped,
                    "cancelled": cancelled,
                });
                self.emit("job_finished", &CallContext::default(), &data);
                self.start_jobs();
            }
        }
    }

    /// Sends an event to the running plugins subscribed to it.
    pub fn emit(&self, name: &str, context: &CallContext, data: &serde_json::Value) {
        for plugin in &self.plugins {
            if let Some(worker) = plugin.running()
                && plugin
                    .code
                    .manifest
                    .events
                    .subscribe
                    .iter()
                    .any(|event| event == name)
            {
                worker.request(Request::Event {
                    name: name.to_owned(),
                    context: context.clone(),
                    data: data.clone(),
                });
            }
        }
    }

    /// Notes the selection; returns `true` when it changed and then stayed
    /// put for a moment, so `selection_changed` should be sent.
    pub fn selection_settled(&mut self, key: u64) -> bool {
        let now = Instant::now();
        if let Some((current, since, sent)) = &mut self.selection
            && *current == key
        {
            if !*sent && now.duration_since(*since) >= Duration::from_millis(250) {
                *sent = true;
                return true;
            }
            if !*sent {
                crate::watcher::request_repaint();
            }
            return false;
        }
        // The selection at startup isn't a change.
        let first = self.selection.is_none();
        self.selection = Some((key, now, first));
        false
    }

    /// Whether any running plugin listens for `name`.
    pub fn wants(&self, name: &str) -> bool {
        self.plugins.iter().any(|plugin| {
            plugin.running().is_some()
                && plugin
                    .code
                    .manifest
                    .events
                    .subscribe
                    .iter()
                    .any(|event| event == name)
        })
    }

    /// Folders changed on disk: their bars and activation are stale.
    pub fn folders_changed(&mut self, dirs: &[PathBuf]) {
        if dirs.is_empty() {
            return;
        }
        for ((_, dir), state) in &mut self.bars {
            if dirs.iter().any(|changed| dir.starts_with(changed) || changed.starts_with(dir)) {
                state.stale = true;
            }
        }
        self.activation
            .retain(|(_, dir), _| !dirs.iter().any(|changed| changed == dir));
        for state in self.inspector.values_mut() {
            state.stale = true;
        }
    }

    fn invalidate_plugin(&mut self, id: &str) {
        for ((owner, _), state) in &mut self.bars {
            if owner == id {
                state.stale = true;
            }
        }
        if let Some(state) = self.inspector.get_mut(id) {
            state.stale = true;
        }
    }

    /// The bar contents for `dir`, asking plugins for anything missing or
    /// stale.
    pub fn bar_for(&mut self, dir: &Path) -> Vec<BarView<'_>> {
        let mut shown: Vec<String> = Vec::new();
        for plugin in &self.plugins {
            let (Some(worker), Some(_)) = (plugin.running(), plugin.code.manifest.bar.as_ref())
            else {
                continue;
            };
            let key = (plugin.id().to_owned(), dir.to_path_buf());
            let active = *self
                .activation
                .entry(key.clone())
                .or_insert_with(|| plugin.code.manifest.activation.matches(dir));
            if !active {
                continue;
            }
            let state = self.bars.entry(key).or_default();
            if !state.loading && (state.fetched.is_none() || state.stale) {
                state.loading = true;
                state.stale = false;
                worker.request(Request::Bar {
                    dir: dir.to_path_buf(),
                    context: CallContext {
                        dir: Some(dir.to_path_buf()),
                        ..Default::default()
                    },
                });
            }
            shown.push(plugin.id().to_owned());
        }
        let mut views = Vec::new();
        for id in &shown {
            let Some(plugin) = self.plugins.iter().find(|plugin| plugin.id() == id) else {
                continue;
            };
            if let Some(state) = self.bars.get(&(id.clone(), dir.to_path_buf())) {
                views.push(BarView {
                    plugin: plugin.id(),
                    name: &plugin.code.manifest.name,
                    state,
                });
            }
        }
        views
    }

    /// Inspector sections for the selected items.
    pub fn inspector_for(&mut self, context: &CallContext) -> Vec<InspectorView<'_>> {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::hash::DefaultHasher::new();
        for target in &context.targets {
            target.path.hash(&mut hasher);
            target.modified.hash(&mut hasher);
        }
        let key = hasher.finish();
        let mut shown = Vec::new();
        for plugin in &self.plugins {
            let (Some(worker), Some(section)) =
                (plugin.running(), plugin.code.manifest.inspector.as_ref())
            else {
                continue;
            };
            if !section.targets.matches(&context.targets, context.dir.is_some()) {
                continue;
            }
            // Search results span folders; the plugin checks those itself.
            if let Some(dir) = &context.dir {
                let active = *self
                    .activation
                    .entry((plugin.id().to_owned(), dir.clone()))
                    .or_insert_with(|| plugin.code.manifest.activation.matches(dir));
                if !active {
                    continue;
                }
            }
            let state = self.inspector.entry(plugin.id().to_owned()).or_default();
            if state.key != key || state.stale {
                if state.key != key {
                    state.rows.clear();
                    state.error = None;
                }
                state.key = key;
                state.stale = false;
                state.loading = true;
                worker.request(Request::Inspector {
                    key,
                    context: context.clone(),
                });
            }
            shown.push(plugin.id().to_owned());
        }
        let mut views = Vec::new();
        for id in shown {
            let Some(plugin) = self.plugins.iter().find(|plugin| plugin.id() == id) else {
                continue;
            };
            let (Some(section), Some(state)) =
                (plugin.code.manifest.inspector.as_ref(), self.inspector.get(&id))
            else {
                continue;
            };
            views.push(InspectorView {
                title: &section.title,
                state,
            });
        }
        views
    }

    /// Plugin actions whose shortcut was pressed.
    pub fn pressed(ctx: &egui::Context) -> Vec<String> {
        keymap::pressed(ctx, &super::registry().bindings)
    }

    pub const fn has_jobs(&self) -> bool {
        !self.jobs.is_empty()
    }
}

fn job_summary(job: &Job, done: usize, skipped: usize, failures: &[String], cancelled: bool) -> String {
    if cancelled {
        return format!("{}: cancelled", job.title);
    }
    if !job.per_item {
        return failures.first().map_or_else(
            || format!("{}: done", job.title),
            |error| format!("{} failed: {error}", job.title),
        );
    }
    let mut parts = vec![format!("{done} done")];
    if skipped > 0 {
        parts.push(format!("{skipped} skipped"));
    }
    if !failures.is_empty() {
        parts.push(format!("{} failed", failures.len()));
    }
    format!("{}: {}", job.title, parts.join(", "))
}

/// The selected items as plugin targets.
pub fn targets_from_entries<'a>(
    entries: impl Iterator<Item = crate::data::files::EntryRef<'a>>,
) -> Vec<TargetInfo> {
    entries
        .map(|entry| TargetInfo {
            path: entry.get_path(),
            is_dir: !entry.is_file(),
            size: entry.meta.size,
            modified: i64::from(*entry.meta.modified_at),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries() {
        let job = Job {
            id: 1,
            plugin: "p".into(),
            title: "Convert".into(),
            fraction: 0.0,
            label: String::new(),
            cancel: Arc::default(),
            per_item: true,
            waiting: false,
        };
        assert_eq!(job_summary(&job, 3, 1, &["x: bad".into()], false), "Convert: 3 done, 1 skipped, 1 failed");
        assert_eq!(job_summary(&job, 0, 0, &[], true), "Convert: cancelled");
    }

    #[test]
    fn selection_settles_after_a_pause() {
        let mut host = PluginHost::default();
        assert!(!host.selection_settled(1), "startup selection isn't a change");
        assert!(!host.selection_settled(2));
        if let Some((_, since, _)) = host.selection.as_mut() {
            *since -= Duration::from_secs(1);
        }
        assert!(host.selection_settled(2));
        assert!(!host.selection_settled(2), "sent once");
    }
}
