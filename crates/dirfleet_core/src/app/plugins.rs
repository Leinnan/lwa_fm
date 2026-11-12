//! Lua plugins, as egui's plugin host runs them: the plugins found (bundled
//! and the user's), which run (enabled, and their permissions approved),
//! their actions in the item menu, and what their threads report.
//!
//! The effects load plugins and keep a worker thread per running plugin;
//! the core says which run and what they are asked, and handles their
//! messages: errors, finished actions, and what plugins ask the app to do.
//! Forms are `plugin_forms`', jobs `plugin_jobs`', results dialogs
//! `plugin_results`', and bars, inspector sections and events
//! `plugin_views`'.
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use crux_core::Command;
use crux_core::capability::Operation;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::palette::CommandId;
use super::{DirFleet, Effect, Event, Model, SystemOp, Tab, ToastKind, tab_id};
use crate::fileops::Origin;
use crate::plugins::manifest::{ActionDef, Activation, TargetInfo, TargetKind};
use crate::plugins::permissions::PermissionSet;
use crate::plugins::{
    AppRequest, CallContext, NotifyLevel, PluginCode, PluginMsg, Request, SettingsMap, Source,
    Values,
};

/// Errors kept per plugin, as egui's.
const MAX_ERRORS: usize = 20;
/// A plugin's errors show as a toast at most this often, in seconds.
const ERROR_TOAST_SECS: u64 = 60;

/// Find the bundled plugins and the user's plugin folders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadPlugins;

/// What loading found. Opaque to facet: it never leaves the core.
#[derive(Clone, Debug, Facet)]
#[facet(opaque)]
pub struct PluginsFound {
    pub plugins: Vec<Arc<PluginCode>>,
    /// Folders that couldn't be loaded, and why.
    pub broken: Vec<(String, String)>,
    /// Where `FFmpeg` is, when a plugin may use it and it is installed.
    pub ffmpeg: Option<String>,
}

// Code isn't compared: a new load is always news.
impl PartialEq for PluginsFound {
    fn eq(&self, _: &Self) -> bool {
        false
    }
}
impl Eq for PluginsFound {}

impl Operation for LoadPlugins {
    type Output = PluginsFound;
}

/// Start, stop or ask a plugin's worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginControl {
    /// Start a worker for the plugin, as of the last load; its `Loaded`
    /// message carries `generation`.
    Start {
        id: String,
        generation: u64,
    },
    Stop {
        id: String,
    },
    Send {
        id: String,
        request: Request,
    },
    /// Run the action as job `job`, on a thread of its own.
    StartJob {
        job: u64,
        id: String,
        action: String,
        context: CallContext,
        per_item: bool,
    },
    CancelJob {
        job: u64,
    },
}

impl Operation for PluginControl {
    type Output = ();
}

/// Which of these plugins' activations hold in `dir`: what it (or a parent)
/// contains decides.
#[derive(Clone, Debug)]
pub struct CheckActivation {
    pub dir: String,
    pub plugins: Vec<(String, Activation)>,
}

/// The plugins whose activation holds. Opaque to facet: it never leaves the
/// core.
#[derive(Clone, Debug, PartialEq, Eq, Facet)]
#[facet(opaque)]
pub struct ActivatedPlugins(pub Vec<String>);

impl Operation for CheckActivation {
    type Output = ActivatedPlugins;
}

/// Report a plugin folder whose files changed, for as long as the app runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchPlugins;

/// The plugin folder that changed. Opaque to facet: it never leaves the core.
#[derive(Clone, Debug, PartialEq, Eq, Facet)]
#[facet(opaque)]
pub struct PluginFolderChanged(pub String);

impl Operation for WatchPlugins {
    type Output = PluginFolderChanged;
}

/// What plugin threads report, for as long as the app runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginMessages;

/// A message from a plugin thread. Opaque to facet: it never leaves the core.
#[derive(Clone, Debug, Facet)]
#[facet(opaque)]
pub struct PluginNotice(pub PluginMsg);

// Messages aren't compared: each one is news.
impl PartialEq for PluginNotice {
    fn eq(&self, _: &Self) -> bool {
        false
    }
}
impl Eq for PluginNotice {}

impl Operation for PluginMessages {
    type Output = PluginNotice;
}

/// Where a plugin stands.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Status {
    Off,
    /// Enabled, but the permissions it asks for weren't approved.
    NeedsApproval,
    Loading,
    Running,
    Failed(String),
}

struct Plugin {
    code: Arc<PluginCode>,
    status: Status,
    /// The worker started last; messages of older ones are dropped.
    generation: u64,
    errors: VecDeque<String>,
    /// When an error last showed as a toast, in Unix seconds.
    last_toast: Option<u64>,
}

impl Plugin {
    fn id(&self) -> &str {
        self.code.id()
    }

    fn permissions(&self) -> PermissionSet {
        PermissionSet::from_manifest(&self.code.manifest.permissions)
    }

    fn bundled(&self) -> bool {
        matches!(self.code.source, Source::Bundled)
    }

    fn enabled(&self, settings: &SettingsMap) -> bool {
        settings
            .get(self.id())
            .and_then(|settings| settings.enabled)
            .unwrap_or_else(|| self.bundled())
    }

    /// Bundled plugins and ones asking for nothing need no approval.
    fn approved(&self, settings: &SettingsMap) -> bool {
        let permissions = self.permissions();
        self.bundled()
            || permissions.is_empty()
            || settings
                .get(self.id())
                .and_then(|settings| settings.approved)
                == Some(permissions.fingerprint())
    }
}

#[derive(Default)]
pub(super) struct Plugins {
    list: Vec<Plugin>,
    broken: Vec<(String, String)>,
    ffmpeg: Option<String>,
    pub(super) settings: SettingsMap,
    next_generation: u64,
    /// The tab the last action ran in: what plugins' requests act on.
    last_tab: Option<u32>,
    /// The open form, and the last form or validation request's number.
    pub(super) form: Option<super::plugin_forms::OpenForm>,
    pub(super) next_request: u64,
    /// Running and waiting jobs, oldest first, and the last job's number.
    pub(super) jobs: Vec<super::plugin_jobs::Job>,
    pub(super) next_job: u64,
    /// Results dialogs, and the one shown.
    pub(super) panels: Vec<super::plugin_results::Panel>,
    pub(super) shown_panel: Option<String>,
    /// Jobs a result row's menu started, and jobs measuring a row again.
    pub(super) row_jobs: Vec<(u64, super::plugin_results::RowOrigin)>,
    pub(super) measure_jobs: Vec<(u64, super::plugin_results::RowOrigin)>,
    /// Which plugins' activations hold in a folder; `None` while it is
    /// checked.
    activation: HashMap<String, Option<Vec<String>>>,
    /// Their bars, inspector sections and events' bookkeeping.
    pub(super) views: super::plugin_views::PluginViews,
    /// The shortcuts the app's commands have, which no action may take;
    /// `None` while they are the ones the app starts with.
    command_shortcuts: Option<Vec<(crate::shortcut::Shortcut, CommandId)>>,
}

/// A plugin as Settings ▸ Plugins shows it.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PluginView {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub bundled: bool,
    pub enabled: bool,
    /// Enabled, and waiting for its permissions to be allowed.
    pub needs_approval: bool,
    /// "Off", "Waiting for approval", "Starting…", "Running" or "Failed: …".
    pub status: String,
    /// What it may do, in words.
    pub permissions: Vec<String>,
    /// The latest errors, oldest first.
    pub errors: Vec<String>,
    /// Its actions' titles.
    pub actions: Vec<String>,
    /// The shortcuts of its actions that can have one.
    pub shortcuts: Vec<PluginShortcutSetting>,
    /// For a plugin that may use `FFmpeg`: whether it is there; else "".
    pub ffmpeg: String,
    /// Where `FFmpeg` is, once found.
    pub ffmpeg_path: String,
}

/// A plugin folder that couldn't be loaded.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BrokenPluginView {
    pub folder: String,
    pub message: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct PluginsView {
    pub plugins: Vec<PluginView>,
    pub broken: Vec<BrokenPluginView>,
}

/// A plugin action the item menu offers for the selection.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PluginActionView {
    /// `<plugin id>.<action id>`, for `RunPluginAction`.
    pub id: String,
    pub title: String,
    pub plugin: String,
    /// Asked before it runs, when set.
    pub confirm: String,
    /// Its shortcut as the app writes them ("⌥⌘M"), or "".
    pub shortcut: String,
}

/// A plugin action bound to a shortcut, for the menu bar and Settings ▸
/// Keyboard.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct PluginShortcutView {
    /// `<plugin id>.<action id>`.
    pub action: String,
    pub title: String,
    pub plugin: String,
    /// As the app writes shortcuts.
    pub text: String,
    /// egui's name for the key ("M", "F5", "Up"), and its modifiers;
    /// `command` is Cmd on macOS, `control` the Mac's Control key.
    pub key: String,
    pub command: bool,
    pub alt: bool,
    pub shift: bool,
    pub control: bool,
}

impl PluginShortcutView {
    /// Whether the action's shortcut is `shortcut`.
    pub(super) fn holds(&self, shortcut: &crate::shortcut::Shortcut) -> bool {
        self.key == shortcut.key
            && self.command == shortcut.command
            && self.control == shortcut.control
            && self.alt == shortcut.alt
            && self.shift == shortcut.shift
    }
}

/// A plugin action's shortcut in Settings ▸ Plugins.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PluginShortcutSetting {
    pub action: String,
    pub title: String,
    /// The user's own, or the manifest's; "" for none.
    pub text: String,
    /// Why it isn't used ("⌘R is used by “Refresh”"), or "".
    pub note: String,
}

/// What a permission allows, in words.
fn describe(permission: &str) -> String {
    match permission {
        "fs.read" => "Read files and folders".to_owned(),
        "fs.write" => "Change, move and delete files".to_owned(),
        "ffmpeg" => "Use FFmpeg".to_owned(),
        "clipboard" => "Write to the clipboard".to_owned(),
        _ => permission
            .strip_prefix("process:")
            .map_or_else(|| permission.to_owned(), |program| format!("Run {program}")),
    }
}

impl Plugins {
    /// Tells which shortcuts the app's commands have now.
    pub(super) fn set_command_shortcuts(
        &mut self,
        taken: Vec<(crate::shortcut::Shortcut, CommandId)>,
    ) {
        self.command_shortcuts = Some(taken);
    }

    pub(super) fn view(&self) -> PluginsView {
        let (_, notes) = self.bindings();
        PluginsView {
            plugins: self
                .list
                .iter()
                .map(|plugin| {
                    let manifest = &plugin.code.manifest;
                    PluginView {
                        id: plugin.id().to_owned(),
                        name: manifest.name.clone(),
                        version: manifest.version.clone(),
                        author: manifest.author.clone(),
                        description: manifest.description.clone(),
                        bundled: plugin.bundled(),
                        enabled: plugin.enabled(&self.settings),
                        needs_approval: plugin.status == Status::NeedsApproval,
                        status: match &plugin.status {
                            Status::Off => "Off".to_owned(),
                            Status::NeedsApproval => "Waiting for approval".to_owned(),
                            Status::Loading => "Starting…".to_owned(),
                            Status::Running => "Running".to_owned(),
                            Status::Failed(error) => format!("Failed: {error}"),
                        },
                        permissions: manifest.permissions.iter().map(|p| describe(p)).collect(),
                        errors: plugin.errors.iter().cloned().collect(),
                        actions: manifest.actions.iter().map(|a| a.title.clone()).collect(),
                        shortcuts: manifest
                            .actions
                            .iter()
                            .filter(|action| action.palette || action.shortcut.is_some())
                            .map(|action| {
                                let full = format!("{}.{}", plugin.id(), action.id);
                                PluginShortcutSetting {
                                    action: action.id.clone(),
                                    title: action.title.clone(),
                                    text: self
                                        .shortcut_text(plugin.id(), &action.id, action)
                                        .unwrap_or_default(),
                                    note: notes.get(&full).cloned().unwrap_or_default(),
                                }
                            })
                            .collect(),
                        ffmpeg: if !manifest.permissions.iter().any(|p| p == "ffmpeg") {
                            String::new()
                        } else if self.ffmpeg.is_some() {
                            "Found".to_owned()
                        } else {
                            "Not installed yet: it downloads the first time it's needed".to_owned()
                        },
                        ffmpeg_path: self.ffmpeg.clone().unwrap_or_default(),
                    }
                })
                .collect(),
            broken: self
                .broken
                .iter()
                .map(|(folder, message)| BrokenPluginView {
                    folder: folder.clone(),
                    message: message.clone(),
                })
                .collect(),
        }
    }

    /// The running plugins' actions the item menu offers for `targets`.
    pub(super) fn item_actions(
        &self,
        targets: &[TargetInfo],
        has_dir: bool,
    ) -> Vec<PluginActionView> {
        if targets.is_empty() {
            return Vec::new();
        }
        self.list
            .iter()
            .filter(|plugin| plugin.status == Status::Running)
            .flat_map(|plugin| {
                plugin
                    .code
                    .manifest
                    .actions
                    .iter()
                    .filter(|action| {
                        action.context_menu.in_entry_menu()
                            && !action.results
                            && action.targets.matches(targets, has_dir)
                    })
                    .map(|action| self.action_view(plugin, action))
            })
            .collect()
    }

    /// The shortcut text of an action: the user's own, else the manifest's;
    /// `None` for none (an empty override takes the default away).
    fn shortcut_text(&self, plugin: &str, action: &str, def: &ActionDef) -> Option<String> {
        self.settings
            .get(plugin)
            .and_then(|settings| settings.shortcuts.get(action))
            .cloned()
            .or_else(|| def.shortcut.clone())
            .filter(|text| !text.trim().is_empty())
    }

    /// The running plugins' actions bound to shortcuts, and why the others
    /// with one aren't: it doesn't read as a shortcut, the app uses it, or an
    /// earlier action does. As egui's registry.
    pub(super) fn bindings(&self) -> (Vec<PluginShortcutView>, HashMap<String, String>) {
        let mut bound: Vec<PluginShortcutView> = Vec::new();
        let mut notes = HashMap::new();
        let commands = self.command_shortcuts.clone().unwrap_or_else(|| {
            CommandId::ALL
                .into_iter()
                .filter_map(|command| Some((command.default_shortcut()?, command)))
                .collect()
        });
        for plugin in self
            .list
            .iter()
            .filter(|plugin| plugin.status == Status::Running)
        {
            for action in &plugin.code.manifest.actions {
                let full = format!("{}.{}", plugin.id(), action.id);
                let Some(text) = self.shortcut_text(plugin.id(), &action.id, action) else {
                    continue;
                };
                let shortcut = match crate::plugins::shortcut::parse(&text) {
                    Ok(shortcut) => shortcut,
                    Err(error) => {
                        notes.insert(full, error);
                        continue;
                    }
                };
                let shown = shortcut.display();
                // Shortcuts are told apart by their keys, however written.
                let held = commands.iter().find(|(taken, _)| *taken == shortcut);
                if let Some((_, command)) = held {
                    notes.insert(
                        full,
                        format!("{shown} is used by \u{201c}{}\u{201d}", command.label()),
                    );
                    continue;
                }
                if bound.iter().any(|binding| binding.text == shown) {
                    notes.insert(full, format!("{shown} is used by another plugin action"));
                    continue;
                }
                bound.push(PluginShortcutView {
                    action: full,
                    title: action.title.clone(),
                    plugin: plugin.code.manifest.name.clone(),
                    text: shown,
                    key: shortcut.key.to_owned(),
                    command: shortcut.command,
                    alt: shortcut.alt,
                    shift: shortcut.shift,
                    control: shortcut.control,
                });
            }
        }
        (bound, notes)
    }

    /// An action as menus and the palette show it, with its shortcut.
    fn action_view(&self, plugin: &Plugin, action: &ActionDef) -> PluginActionView {
        let id = format!("{}.{}", plugin.id(), action.id);
        let shortcut = self
            .bindings()
            .0
            .into_iter()
            .find(|binding| binding.action == id)
            .map(|binding| binding.text)
            .unwrap_or_default();
        PluginActionView {
            id,
            title: action.title.clone(),
            plugin: plugin.code.manifest.name.clone(),
            confirm: action.confirm.clone().unwrap_or_default(),
            shortcut,
        }
    }

    /// A running plugin's action.
    pub(super) fn action(&self, plugin: &str, action: &str) -> Option<&ActionDef> {
        self.list
            .iter()
            .find(|state| state.id() == plugin && state.status == Status::Running)?
            .code
            .action(action)
    }

    /// The values a form of the action had last time.
    pub(super) fn remembered(&self, plugin: &str, action: &str) -> Option<&Values> {
        self.settings.get(plugin)?.remembered.get(action)
    }

    /// How many of the plugin's jobs may run at once.
    pub(super) fn max_jobs(&self, plugin: &str) -> usize {
        self.list
            .iter()
            .find(|state| state.id() == plugin)
            .map_or(1, |state| state.code.manifest.max_jobs)
    }

    /// The tab's selection's plugin actions; none without a selection.
    pub(super) fn item_actions_of(&self, tab: &Tab) -> Vec<PluginActionView> {
        // Without a plugin that has such actions the selection isn't looked at.
        if tab.selection.is_empty() || !self.has_item_actions() {
            return Vec::new();
        }
        self.item_actions(&tab.plugin_targets(), tab.path.is_some())
    }

    /// Whether a running plugin has an action for the entry menu.
    fn has_item_actions(&self) -> bool {
        self.list
            .iter()
            .filter(|plugin| plugin.status == Status::Running)
            .any(|plugin| {
                plugin
                    .code
                    .manifest
                    .actions
                    .iter()
                    .any(|action| action.context_menu.in_entry_menu() && !action.results)
            })
    }

    /// Whether a running plugin adds a section to the inspector.
    pub(super) fn has_sections(&self) -> bool {
        self.list.iter().any(|plugin| {
            plugin.status == Status::Running && plugin.code.manifest.inspector.is_some()
        })
    }

    /// The actions the palette offers for the tab, as egui's: those the
    /// manifest lets in the palette that apply to its selection, or to its
    /// folder where the plugin's activation holds.
    pub(super) fn palette_actions(&self, tab: &Tab) -> Vec<PluginActionView> {
        let targets = tab.plugin_targets();
        let targets = targets.as_slice();
        let dir = tab.path.as_deref();
        self.list
            .iter()
            .filter(|plugin| plugin.status == Status::Running)
            .flat_map(|plugin| {
                plugin
                    .code
                    .manifest
                    .actions
                    .iter()
                    .filter(move |action| {
                        let applies = if action.targets.kind == TargetKind::CurrentDir {
                            dir.is_some_and(|dir| self.active_in(plugin, dir))
                        } else {
                            action.targets.matches(targets, dir.is_some())
                        };
                        action.palette && applies
                    })
                    .map(|action| self.action_view(plugin, action))
            })
            .collect()
    }

    /// The tab's folder's plugin actions.
    pub(super) fn folder_actions_of(&self, tab: &Tab) -> Vec<PluginActionView> {
        tab.path
            .as_deref()
            .map(|dir| self.folder_actions(dir))
            .unwrap_or_default()
    }

    /// The running plugins' folder actions the empty area's menu offers in
    /// `dir`, where their activation holds.
    pub(super) fn folder_actions(&self, dir: &str) -> Vec<PluginActionView> {
        let checked = self.activation.get(dir).and_then(Option::as_ref);
        self.list
            .iter()
            .filter(|plugin| plugin.status == Status::Running)
            .filter(|plugin| {
                plugin.code.manifest.activation.everywhere()
                    || checked.is_some_and(|ids| ids.iter().any(|id| id == plugin.id()))
            })
            .flat_map(|plugin| {
                plugin
                    .code
                    .manifest
                    .actions
                    .iter()
                    .filter(|action| {
                        action.context_menu.in_background_menu()
                            && action.targets.kind == TargetKind::CurrentDir
                    })
                    .map(|action| self.action_view(plugin, action))
            })
            .collect()
    }

    /// A folder was read again: what it contains may have changed.
    pub(super) fn forget_activation(&mut self, dir: &str) {
        self.activation.remove(dir);
    }

    /// Whether the plugin's activation holds in `dir`, as far as known.
    fn active_in(&self, plugin: &Plugin, dir: &str) -> bool {
        plugin.code.manifest.activation.everywhere()
            || self
                .activation
                .get(dir)
                .and_then(Option::as_ref)
                .is_some_and(|ids| ids.iter().any(|id| id == plugin.id()))
    }

    /// The running plugins with a bar that apply in `dir`: id and name.
    pub(super) fn with_bar(&self, dir: &str) -> Vec<(String, String)> {
        self.list
            .iter()
            .filter(|plugin| {
                plugin.status == Status::Running
                    && plugin.code.manifest.bar.is_some()
                    && self.active_in(plugin, dir)
            })
            .map(|plugin| (plugin.id().to_owned(), plugin.code.manifest.name.clone()))
            .collect()
    }

    /// How often a plugin's bar is made again, if it says.
    pub(super) fn bar_refresh_secs(&self, id: &str) -> Option<u64> {
        self.list
            .iter()
            .find(|plugin| plugin.id() == id)?
            .code
            .manifest
            .bar
            .as_ref()?
            .refresh_secs
    }

    /// The running plugins with an inspector section for this selection:
    /// id and title. Search results span folders: the plugin checks those.
    pub(super) fn with_section(
        &self,
        dir: Option<&str>,
        targets: &[TargetInfo],
    ) -> Vec<(String, String)> {
        self.list
            .iter()
            .filter(|plugin| plugin.status == Status::Running)
            .filter_map(|plugin| {
                let section = plugin.code.manifest.inspector.as_ref()?;
                let applies = section.targets.matches(targets, dir.is_some())
                    && dir.is_none_or(|dir| self.active_in(plugin, dir));
                applies.then(|| (plugin.id().to_owned(), section.title.clone()))
            })
            .collect()
    }

    /// The running plugins subscribed to the event `name`.
    pub(super) fn subscribed(&self, name: &str) -> Vec<String> {
        self.list
            .iter()
            .filter(|plugin| {
                plugin.status == Status::Running
                    && plugin
                        .code
                        .manifest
                        .events
                        .subscribe
                        .iter()
                        .any(|event| event == name)
            })
            .map(|plugin| plugin.id().to_owned())
            .collect()
    }

    /// The running plugins whose folder actions, bar or inspector section
    /// need their activation looked at.
    fn needing_activation(&self) -> Vec<(String, Activation)> {
        self.list
            .iter()
            .filter(|plugin| {
                let manifest = &plugin.code.manifest;
                plugin.status == Status::Running
                    && !manifest.activation.everywhere()
                    && (manifest.bar.is_some()
                        || manifest.inspector.is_some()
                        || manifest.actions.iter().any(|action| {
                            action.context_menu.in_background_menu()
                                && action.targets.kind == TargetKind::CurrentDir
                        }))
            })
            .map(|plugin| {
                (
                    plugin.id().to_owned(),
                    plugin.code.manifest.activation.clone(),
                )
            })
            .collect()
    }

    pub(super) fn form_view(&self) -> Option<super::plugin_forms::PluginFormView> {
        self.form.as_ref().map(super::plugin_forms::OpenForm::view)
    }

    pub(super) fn job_views(&self) -> Vec<super::plugin_jobs::PluginJobView> {
        self.jobs
            .iter()
            .map(|job| {
                let mut view = job.view();
                view.results = self
                    .panels
                    .iter()
                    .find(|panel| panel.job == Some(job.id))
                    .map(super::plugin_results::Panel::key)
                    .unwrap_or_default();
                view
            })
            .collect()
    }

    fn get_mut(&mut self, id: &str) -> Option<&mut Plugin> {
        self.list.iter_mut().find(|plugin| plugin.id() == id)
    }

    /// Starts and stops workers to match the settings.
    fn sync(&mut self) -> Command<Effect, Event> {
        let mut commands = Vec::new();
        for plugin in &mut self.list {
            let enabled = plugin.enabled(&self.settings);
            let wanted = enabled && plugin.approved(&self.settings);
            match (&plugin.status, wanted) {
                (Status::Off | Status::NeedsApproval, true) => {
                    self.next_generation += 1;
                    plugin.generation = self.next_generation;
                    plugin.status = Status::Loading;
                    commands.push(control(PluginControl::Start {
                        id: plugin.id().to_owned(),
                        generation: plugin.generation,
                    }));
                }
                (Status::Loading | Status::Running | Status::Failed(_), false) => {
                    commands.push(control(PluginControl::Stop {
                        id: plugin.id().to_owned(),
                    }));
                    plugin.status = if enabled {
                        Status::NeedsApproval
                    } else {
                        Status::Off
                    };
                }
                (Status::Off | Status::NeedsApproval, false) => {
                    plugin.status = if enabled {
                        Status::NeedsApproval
                    } else {
                        Status::Off
                    };
                }
                _ => {}
            }
        }
        Command::all(commands)
    }
}

pub(super) fn control(op: PluginControl) -> Command<Effect, Event> {
    Command::notify_shell(op).into()
}

impl Tab {
    /// The selection (or the cursor's row) as plugins see it, worked out
    /// now; `plugin_targets` keeps it.
    pub(super) fn plugin_targets_now(&self) -> Vec<TargetInfo> {
        let rows: Vec<usize> = if self.selection.is_empty() {
            self.selection.cursor().into_iter().collect()
        } else {
            self.selection.rows().collect()
        };
        rows.into_iter()
            .filter_map(|row| {
                let entry = *self.order.get(row)? as usize;
                let meta = self.listing.meta(entry);
                Some(TargetInfo {
                    path: self.row_path(row)?,
                    is_dir: meta.entry_type == crate::listing::EntryType::Directory,
                    size: meta.size,
                    modified: i64::from(*meta.modified_at),
                })
            })
            .collect()
    }
}

impl DirFleet {
    /// Listens to plugin threads, and watches the plugins folder, from the
    /// start.
    pub(super) fn start_plugins() -> Command<Effect, Event> {
        Command::stream_from_shell(PluginMessages)
            .then_send(|notice| Event::PluginNoticed { notice })
            .and(
                Command::stream_from_shell(WatchPlugins)
                    .then_send(|changed| Event::PluginFolderChanged { folder: changed.0 }),
            )
    }

    /// Checks the activation of plugins with folder actions in each shown
    /// folder not checked yet; after every event, like thumbnails.
    pub(super) fn request_activation(model: &mut Model) -> Command<Effect, Event> {
        let plugins = model.plugins.needing_activation();
        if plugins.is_empty() {
            return Command::done();
        }
        let dirs: Vec<String> = model
            .windows
            .iter()
            .flat_map(|window| window.layout.shown())
            .filter_map(|id| model.tabs.get(&id)?.path.clone())
            .filter(|dir| !model.plugins.activation.contains_key(dir))
            .collect();
        let mut commands = Vec::new();
        for dir in dirs {
            if model.plugins.activation.contains_key(&dir) {
                continue;
            }
            model.plugins.activation.insert(dir.clone(), None);
            let asked = dir.clone();
            commands.push(
                Command::request_from_shell(CheckActivation {
                    dir,
                    plugins: plugins.clone(),
                })
                .then_send(move |found| Event::PluginsActivated {
                    dir: asked,
                    plugins: found,
                }),
            );
        }
        Command::all(commands)
    }

    /// Finds the plugins, once the settings are known.
    pub(super) fn load_plugins() -> Command<Effect, Event> {
        Command::request_from_shell(LoadPlugins).then_send(|found| Event::PluginsLoaded { found })
    }

    /// The events of plugins and Settings ▸ Plugins.
    pub(super) fn plugin_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        match event {
            Event::PluginsLoaded { found } => {
                let plugins = &mut model.plugins;
                let stops: Vec<_> = plugins
                    .list
                    .drain(..)
                    .filter(|plugin| matches!(plugin.status, Status::Loading | Status::Running))
                    .map(|plugin| {
                        control(PluginControl::Stop {
                            id: plugin.id().to_owned(),
                        })
                    })
                    .collect();
                plugins.list = found
                    .plugins
                    .into_iter()
                    .map(|code| Plugin {
                        code,
                        status: Status::Off,
                        generation: 0,
                        errors: VecDeque::new(),
                        last_toast: None,
                    })
                    .collect();
                plugins
                    .list
                    .sort_by(|a, b| a.code.manifest.name.cmp(&b.code.manifest.name));
                plugins.broken = found.broken;
                plugins.ffmpeg = found.ffmpeg;
                plugins.activation.clear();
                plugins.views.clear();
                Command::all(stops).and(plugins.sync()).and(render())
            }
            Event::ReloadPlugins => Self::load_plugins(),
            // As egui: an edited plugin is read again and restarts.
            Event::PluginFolderChanged { folder } => {
                let text = format!("Plugin reloaded: {folder}");
                Self::toast(model, ToastKind::Info, text).and(Self::load_plugins())
            }
            Event::SetPluginEnabled { id, enabled } => {
                model.plugins.settings.entry(id).or_default().enabled = Some(enabled);
                model.plugins.sync().and(render())
            }
            Event::SetPluginShortcut {
                plugin,
                action,
                text,
            } => {
                let default = model
                    .plugins
                    .list
                    .iter()
                    .find(|state| state.id() == plugin)
                    .and_then(|state| state.code.action(&action))
                    .and_then(|def| def.shortcut.clone())
                    .unwrap_or_default();
                let shortcuts = &mut model.plugins.settings.entry(plugin).or_default().shortcuts;
                if text == default {
                    shortcuts.remove(&action);
                } else {
                    shortcuts.insert(action, text);
                }
                render()
            }
            Event::ApprovePlugin { id } => {
                let Some(plugin) = model.plugins.list.iter().find(|plugin| plugin.id() == id)
                else {
                    return Command::done();
                };
                let fingerprint = plugin.permissions().fingerprint();
                model.plugins.settings.entry(id).or_default().approved = Some(fingerprint);
                model.plugins.sync().and(render())
            }
            Event::RunPluginAction { tab, action } => Self::run_plugin_action(model, tab, &action),
            Event::SetPluginFormField { .. }
            | Event::SubmitPluginForm
            | Event::CancelPluginForm => Self::plugin_form_event(model, event),
            Event::CancelPluginJob { id } => Self::cancel_plugin_job(model, id),
            Event::RunPluginResults
            | Event::StopPluginResults
            | Event::HidePluginResults
            | Event::ShowPluginResults { .. }
            | Event::SetPluginResultsField { .. }
            | Event::SortPluginResults { .. }
            | Event::FilterPluginResults { .. }
            | Event::OpenPluginResultsRow { .. }
            | Event::RefreshPluginResultsRow { .. }
            | Event::PluginResultsRowAction { .. } => Self::results_event(model, event),
            Event::PluginNoticed { notice } => Self::plugin_message(model, notice.0),
            Event::SelectionSettled { count } => Self::selection_settled(model, count),
            Event::PluginsActivated { dir, plugins } => {
                // Dropped when the folder was read again meanwhile.
                if let Some(entry) = model.plugins.activation.get_mut(&dir)
                    && entry.is_none()
                {
                    *entry = Some(plugins.0);
                }
                render()
            }
            _ => Command::done(),
        }
    }

    /// Runs a quick action on the tab's selection.
    fn run_plugin_action(model: &mut Model, tab: u32, full_id: &str) -> Command<Effect, Event> {
        let Some((plugin_id, action_id)) = full_id.rsplit_once('.') else {
            return Command::done();
        };
        let Some(state) = tab_id(tab).and_then(|id| model.tabs.get(&id)) else {
            return Command::done();
        };
        let mut targets = state.plugin_targets().as_ref().clone();
        let dir = state.path.clone().map(PathBuf::from);
        let Some(plugin) = model
            .plugins
            .list
            .iter()
            .find(|plugin| plugin.id() == plugin_id && plugin.status == Status::Running)
        else {
            return Command::done();
        };
        let Some(action) = plugin.code.action(action_id) else {
            return Command::done();
        };
        // A folder action acts on the folder shown, not the selection.
        if action.targets.kind == TargetKind::CurrentDir {
            targets.clear();
        }
        if !action.targets.matches(&targets, dir.is_some()) {
            let text = format!(
                "\u{201c}{}\u{201d} doesn't apply to the selection",
                action.title
            );
            return Self::toast(model, ToastKind::Info, text);
        }
        model.plugins.last_tab = Some(tab);
        let context = CallContext {
            dir,
            targets,
            tab: Some(tab),
            values: None,
        };
        Self::start_plugin_action(model, plugin_id, action_id, context, None)
    }

    /// Starts an action: its results dialog, its form, or the action itself.
    /// `row` is the result row it was asked for from, with the title its
    /// form takes.
    pub(super) fn start_plugin_action(
        model: &mut Model,
        plugin: &str,
        action: &str,
        context: CallContext,
        row: Option<(super::plugin_results::RowOrigin, Option<String>)>,
    ) -> Command<Effect, Event> {
        if model
            .plugins
            .action(plugin, action)
            .is_some_and(|def| def.results)
        {
            return Self::open_results(model, plugin, action, context);
        }
        if let Some(command) = Self::open_plugin_form(model, plugin, action, context.clone()) {
            if let (Some(form), Some((row, title))) = (model.plugins.form.as_mut(), row) {
                if let Some(title) = title {
                    form.title = title;
                }
                form.row = Some(row);
            }
            return command;
        }
        let (command, job) = Self::dispatch_plugin_action(model, plugin, action, context);
        if let (Some(job), Some((row, _))) = (job, row) {
            model.plugins.row_jobs.push((job, row));
        }
        command
    }

    /// Keeps a plugin's error for Settings and, at most once a minute, shows it.
    pub(super) fn plugin_error(
        model: &mut Model,
        id: &str,
        message: String,
    ) -> Command<Effect, Event> {
        let now = model.now;
        let Some(plugin) = model.plugins.get_mut(id) else {
            return Command::done();
        };
        let text = format!("{}: {message}", plugin.code.manifest.name);
        plugin.errors.push_back(message);
        while plugin.errors.len() > MAX_ERRORS {
            plugin.errors.pop_front();
        }
        let quiet = plugin
            .last_toast
            .is_some_and(|at| now.saturating_sub(at) < ERROR_TOAST_SECS);
        if quiet {
            return render();
        }
        plugin.last_toast = Some(now);
        Self::toast(model, ToastKind::Error, text)
    }

    fn plugin_message(model: &mut Model, message: PluginMsg) -> Command<Effect, Event> {
        match message {
            PluginMsg::Loaded {
                plugin,
                generation,
                result,
            } => {
                let Some(state) = model.plugins.get_mut(&plugin).filter(|state| {
                    state.generation == generation && state.status == Status::Loading
                }) else {
                    return Command::done();
                };
                match result {
                    Ok(()) => {
                        state.status = Status::Running;
                        model.plugins.activation.clear();
                        model.plugins.views.invalidate(&plugin, true, true);
                        // Told once that the app started, not on every restart.
                        if model.plugins.views.started.contains(&plugin) {
                            return render();
                        }
                        model.plugins.views.started.push(plugin.clone());
                        if !model.plugins.subscribed("app_started").contains(&plugin) {
                            return render();
                        }
                        render().and(control(PluginControl::Send {
                            id: plugin,
                            request: Request::Event {
                                name: "app_started".into(),
                                context: CallContext::default(),
                                data: serde_json::Value::Null,
                            },
                        }))
                    }
                    Err(error) => {
                        state.status = Status::Failed(error.clone());
                        Self::plugin_error(model, &plugin, error)
                    }
                }
            }
            PluginMsg::ActionDone { plugin, result } => {
                model.plugins.views.invalidate(&plugin, true, true);
                match result {
                    Ok(()) => render(),
                    Err(error) => Self::plugin_error(model, &plugin, error),
                }
            }
            PluginMsg::Error {
                plugin,
                message: error,
            } => Self::plugin_error(model, &plugin, error),
            PluginMsg::Bar {
                plugin,
                dir,
                result,
            } => Self::plugin_bar_made(model, &plugin, &dir, result),
            PluginMsg::Inspector {
                plugin,
                key,
                result,
            } => Self::plugin_section_made(model, &plugin, key, result),
            PluginMsg::Invalidate {
                plugin,
                bar,
                inspector,
            } => {
                model.plugins.views.invalidate(&plugin, bar, inspector);
                render()
            }
            PluginMsg::App(request) => Self::plugin_request(model, request),
            PluginMsg::FormReady { request, result } => {
                Self::plugin_form_ready(model, request, result)
            }
            PluginMsg::Validated { request, result } => {
                if model
                    .plugins
                    .panels
                    .iter()
                    .any(|panel| panel.form.request == request)
                {
                    Self::results_validated(model, request, result)
                } else {
                    Self::plugin_form_validated(model, request, result)
                }
            }
            PluginMsg::Job { id, event } => Self::plugin_job_event(model, id, event),
        }
    }

    /// Does what a plugin asked, in the tab its action ran in.
    fn plugin_request(model: &mut Model, request: AppRequest) -> Command<Effect, Event> {
        let tab = model
            .plugins
            .last_tab
            .filter(|tab| tab_id(*tab).is_some_and(|id| model.tabs.contains_key(&id)));
        let text = |path: PathBuf| path.to_string_lossy().into_owned();
        match request {
            AppRequest::Notify { level, text } => {
                let kind = match level {
                    NotifyLevel::Info => ToastKind::Info,
                    NotifyLevel::Success => ToastKind::Success,
                    NotifyLevel::Warning => ToastKind::Warning,
                    NotifyLevel::Error => ToastKind::Error,
                };
                Self::toast(model, kind, text)
            }
            AppRequest::Clipboard(copied) => Self::system(SystemOp::CopyText(copied)),
            AppRequest::SystemOpen(path) => Self::system(SystemOp::Open(vec![text(path)])),
            AppRequest::OpenTerminal(folder) => Self::system(SystemOp::Terminal {
                folder: text(folder),
                app: model.settings.terminal.clone(),
            }),
            AppRequest::OpenInNewTab(path) => Self::new_window(model, Some(text(path))),
            AppRequest::Open(path) => match tab {
                Some(tab) => Command::event(Event::Navigate {
                    tab,
                    path: text(path),
                }),
                None => Self::new_window(model, Some(text(path))),
            },
            AppRequest::Refresh => {
                tab.map_or_else(Command::done, |tab| Command::event(Event::Refresh { tab }))
            }
            AppRequest::Select(paths) => {
                let Some(state) = tab.and_then(tab_id).and_then(|id| model.tabs.get_mut(&id))
                else {
                    return Command::done();
                };
                state.select_after = paths;
                tab.map_or_else(Command::done, |tab| Command::event(Event::Refresh { tab }))
            }
            AppRequest::FileOp(op) => {
                Self::run(model, op, Origin::User, tab.and_then(tab_id), false)
            }
            AppRequest::ShowTable { plugin, job, spec } => {
                Self::results_table(model, &plugin, job, spec)
            }
            AppRequest::UpdateTableRow { plugin, update } => {
                Self::results_row(model, &plugin, update)
            }
        }
    }
}
