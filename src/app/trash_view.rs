//! The Recycle Bin / Trash as a tab location: loading it, the sidebar row,
//! and the actions that make sense on its rows (restore, delete permanently,
//! empty).

use super::{
    App, BG_POOL, COMMANDS_QUEUE,
    commands::{ActionToPerform, ModalWindow, TabAction, TabTarget},
    directory_view_settings::DirectoryViewSettings,
    dock::{CurrentPath, TabData},
    file_ops::{AfterOp, PendingDelete},
};
use crate::fileops::{FileOp, trash_bin, trash_bin::TrashKey};
use crate::helper::DataHolder;
use crate::toast;
use egui::{Context, Ui};
use std::sync::{Arc, atomic::Ordering};

impl App {
    /// Lists the Recycle Bin on the background pool for `tab`.
    pub(crate) fn load_trash(tab: &mut TabData, ctx: &Context) {
        let tab_id = tab.id;
        let show_hidden = tab.show_hidden;
        let search = tab.search.clone();
        let settings = ctx.data_get_path_or_persisted::<DirectoryViewSettings>(&tab.current_path);
        tab.load = crate::app::dock::LoadState::loading(Some(format!(
            "Reading the {}…",
            trash_bin::LABEL
        )));
        tab.cancel_token.store(false, Ordering::SeqCst);
        let generation = tab.refresh_generation.load(Ordering::SeqCst);
        BG_POOL.spawn(move || {
            let listing = match trash_bin::listing() {
                Ok(listing) => listing,
                Err(error) => {
                    toast!(Error, "{error}");
                    crate::data::files::Listing::default()
                }
            };
            let listing = Arc::new(listing);
            let order = super::dir_handling::sort_order(&listing, &settings.data);
            let visible =
                super::dir_handling::filter_visible(&listing, &order, show_hidden, search.as_ref());
            COMMANDS_QUEUE.push(ActionToPerform::TabAction(
                TabTarget::TabWithId(tab_id),
                TabAction::FilesLoaded {
                    listing,
                    order,
                    visible,
                    generation,
                    linked_dirs: Vec::new(),
                },
            ));
        });
    }

    /// Shows the Recycle Bin in the active tab, or in a new one.
    pub(crate) fn open_trash(&mut self, ctx: &Context, new_tab: bool) {
        let tab_id = if new_tab {
            let start = self
                .tabs
                .get_current_path()
                .or_else(|| directories::UserDirs::new().map(|d| d.home_dir().to_path_buf()))
                .unwrap_or_default();
            Some(self.tabs.open_in_new_tab(&start))
        } else {
            self.tabs.get_current_index()
        };
        let Some(tab_id) = tab_id else {
            return;
        };
        if new_tab {
            // The new tab opens its start folder first; the Trash follows it.
            TabAction::ChangePaths(CurrentPath::Trash).schedule_tab(tab_id);
        } else {
            self.handle_action(
                ctx,
                ActionToPerform::TabAction(
                    TabTarget::TabWithId(tab_id),
                    TabAction::ChangePaths(CurrentPath::Trash),
                ),
            );
        }
    }

    /// Whether the active tab shows the Recycle Bin.
    pub(crate) fn in_trash_view(&mut self) -> bool {
        self.tabs
            .get_current_tab()
            .is_some_and(|tab| tab.current_path.is_trash())
    }

    /// The selected Recycle Bin rows of the active tab.
    fn selected_trash_keys(&mut self) -> Vec<TrashKey> {
        let Some(tab) = self.tabs.get_current_tab() else {
            return Vec::new();
        };
        if !tab.current_path.is_trash() {
            return Vec::new();
        }
        tab.selection
            .rows()
            .filter_map(|row| {
                tab.entry_at(row)
                    .map(|entry| crate::fileops::trash_bin::key_of(&entry))
            })
            .collect()
    }

    pub(crate) fn restore_trash_selection(&mut self, ctx: &Context) {
        let keys = self.selected_trash_keys();
        if !keys.is_empty() && trash_bin::supports_restore() {
            self.submit_file_op(ctx, FileOp::RestoreTrashed(keys), AfterOp::Nothing);
        }
    }

    /// Asks before deleting the selected Recycle Bin rows for good.
    pub(crate) fn purge_trash_selection(&mut self) {
        let keys = self.selected_trash_keys();
        if keys.is_empty() {
            return;
        }
        self.pending_delete = PendingDelete {
            paths: keys.iter().map(|key| key.path.clone()).collect(),
            permanent: true,
            trash: Some(keys),
            empty_trash: false,
        };
        self.display_modal = Some(ModalWindow::ConfirmDelete);
    }

    /// Asks before emptying the Recycle Bin.
    pub(crate) fn confirm_empty_trash(&mut self) {
        self.pending_delete = PendingDelete {
            paths: Vec::new(),
            permanent: true,
            trash: None,
            empty_trash: true,
        };
        self.display_modal = Some(ModalWindow::ConfirmDelete);
    }

    /// The sidebar row that opens the Recycle Bin; items dropped on it are trashed.
    pub(crate) fn trash_sidebar_row(ui: &mut Ui, active: bool) {
        let response = super::ui::nav_row(
            ui,
            lucide_icons::Icon::Trash2,
            trash_bin::LABEL,
            active,
            egui::Sense::click(),
        );
        let new_tab =
            response.middle_clicked() || (response.clicked() && ui.input(|i| i.modifiers.command));
        if response.clicked() || new_tab {
            ActionToPerform::OpenTrash { new_tab }.schedule();
        }
        if let Some(payload) = response.dnd_release_payload::<crate::app::file_ops::DraggedPaths>()
        {
            ActionToPerform::DropToTrash(payload.0.clone()).schedule();
        }
        response.context_menu(|ui| {
            if ui.button("Open in new tab").clicked() {
                ActionToPerform::OpenTrash { new_tab: true }.schedule();
            }
            if ui.button(format!("Empty {}…", trash_bin::LABEL)).clicked() {
                ActionToPerform::ConfirmEmptyTrash.schedule();
            }
        });
    }
}
