//! Per-entry actions shared by the list and grid views: context menu, activation, hover preview.
use super::{MyTabViewer, PopupOpened, TabData, popup_memory_id};
use crate::app::DisplayType;
use crate::app::assets::AssetManager;
use crate::app::assets::Preview;
use crate::app::assets::PreviewIntent;
use crate::app::commands::ActionToPerform;
use crate::app::commands::TabAction;
use crate::app::dock::TabId;
use crate::app::keymap::{self, Command};
use crate::app::top_bottom::sort_submenu;
use crate::data::files::EntryRef;
use crate::locations::Locations;
use crate::toast;
use egui::Id;
use egui::Ui;
use egui::Vec2;
use itertools::Itertools as _;
use std::path::{Path, PathBuf};

/// What the entry menu's sections act on.
struct EntryMenu<'a, 'e> {
    tab: &'a TabData,
    /// The right-clicked item.
    entry: &'a EntryRef<'e>,
    /// The whole selection, which always includes `entry`.
    paths: &'a [PathBuf],
    single: bool,
    is_dir: bool,
}

impl EntryMenu<'_, '_> {
    /// Ways to open the selection, or to find it elsewhere.
    fn open_section(&self, ui: &mut Ui) {
        if ui.button("Open").clicked() {
            MyTabViewer::activate_selection(self.tab, false);
            ui.close();
        }
        let path = self.entry.get_path();
        if self.single && self.entry.is_package() && ui.button("Show Package Contents").clicked() {
            TabAction::ChangePaths(crate::app::dock::existing_dir(path.clone()))
                .schedule_tab(self.tab.id);
            ui.close();
        }
        if self.single && self.is_dir {
            ui.menu_button("Open in", |ui| {
                crate::app::menus::open_folder_items(
                    ui,
                    &path,
                    ["New tab", "New window", "Terminal"],
                );
            });
        }
        #[cfg(any(windows, target_os = "macos"))]
        if self.single && !self.is_dir && ui.button("Open with…").clicked() {
            crate::shell::open_with(&path);
            crate::app::open_history::opened(&path);
            ui.close();
        }
        #[cfg(any(windows, target_os = "macos"))]
        if self.single && ui.button(crate::app::menus::reveal_label()).clicked() {
            crate::shell::display_in_explorer(&path).unwrap_or_else(|error| {
                toast!(Error, "{error}");
            });
            ui.close();
        }
    }

    /// The everyday edits, which also have shortcuts.
    fn clipboard_section(&self, ui: &mut Ui) {
        keymap::menu_button(ui, Command::Cut);
        keymap::menu_button(ui, Command::Copy);
        if self.single && self.is_dir && ui.button("Paste into folder").clicked() {
            ActionToPerform::PasteInto(self.entry.get_path()).schedule();
            ui.close();
        }
        keymap::menu_button(ui, Command::Rename);
        keymap::menu_button(ui, Command::Delete);
    }

    /// The rarer actions, kept in submenus.
    fn organize_section(&self, ui: &mut Ui, targets: &[PathBuf], favorites: &Locations) {
        if self.single
            && !self.is_dir
            && crate::app::assets::kinds::media_kind(Path::new(self.entry.file_name))
                == Some(crate::app::assets::kinds::MediaKind::Video)
        {
            let select = |ui: &mut Ui| {
                if ui.button("Select thumbnail…").clicked() {
                    ActionToPerform::SelectThumbnail(Box::new(self.entry.to_entry())).schedule();
                    ui.close();
                }
            };
            // A submenu only pays off once there is something to reset.
            let path = self.entry.get_path();
            if crate::app::thumbnail_choice::has(&path) {
                ui.menu_button("Thumbnail", |ui| {
                    select(ui);
                    if ui.button("Reset thumbnail").clicked() {
                        ActionToPerform::ResetThumbnail(path).schedule();
                        ui.close();
                    }
                });
            } else {
                select(ui);
            }
        }
        ui.menu_button("More", |ui| {
            ui.menu_button("Move to", |ui| transfer_menu(ui, targets, self.paths, true));
            ui.menu_button("Copy to", |ui| {
                transfer_menu(ui, targets, self.paths, false);
            });
            keymap::menu_button(ui, Command::Duplicate);
            keymap::menu_button(ui, Command::CopyAsPath);
            keymap::menu_button(ui, Command::DeletePermanently);
            if self
                .paths
                .iter()
                .any(|path| crate::app::open_history::get(path).is_some())
                && ui.button("Remove from open history").clicked()
            {
                crate::app::open_history::forget(self.paths.iter().map(PathBuf::as_path));
                ui.close();
            }
        });
        if self.single && self.is_dir {
            let path = PathBuf::from(self.entry.full_path_string());
            crate::app::menus::favorite_toggle(ui, &path, favorites);
        }
    }

    /// What the operating system offers for the selection. Draws its own
    /// separator, and nothing at all when there is nothing to offer.
    fn system_section(&self, ui: &mut Ui) {
        #[cfg(any(windows, target_os = "macos"))]
        if self.single {
            ui.separator();
            if ui.button(crate::app::menus::properties_label()).clicked() {
                crate::shell::open_properties(self.entry.get_path());
                ui.close();
            }
        }
        #[cfg(windows)]
        if shares_parent(self.paths) {
            if !self.single {
                ui.separator();
            }
            if ui.button("Windows context menu").clicked() {
                ActionToPerform::ShowSystemContextMenu(self.paths.to_vec()).schedule();
                ui.close();
            }
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        let _ = ui;
    }
}

/// Whether all the paths are in one folder, which the Windows shell needs to
/// build a menu for them together (search results can span folders).
#[cfg_attr(not(windows), allow(dead_code))]
fn shares_parent(paths: &[PathBuf]) -> bool {
    paths.iter().map(|path| path.parent()).all_equal()
}

impl MyTabViewer<'_> {
    /// Context menu for an item. It acts on the whole selection, which
    /// always includes the right-clicked item (right-click selects it first).
    pub(super) fn show_entry_context_menu(
        &self,
        row_response: &egui::Response,
        tab: &TabData,
        val: &EntryRef<'_>,
        row_index: usize,
        tab_popup_id: Id,
        favorites: &Locations,
    ) {
        let popup_id = tab_popup_id.with(row_index);
        egui::Popup::context_menu(row_response)
            .id(popup_id)
            .show(|ui| {
                ui.data_mut(|data| {
                    data.insert_temp(popup_memory_id(tab.id), PopupOpened(popup_id, row_index));
                });
                let mut paths: Vec<PathBuf> = tab
                    .selection
                    .rows()
                    .filter_map(|row| tab.entry_at(row).map(|entry| entry.get_path()))
                    .collect();
                if paths.is_empty() {
                    paths.push(val.get_path());
                }
                if tab.current_path.is_trash() {
                    Self::trash_entry_menu(ui);
                    return;
                }
                let menu = EntryMenu {
                    tab,
                    entry: val,
                    single: paths.len() == 1,
                    is_dir: !val.is_file(),
                    paths: &paths,
                };
                let current = tab.current_path.single_path();
                let targets = self.transfer_targets(favorites, current.as_deref());
                let mut entries: Vec<EntryRef<'_>> = tab
                    .selection
                    .rows()
                    .filter_map(|row| tab.entry_at(row))
                    .collect();
                if entries.is_empty() {
                    entries.push(*val);
                }
                let plugin_targets = crate::plugins::host_targets(entries.into_iter());

                crate::app::menus::fit_to_window(ui, |ui| {
                    menu.open_section(ui);
                    ui.separator();
                    menu.clipboard_section(ui);
                    ui.separator();
                    menu.organize_section(ui, &targets, favorites);
                    crate::plugins::menu_items(ui, &plugin_targets, current.as_deref(), true);
                    menu.system_section(ui);
                });
            });
    }

    /// Folders offered by "Move to" / "Copy to": the other open tabs and the
    /// favorites, without the folder being shown.
    fn transfer_targets(&self, favorites: &Locations, current: Option<&Path>) -> Vec<PathBuf> {
        let mut targets: Vec<PathBuf> = Vec::new();
        for path in self.tab_paths.iter().cloned().chain(favorites.paths()) {
            if Some(path.as_path()) != current && !targets.contains(&path) {
                targets.push(path);
            }
        }
        targets
    }

    /// Context menu for the empty area of the view: actions on the folder
    /// being shown rather than on its items.
    pub(super) fn show_background_menu(
        response: &egui::Response,
        tab: &TabData,
        favorites: &Locations,
    ) {
        response.context_menu(|ui| {
            ui.menu_button("View", |ui| {
                keymap::menu_toggle(ui, Command::ViewList, tab.display_type == DisplayType::List);
                keymap::menu_toggle(
                    ui,
                    Command::ViewGrid,
                    tab.display_type == DisplayType::Icons,
                );
            });
            sort_submenu(ui, tab);
            keymap::menu_toggle(ui, Command::ToggleHidden, tab.show_hidden);
            keymap::menu_button(ui, Command::Refresh);
            ui.separator();
            if tab.current_path.is_trash() {
                if ui
                    .button(format!("Empty {}…", crate::fileops::trash_bin::LABEL))
                    .clicked()
                {
                    ActionToPerform::ConfirmEmptyTrash.schedule();
                    ui.close();
                }
                return;
            }
            keymap::menu_button(ui, Command::Paste);
            ui.add_enabled_ui(tab.current_path.single_path().is_some(), |ui| {
                ui.menu_button("New", |ui| {
                    keymap::menu_button(ui, Command::NewFolder);
                    if ui.button("Text document").clicked() {
                        if let Some(parent) = tab.current_path.single_path() {
                            ActionToPerform::FileOp(crate::fileops::FileOp::NewFile { parent })
                                .schedule();
                        }
                        ui.close();
                    }
                });
            });
            keymap::menu_button(ui, Command::Undo);
            let Some(path) = tab.current_path.single_path() else {
                return;
            };
            crate::plugins::menu_items(ui, &[], Some(&path), false);
            ui.separator();
            crate::app::menus::folder_actions(ui, &path, favorites, false);
        });
    }

    /// The row menu in the Recycle Bin view.
    fn trash_entry_menu(ui: &mut Ui) {
        if crate::fileops::trash_bin::supports_restore() && ui.button("Restore").clicked() {
            ActionToPerform::RestoreTrashSelection.schedule();
            ui.close();
        }
        keymap::menu_button(ui, Command::DeletePermanently);
        ui.separator();
        if ui
            .button(format!("Empty {}…", crate::fileops::trash_bin::LABEL))
            .clicked()
        {
            ActionToPerform::ConfirmEmptyTrash.schedule();
            ui.close();
        }
    }

    pub(super) fn activate_entry(entry: &EntryRef<'_>, tab_id: TabId, open_in_new_tab: bool) {
        if entry.is_file() {
            ActionToPerform::SystemOpen(entry.full_path_string().into()).schedule();
        } else if let Ok(path) = std::fs::canonicalize(entry.get_path()) {
            if open_in_new_tab {
                ActionToPerform::NewTab(path).schedule();
            } else {
                TabAction::ChangePaths(crate::app::dock::existing_dir(path)).schedule_tab(tab_id);
            }
        }
    }

    /// Tooltip of a list row: the preview, then the exact dates the columns
    /// show as "3 days ago".
    pub(super) fn show_row_tooltip(
        assets: &mut AssetManager,
        ui: &mut Ui,
        entry: &EntryRef<'_>,
        in_trash: bool,
    ) {
        Self::show_entry_hover_preview(assets, ui, entry);
        let modified = if in_trash { "Deleted" } else { "Modified" };
        for (label, timestamp) in [
            (modified, entry.meta.modified_at),
            ("Created", entry.meta.created_at),
            (
                "Opened",
                crate::app::open_history::last_opened(
                    crate::app::open_history::of_entry(entry),
                    entry.meta.accessed_at,
                ),
            ),
        ] {
            if *timestamp != 0 {
                ui.small(format!(
                    "{label}: {}",
                    crate::app::inspector::format_date(timestamp)
                ));
            }
        }
    }

    pub(super) fn show_entry_hover_preview(
        assets: &mut AssetManager,
        ui: &mut Ui,
        entry: &EntryRef<'_>,
    ) {
        match assets.request_preview(ui.ctx(), entry, PreviewIntent::Hovered) {
            Preview::Ready(texture) => {
                ui.add(
                    egui::Image::new(&texture)
                        .maintain_aspect_ratio(true)
                        .max_size(Vec2::new(300.0, 300.0)),
                );
            }
            Preview::Pending => {
                if let Some(texture) = assets.request_entry_texture(entry) {
                    ui.add(
                        egui::Image::new(&texture)
                            .maintain_aspect_ratio(true)
                            .max_size(Vec2::new(300.0, 300.0)),
                    );
                }
                ui.label("Loading preview...");
                ui.label(entry.full_path_string());
            }
            Preview::Unavailable {
                reason,
                retry_after,
            } => {
                if let Some(texture) = assets.request_entry_texture(entry) {
                    ui.add(
                        egui::Image::new(&texture)
                            .maintain_aspect_ratio(true)
                            .max_size(Vec2::new(300.0, 300.0)),
                    );
                }
                ui.label("Preview unavailable");
                ui.small(reason);
                if let Some(retry_after) = retry_after {
                    ui.small(format!(
                        "Retrying in {} seconds",
                        retry_after.as_secs().max(1)
                    ));
                }
                ui.label(entry.full_path_string());
            }
            Preview::Fallback | Preview::Paused => {
                ui.label(entry.full_path_string());
            }
        }
    }
}

fn transfer_menu(ui: &mut Ui, targets: &[PathBuf], paths: &[PathBuf], moving: bool) {
    crate::app::menus::fit_to_window(ui, |ui| {
        if ui.button("Choose folder…").clicked() {
            ActionToPerform::ChooseTransferTarget {
                srcs: paths.to_vec(),
                moving,
            }
            .schedule();
            ui.close();
        }
        if !targets.is_empty() {
            ui.separator();
        }
        for target in targets {
            let name = target.file_name().map_or_else(
                || target.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            if ui
                .button(name)
                .on_hover_text(target.display().to_string())
                .clicked()
            {
                ActionToPerform::Transfer {
                    srcs: paths.to_vec(),
                    dst: target.clone(),
                    moving,
                }
                .schedule();
                ui.close();
            }
        }
    });
}
