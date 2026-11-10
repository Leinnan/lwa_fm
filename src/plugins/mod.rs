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

pub mod form;
pub mod host;
pub mod keymap;
pub mod panel;
pub mod table;
#[cfg(test)]
mod tests;
pub mod ui;
pub use dirfleet_core::plugins::{
    AppRequest, BarSegment, CallContext, InspectorRow, JobEvent, NotifyLevel, PluginCode,
    PluginMsg, Source, manifest, permissions,
};
#[cfg(test)]
pub use dirfleet_effects::plugins::DISCARD_MESSAGES;
#[cfg(test)]
pub use dirfleet_effects::plugins::sandbox;
pub use dirfleet_effects::plugins::{discovery, jobs, runtime};
pub use dirfleet_effects::usage;

pub use host::PluginHost;
pub use host::targets_from_entries as host_targets;
pub use manifest::TargetInfo;

use crate::app::commands::ActionToPerform;
use dirfleet_effects::plugins::ActivationExt as _;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, RwLock};

/// Sends plugin threads' messages to the UI thread and wakes it, and points
/// plugins at this app's folders.
pub fn connect() {
    dirfleet_effects::plugins::configure(dirfleet_effects::plugins::PluginDirs {
        plugins: std::env::var_os("DIRFLEET_PLUGINS_DIR")
            .map(PathBuf::from)
            .or_else(|| crate::paths::project_dirs().map(|dirs| dirs.config_dir.join("plugins"))),
        data: crate::paths::project_dirs().map(|dirs| dirs.data_dir.join("plugin-data")),
    });
    dirfleet_effects::plugins::set_sink(|message| {
        ActionToPerform::Plugin(Box::new(message)).schedule();
        crate::watcher::request_repaint();
    });
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
    // The entry menu has less room to spare than the folder's background menu.
    let inline_limit = if entry { 2 } else { 5 };
    if actions.len() > inline_limit {
        ui.menu_button("Plugins", |ui| {
            crate::app::menus::fit_to_window(ui, |ui| draw(ui));
        });
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
