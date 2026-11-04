//! Lua plugins: folders with a `plugin.toml` and a `main.lua` that add
//! actions on files, forms, background jobs, a directory bar, inspector
//! sections and event hooks.
//!
//! Lua never runs on the UI thread. Each enabled plugin has a worker thread
//! owning its Lua state (`runtime`); jobs get a thread and a fresh state of
//! their own (`jobs`). Results come back through
//! [`COMMANDS_QUEUE`](crate::app::commands::COMMANDS_QUEUE) as
//! [`ActionToPerform::Plugin`](crate::app::commands::ActionToPerform::Plugin).
//! See `docs/plugins.md` for the plugin author's view.

mod api;
mod discovery;
pub mod form;
pub mod host;
mod jobs;
pub mod keymap;
pub mod manifest;
pub mod panel;
pub mod permissions;
mod runtime;
mod sandbox;
pub mod table;
#[cfg(test)]
mod tests;
pub mod ui;
pub mod usage;
#[cfg(target_os = "macos")]
mod usage_bulk;

pub use host::PluginHost;
pub use host::targets_from_entries as host_targets;
pub use manifest::TargetInfo;

use crate::app::commands::ActionToPerform;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, RwLock};

/// Where a plugin was loaded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Shipped inside the app.
    Bundled,
    /// A folder in the plugins directory.
    User(PathBuf),
}

/// A plugin's code: its manifest and Lua files, loaded once and shared by
/// its worker and jobs.
#[derive(Debug)]
pub struct PluginCode {
    pub manifest: manifest::Manifest,
    /// Lua sources by path relative to the plugin folder, with `/`.
    pub files: BTreeMap<String, String>,
    pub source: Source,
}

impl PluginCode {
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    pub fn action(&self, id: &str) -> Option<&manifest::ActionDef> {
        self.manifest.actions.iter().find(|action| action.id == id)
    }
}

/// What a handler is called with: the folder, the items and form values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallContext {
    /// The folder shown, when it is one folder.
    pub dir: Option<PathBuf>,
    pub targets: Vec<TargetInfo>,
    pub tab: Option<u32>,
    pub values: Option<form::Values>,
}

/// One segment of the directory bar.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BarSegment {
    pub text: Option<String>,
    /// A small rounded label, such as a count.
    pub badge: Option<String>,
    /// A button that runs this plugin's action `action`.
    pub button: Option<String>,
    pub action: Option<String>,
    pub icon: Option<String>,
    pub tooltip: Option<String>,
    /// `normal`, `muted`, `good`, `warn` or `bad`.
    pub tone: Option<String>,
    #[serde(default)]
    pub spinner: bool,
}

/// One row of a plugin's inspector section.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(untagged)]
pub enum InspectorRow {
    /// `{"Label", "value"}`
    Pair(String, String),
    Named {
        label: String,
        value: String,
    },
}

/// What a job reports while it runs.
#[derive(Debug, Clone)]
pub enum JobEvent {
    Progress {
        fraction: f32,
        label: String,
    },
    Finished {
        done: usize,
        skipped: usize,
        failures: Vec<String>,
        cancelled: bool,
    },
}

/// Something a plugin asks the app to do.
#[derive(Debug, Clone)]
pub enum AppRequest {
    Open(PathBuf),
    OpenInNewTab(PathBuf),
    Refresh,
    Select(Vec<PathBuf>),
    SystemOpen(PathBuf),
    OpenTerminal(PathBuf),
    FileOp(crate::fileops::FileOp),
    Clipboard(String),
    Notify {
        level: NotifyLevel,
        text: String,
    },
    /// A job's results, for the dialog of the run it belongs to.
    ShowTable {
        plugin: String,
        job: Option<u64>,
        spec: table::TableSpec,
    },
    /// A change to one row of the plugin's results.
    UpdateTableRow {
        plugin: String,
        update: table::RowUpdate,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyLevel {
    Info,
    Success,
    Warning,
    Error,
}

/// Messages from plugin threads to the UI thread.
#[derive(Debug, Clone)]
pub enum PluginMsg {
    /// A worker finished loading its code (or failed to).
    Loaded {
        plugin: String,
        generation: u64,
        result: Result<(), String>,
    },
    FormReady {
        request: u64,
        result: Result<form::Form, String>,
    },
    Validated {
        request: u64,
        result: Result<BTreeMap<String, String>, String>,
    },
    Bar {
        plugin: String,
        dir: PathBuf,
        result: Result<Vec<BarSegment>, String>,
    },
    Inspector {
        plugin: String,
        key: u64,
        result: Result<Vec<InspectorRow>, String>,
    },
    /// A quick action finished.
    ActionDone {
        plugin: String,
        result: Result<(), String>,
    },
    Job {
        id: u64,
        event: JobEvent,
    },
    App(AppRequest),
    Invalidate {
        plugin: String,
        bar: bool,
        inspector: bool,
    },
    Error {
        plugin: String,
        message: String,
    },
}

/// Hands a message to the UI thread and wakes it.
pub fn send(message: PluginMsg) {
    #[cfg(test)]
    if DISCARD_MESSAGES.get() {
        return;
    }
    ActionToPerform::Plugin(Box::new(message)).schedule();
    crate::watcher::request_repaint();
}

#[cfg(test)]
thread_local! {
    /// Set on a test's thread when it calls Lua directly, with no app to take
    /// the messages: the queue is shared, so another test would get them.
    pub static DISCARD_MESSAGES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// An action as menus, the palette and shortcuts see it.
#[derive(Debug, Clone)]
pub struct ActionEntry {
    /// `<plugin id>.<action id>`
    pub full_id: String,
    pub plugin_name: String,
    pub def: manifest::ActionDef,
    pub shortcut: Option<egui::KeyboardShortcut>,
    /// The plugin's activation, which folder actions also need.
    pub activation: manifest::Activation,
}

/// Whether an action applies to these items in `dir`. Folder actions
/// (`current_dir`) also need the plugin's activation to match the folder.
pub fn applies(
    action: &ActionEntry,
    targets: &[TargetInfo],
    dir: Option<&std::path::Path>,
) -> bool {
    action.def.targets.matches(targets, dir.is_some())
        && (action.def.targets.kind != manifest::TargetKind::CurrentDir
            || dir.is_some_and(|dir| action.activation.matches(dir)))
}

/// The actions of running plugins, readable from any drawing code.
#[derive(Debug, Default)]
pub struct Registry {
    pub actions: Vec<ActionEntry>,
    pub bindings: Vec<keymap::PluginBinding>,
}

static REGISTRY: LazyLock<RwLock<Arc<Registry>>> = LazyLock::new(RwLock::default);

/// The current actions; cheap to call every frame.
pub fn registry() -> Arc<Registry> {
    REGISTRY
        .read()
        .map_or_else(|_| Arc::default(), |registry| Arc::clone(&registry))
}

fn publish(registry: Registry) {
    if let Ok(mut current) = REGISTRY.write() {
        *current = Arc::new(registry);
    }
}

/// Draws the menu entries of the plugin actions that apply to `targets`,
/// for the entry menu (`entry`) or the folder's background menu.
pub fn menu_items(
    ui: &mut egui::Ui,
    targets: &[TargetInfo],
    dir: Option<&std::path::Path>,
    entry: bool,
) {
    let registry = registry();
    let actions: Vec<&ActionEntry> = registry
        .actions
        .iter()
        .filter(|action| {
            let place = action.def.context_menu;
            (if entry {
                place.in_entry_menu()
            } else {
                place.in_background_menu()
            }) && applies(action, if entry { targets } else { &[] }, dir)
        })
        .collect();
    if actions.is_empty() {
        return;
    }
    ui.separator();
    let draw = |ui: &mut egui::Ui| {
        for action in &actions {
            let mut button = ui::icon(action.def.icon.as_deref()).map_or_else(
                || egui::Button::new(&action.def.title),
                |icon| {
                    egui::Button::new((
                        crate::app::ui::glyph(icon).size(13.0),
                        action.def.title.as_str(),
                    ))
                },
            );
            if let Some(shortcut) = &action.shortcut {
                button = button.shortcut_text(ui.ctx().format_shortcut(shortcut));
            }
            if ui.add(button).on_hover_text(&action.plugin_name).clicked() {
                ActionToPerform::RunPluginAction {
                    action: action.full_id.clone(),
                    context: CallContext {
                        dir: dir.map(std::path::Path::to_path_buf),
                        targets: if entry { targets.to_vec() } else { Vec::new() },
                        tab: None,
                        values: None,
                    },
                }
                .schedule();
                ui.close();
            }
        }
    };
    if actions.len() > 5 {
        ui.menu_button("Plugins", |ui| draw(ui));
    } else {
        draw(ui);
    }
}

/// The plugin shortcuts, for the Keyboard settings page and the overlay.
pub fn shortcut_rows(ui: &mut egui::Ui) {
    let registry = registry();
    let rows: Vec<(&ActionEntry, &egui::KeyboardShortcut)> = registry
        .actions
        .iter()
        .filter_map(|action| action.shortcut.as_ref().map(|shortcut| (action, shortcut)))
        .collect();
    if rows.is_empty() {
        return;
    }
    let ctx = ui.ctx().clone();
    crate::app::ui::settings_group(ui, "Plugins", |group| {
        for (action, shortcut) in rows {
            group.row(&action.def.title, Some(&action.plugin_name), |ui| {
                crate::app::ui::key_caps(ui, &[ctx.format_shortcut(shortcut)]);
            });
        }
    });
}
