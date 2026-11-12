//! Lua plugins as the shared core sees them.
//!
//! What a plugin is ([`manifest`]), what it may do ([`permissions`]) and the
//! messages its threads send. The Lua runtime is `dirfleet_effects`'; the forms and tables a
//! plugin shows are `egui_mev_data_core`'s, with paths as their payloads.
//! See `docs/plugins.md` for the plugin author's view.

pub mod manifest;
pub mod permissions;
/// Where shortcuts are read: the app's commands use them too.
pub use crate::shortcut;

use std::collections::BTreeMap;
use std::path::PathBuf;

pub use egui_mev_data_core::form::{FieldDef, Form, Outcome, Values};
pub use manifest::TargetInfo;

/// A plugin's results: rows stand for folders, so their payload is a path.
pub type TableSpec = egui_mev_data_core::table::TableSpec<PathBuf>;
pub type RowUpdate = egui_mev_data_core::table::RowUpdate<PathBuf>;
pub type RowClick = egui_mev_data_core::table::RowClick<PathBuf>;
pub type Table = egui_mev_data_core::table::Table<PathBuf>;

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
    pub values: Option<Values>,
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
        spec: TableSpec,
    },
    /// A change to one row of the plugin's results.
    UpdateTableRow {
        plugin: String,
        update: RowUpdate,
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
        result: Result<Form, String>,
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

/// What a plugin's worker is asked to do, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Form {
        request: u64,
        action: String,
        context: CallContext,
    },
    Validate {
        request: u64,
        action: String,
        context: CallContext,
    },
    Run {
        action: String,
        context: CallContext,
    },
    Bar {
        dir: PathBuf,
        context: CallContext,
    },
    Inspector {
        key: u64,
        context: CallContext,
    },
    Event {
        name: String,
        context: CallContext,
        data: serde_json::Value,
    },
}

/// What the user chose for a plugin. Saved with the app settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
