//! Menu entries shared by every place that shows a folder: the sidebar, the
//! breadcrumbs and the empty area of the file view. Keeping them in one
//! function makes the same folder offer the same actions everywhere.

use super::commands::ActionToPerform;
use crate::locations::Locations;
use egui::Ui;
use std::path::Path;

/// Label of the action that opens a folder in the system file manager.
pub const fn system_file_manager_label() -> &'static str {
    if cfg!(windows) {
        "Open in Explorer"
    } else if cfg!(target_os = "macos") {
        "Open in Finder"
    } else {
        "Open in file manager"
    }
}

/// Label of the action that selects an item in the system file manager.
pub const fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Show in Finder"
    } else {
        "Show in Explorer"
    }
}

/// Label of the action that opens the system's details window for an item.
pub const fn properties_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Get Info"
    } else {
        "Properties"
    }
}

/// Space left free above and below a menu capped by [`fit_to_window`].
const WINDOW_MARGIN: f32 = 16.0;

/// Caps a menu's height to the window, scrolling what doesn't fit. egui menus
/// don't scroll by themselves, so anything past the window edge is unreachable.
pub fn fit_to_window<R>(ui: &mut Ui, content: impl FnOnce(&mut Ui) -> R) -> R {
    let max_height = (ui.ctx().content_rect().height() - WINDOW_MARGIN).max(0.0);
    egui::ScrollArea::vertical()
        .max_height(max_height)
        .auto_shrink([true, true])
        .show(ui, content)
        .inner
}

/// The ways to open a folder somewhere other than the current tab.
pub fn open_folder_items(ui: &mut Ui, path: &Path, labels: [&str; 3]) {
    let [tab, window, terminal] = labels;
    if ui.button(tab).clicked() {
        ActionToPerform::NewTab(path.to_path_buf()).schedule();
        ui.close();
    }
    if ui.button(window).clicked() {
        ActionToPerform::NewWindow(path.to_path_buf()).schedule();
        ui.close();
    }
    if ui.button(terminal).clicked() {
        ActionToPerform::OpenInTerminal(path.to_path_buf()).schedule();
        ui.close();
    }
}

/// "Add to favorites", or "Remove from favorites" when it already is one.
pub fn favorite_toggle(ui: &mut Ui, path: &Path, favorites: &Locations) {
    let key = path.to_string_lossy();
    if favorites.locations.iter().any(|f| f.path == key) {
        if ui.button("Remove from favorites").clicked() {
            ActionToPerform::RemoveFromFavorites(key.into_owned().into()).schedule();
            ui.close();
        }
    } else if ui.button("Add to favorites").clicked() {
        ActionToPerform::AddToFavorites(key.into_owned().into()).schedule();
        ui.close();
    }
}

/// Actions on a folder. `open` adds "Open" for places where clicking the
/// folder is not already the obvious way to open it.
pub fn folder_actions(ui: &mut Ui, path: &Path, favorites: &Locations, open: bool) {
    if open && ui.button("Open").clicked() {
        if let Some(action) = ActionToPerform::path_from_str(path.to_string_lossy(), false) {
            action.schedule();
        }
        ui.close();
    }
    open_folder_items(
        ui,
        path,
        ["Open in new tab", "Open in new window", "Open in terminal"],
    );
    if ui.button(system_file_manager_label()).clicked() {
        // Opening a package would launch it; show it in Finder instead.
        if crate::fs_enum::is_package(path) {
            #[cfg(target_os = "macos")]
            if let Err(error) = crate::shell::display_in_explorer(path) {
                crate::toast!(Error, "{error}");
            }
        } else {
            ActionToPerform::SystemOpen(path.to_string_lossy().into_owned().into()).schedule();
        }
        ui.close();
    }
    ui.separator();
    if ui.button("Paste into folder").clicked() {
        ActionToPerform::PasteInto(path.to_path_buf()).schedule();
        ui.close();
    }
    if ui.button("Copy as path").clicked() {
        ui.ctx().copy_text(path.display().to_string());
        ui.close();
    }
    ui.separator();
    favorite_toggle(ui, path, favorites);
    #[cfg(any(windows, target_os = "macos"))]
    {
        ui.separator();
        if ui.button(properties_label()).clicked() {
            crate::shell::open_properties(path);
            ui.close();
        }
    }
}
