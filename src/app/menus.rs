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

/// Actions on a folder. `open` adds "Open" for places where clicking the
/// folder is not already the obvious way to open it.
pub fn folder_actions(ui: &mut Ui, path: &Path, favorites: &Locations, open: bool) {
    if open && ui.button("Open").clicked() {
        if let Some(action) = ActionToPerform::path_from_str(path.to_string_lossy(), false) {
            action.schedule();
        }
        ui.close();
    }
    if ui.button("Open in new tab").clicked() {
        ActionToPerform::NewTab(path.to_path_buf()).schedule();
        ui.close();
    }
    if ui.button("Open in new window").clicked() {
        ActionToPerform::NewWindow(path.to_path_buf()).schedule();
        ui.close();
    }
    if ui.button("Open in terminal").clicked() {
        ActionToPerform::OpenInTerminal(path.to_path_buf()).schedule();
        ui.close();
    }
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
    #[cfg(any(windows, target_os = "macos"))]
    {
        ui.separator();
        if ui.button(properties_label()).clicked() {
            crate::shell::open_properties(path);
            ui.close();
        }
    }
}
