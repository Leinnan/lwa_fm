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
use std::path::{Path, PathBuf};

impl MyTabViewer<'_> {
    /// Context menu for an item. It acts on the whole selection, which
    /// always includes the right-clicked item (right-click selects it first).
    #[allow(clippy::too_many_lines)]
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
                let single = paths.len() == 1;
                let is_dir = !val.is_file();
                if tab.current_path.is_trash() {
                    Self::trash_entry_menu(ui);
                    return;
                }

                if ui.button("Open").clicked() {
                    Self::activate_selection(tab, false);
                    ui.close();
                }
                if single && val.is_package() && ui.button("Show Package Contents").clicked() {
                    TabAction::ChangePaths(val.get_path().into()).schedule_tab(tab.id);
                    ui.close();
                }
                if single && is_dir {
                    if ui.button("Open in new tab").clicked() {
                        ActionToPerform::NewTab(val.get_path()).schedule();
                        ui.close();
                    }
                    if ui.button("Open in new window").clicked() {
                        ActionToPerform::NewWindow(val.get_path()).schedule();
                        ui.close();
                    }
                    if ui.button("Open in terminal").clicked() {
                        ActionToPerform::OpenInTerminal(val.get_path()).schedule();
                        ui.close();
                    }
                }
                #[cfg(any(windows, target_os = "macos"))]
                if single && !is_dir && ui.button("Open with…").clicked() {
                    crate::shell::open_with(val.get_path());
                    crate::app::open_history::opened(&val.get_path());
                    ui.close();
                }
                if single
                    && !is_dir
                    && crate::app::assets::kinds::media_kind(Path::new(val.file_name))
                        == Some(crate::app::assets::kinds::MediaKind::Video)
                {
                    if ui.button("Select thumbnail…").clicked() {
                        ActionToPerform::SelectThumbnail(Box::new(val.to_entry())).schedule();
                        ui.close();
                    }
                    if crate::app::thumbnail_choice::has(&val.get_path())
                        && ui.button("Reset thumbnail").clicked()
                    {
                        ActionToPerform::ResetThumbnail(val.get_path()).schedule();
                        ui.close();
                    }
                }
                if paths
                    .iter()
                    .any(|path| crate::app::open_history::get(path).is_some())
                    && ui.button("Remove from open history").clicked()
                {
                    crate::app::open_history::forget(paths.iter().map(PathBuf::as_path));
                    ui.close();
                }
                #[cfg(any(windows, target_os = "macos"))]
                if single && ui.button(crate::app::menus::reveal_label()).clicked() {
                    crate::shell::display_in_explorer(val.get_path()).unwrap_or_else(|error| {
                        toast!(Error, "{error}");
                    });
                    ui.close();
                }

                ui.separator();
                keymap::menu_button(ui, Command::Cut);
                keymap::menu_button(ui, Command::Copy);
                if single && is_dir && ui.button("Paste into folder").clicked() {
                    ActionToPerform::PasteInto(val.get_path()).schedule();
                    ui.close();
                }
                keymap::menu_button(ui, Command::CopyAsPath);

                ui.separator();
                keymap::menu_button(ui, Command::Rename);
                keymap::menu_button(ui, Command::Duplicate);
                let current = tab.current_path.single_path();
                let targets = self.transfer_targets(favorites, current.as_deref());
                ui.menu_button("Move to", |ui| transfer_menu(ui, &targets, &paths, true));
                ui.menu_button("Copy to", |ui| transfer_menu(ui, &targets, &paths, false));
                keymap::menu_button(ui, Command::Delete);
                keymap::menu_button(ui, Command::DeletePermanently);

                let mut entries: Vec<EntryRef<'_>> = tab
                    .selection
                    .rows()
                    .filter_map(|row| tab.entry_at(row))
                    .collect();
                if entries.is_empty() {
                    entries.push(*val);
                }
                crate::plugins::menu_items(
                    ui,
                    &crate::plugins::host_targets(entries.into_iter()),
                    current.as_deref(),
                    true,
                );

                if single && is_dir {
                    ui.separator();
                    let path = val.full_path_string();
                    if favorites.locations.iter().any(|f| f.path == path) {
                        if ui.button("Remove from favorites").clicked() {
                            ActionToPerform::RemoveFromFavorites(path.into()).schedule();
                            ui.close();
                        }
                    } else if ui.button("Add to favorites").clicked() {
                        ActionToPerform::AddToFavorites(path.into()).schedule();
                        ui.close();
                    }
                }

                #[cfg(any(windows, target_os = "macos"))]
                if single {
                    ui.separator();
                    if ui.button(crate::app::menus::properties_label()).clicked() {
                        crate::shell::open_properties(val.get_path());
                        ui.close();
                    }
                }
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
                TabAction::ChangePaths(path.into()).schedule_tab(tab_id);
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
}
