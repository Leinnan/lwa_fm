//! Saved searches in the UI: the sidebar section, the save / edit dialog,
//! running, updating and reverting. Every change to the list is undoable.

use super::commands::{ActionToPerform, ModalWindow, TabAction, TabTarget};
use super::dock::CurrentPath;
use super::search::{MatchMode, SavedScope, SavedSearch, SavedSearches, Search, query};
use super::undo::UndoItem;
use super::{App, ui};
use crate::app::dock::TabId;
use crate::fileops::Origin;
use crate::helper::DataHolder;
use crate::locations::Locations;
use crate::toast;
use crate::widgets::chip_input::{ChipContext, chip_input};
use egui::{Context, Ui};
use lucide_icons::Icon;
use std::path::PathBuf;

/// Where to run a saved search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunTarget {
    ActiveTab,
    Tab(TabId),
    NewTab,
}

/// Drag payload for reordering saved searches in the sidebar.
#[derive(Debug, Clone, Copy)]
struct DraggedSavedSearch(u64);

/// State of the save / edit dialog.
#[derive(Debug, Clone)]
pub struct SavedSearchEditor {
    /// `None` saves a new search.
    id: Option<u64>,
    name: String,
    query: Search,
    scope: SavedScope,
    new_folder: String,
    /// Run the search in the active tab after saving a new one.
    attach_to_tab: Option<TabId>,
    focused: bool,
}

pub fn load_saved(ctx: &Context) -> SavedSearches {
    ctx.data_get_persisted::<SavedSearches>()
        .unwrap_or_default()
        .normalized()
}

impl App {
    /// Changes the saved searches and records the change for undo.
    pub(crate) fn update_saved_searches(
        &mut self,
        ctx: &Context,
        label: &str,
        change: impl FnOnce(&mut Vec<SavedSearch>),
    ) {
        let before = load_saved(ctx).searches;
        let mut after = before.clone();
        change(&mut after);
        if before == after {
            return;
        }
        ctx.data_set_persisted(SavedSearches {
            searches: after.clone(),
        });
        self.file_ops.undo.record(
            Origin::User,
            UndoItem::SavedSearches {
                before,
                after,
                label: label.to_owned(),
            },
        );
    }

    /// Opens the folder(s) of a saved search with its query.
    pub(crate) fn run_saved_search(&mut self, ctx: &Context, id: u64, target: RunTarget) {
        let Some(saved) = load_saved(ctx).get(id).cloned() else {
            toast!(Info, "That saved search no longer exists.");
            return;
        };
        let tab_id = match target {
            RunTarget::ActiveTab => self.tabs.get_current_tab().map(|tab| tab.id),
            RunTarget::Tab(id) => Some(id),
            RunTarget::NewTab => {
                let start = match &saved.scope {
                    SavedScope::Folders(paths) => paths.first().cloned(),
                    _ => None,
                }
                .or_else(|| self.tabs.get_current_path());
                if let Some(path) = start {
                    let new_id = self.tabs.open_in_new_tab(&path);
                    // Runs after the new tab has opened its folder.
                    ActionToPerform::RunSavedSearch {
                        id,
                        target: RunTarget::Tab(new_id),
                    }
                    .schedule();
                }
                return;
            }
        };
        let Some(tab_id) = tab_id else {
            return;
        };
        let favorites = ctx.data_get_persisted::<Locations>().unwrap_or_default();
        let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
            return;
        };
        let path = match &saved.scope {
            SavedScope::CurrentFolder => match &tab.current_path {
                // Leave favorites mode for the folder the search started from.
                CurrentPath::Multiple(paths) => paths.first().cloned().map(CurrentPath::One),
                _ => None,
            },
            SavedScope::Favorites => Some(CurrentPath::Multiple(favorites.paths())),
            SavedScope::Folders(paths) => match paths.as_slice() {
                [] => None,
                [one] => Some(CurrentPath::One(one.clone())),
                many => Some(CurrentPath::Multiple(many.to_vec())),
            },
        };
        tab.search = Some(Search {
            saved_from: Some(id),
            ..saved.query.clone()
        });
        tab.search_dirty_since = None;
        if let Some(path) = path.filter(|path| *path != tab.current_path) {
            {
                tab.keep_search_once = true;
                self.handle_action(
                    ctx,
                    ActionToPerform::TabAction(
                        TabTarget::TabWithId(tab_id),
                        TabAction::ChangePaths(path),
                    ),
                );
            }
        } else {
            tab.update_visible_entries();
            TabAction::RequestFilesRefresh.schedule_tab(tab_id);
        }
        self.commit_search(tab_id, &format!("run saved search \"{}\"", saved.name));
    }

    /// Opens the dialog that saves the active tab's search.
    pub(crate) fn open_save_dialog(&mut self, ctx: &Context) {
        let Some(tab) = self.tabs.get_current_tab() else {
            return;
        };
        let Some(search) = tab.search.clone() else {
            toast!(Info, "Start a search to save it.");
            return;
        };
        let scope = if tab.current_path.multiple_paths() {
            SavedScope::Favorites
        } else {
            SavedScope::CurrentFolder
        };
        let text = query::format(&search);
        let name = load_saved(ctx).free_name(&text.chars().take(40).collect::<String>());
        self.saved_editor = Some(SavedSearchEditor {
            id: None,
            name,
            query: Search {
                saved_from: None,
                ..search
            },
            scope,
            new_folder: String::new(),
            attach_to_tab: Some(tab.id),
            focused: false,
        });
        self.display_modal = Some(ModalWindow::EditSavedSearch);
    }

    pub(crate) fn open_edit_dialog(&mut self, ctx: &Context, id: u64) {
        let Some(saved) = load_saved(ctx).get(id).cloned() else {
            return;
        };
        self.saved_editor = Some(SavedSearchEditor {
            id: Some(id),
            name: saved.name,
            query: saved.query,
            scope: saved.scope,
            new_folder: String::new(),
            attach_to_tab: None,
            focused: false,
        });
        self.display_modal = Some(ModalWindow::EditSavedSearch);
    }

    /// Writes the active tab's search over the saved search it came from.
    pub(crate) fn update_saved_from_tab(&mut self, ctx: &Context) {
        let Some(search) = self
            .tabs
            .get_current_tab()
            .and_then(|tab| tab.search.clone())
        else {
            return;
        };
        let Some(id) = search.saved_from else {
            return;
        };
        let name = load_saved(ctx)
            .get(id)
            .map(|s| s.name.clone())
            .unwrap_or_default();
        self.update_saved_searches(ctx, &format!("update saved search \"{name}\""), |list| {
            if let Some(saved) = list.iter_mut().find(|s| s.id == id) {
                saved.query = Search {
                    saved_from: None,
                    ..search
                };
            }
        });
        toast!(Success, "Updated \"{name}\" · Ctrl+Z to undo");
    }

    /// Puts the saved version back into the active tab's search.
    pub(crate) fn revert_to_saved(&mut self, ctx: &Context) {
        let Some(tab) = self.tabs.get_current_tab() else {
            return;
        };
        let Some(id) = tab.search.as_ref().and_then(|s| s.saved_from) else {
            return;
        };
        let tab_id = tab.id;
        if let Some(saved) = load_saved(ctx).get(id) {
            tab.search = Some(Search {
                saved_from: Some(id),
                ..saved.query.clone()
            });
            tab.update_visible_entries();
            TabAction::RequestFilesRefresh.schedule_tab(tab_id);
            self.commit_search(tab_id, "revert to saved search");
        }
    }

    pub(crate) fn delete_saved_search(&mut self, ctx: &Context, id: u64) {
        let name = load_saved(ctx)
            .get(id)
            .map(|s| s.name.clone())
            .unwrap_or_default();
        self.update_saved_searches(ctx, &format!("delete saved search \"{name}\""), |list| {
            list.retain(|s| s.id != id);
        });
        toast!(Info, "Deleted \"{name}\" · Ctrl+Z to undo");
    }

    pub(crate) fn duplicate_saved_search(&mut self, ctx: &Context, id: u64) {
        let saved = load_saved(ctx);
        let Some(original) = saved.get(id).cloned() else {
            return;
        };
        let copy = SavedSearch {
            id: saved.next_id(),
            name: saved.free_name(&format!("{} copy", original.name)),
            ..original
        };
        self.update_saved_searches(ctx, "duplicate saved search", |list| {
            let at = list
                .iter()
                .position(|s| s.id == id)
                .map_or(list.len(), |i| i + 1);
            list.insert(at, copy);
        });
    }

    /// Moves a saved search to `to` (an index in the list).
    pub(crate) fn move_saved_search(&mut self, ctx: &Context, id: u64, to: usize) {
        self.update_saved_searches(ctx, "reorder saved searches", |list| {
            if let Some(from) = list.iter().position(|s| s.id == id) {
                let item = list.remove(from);
                list.insert(to.min(list.len()), item);
            }
        });
    }

    /// The rows of the "Saved searches" section of the sidebar.
    pub(crate) fn saved_searches_sidebar(
        ui: &mut Ui,
        saved: &SavedSearches,
        active_saved: Option<u64>,
    ) {
        let count = saved.searches.len();
        for (index, item) in saved.searches.iter().enumerate() {
            let response = ui::nav_row(
                ui,
                Icon::Search,
                &item.name,
                active_saved == Some(item.id),
                egui::Sense::click_and_drag(),
            );
            let response = response.on_hover_text(item.summary());
            let new_tab = response.middle_clicked()
                || (response.clicked() && ui.input(|i| i.modifiers.command));
            if new_tab || response.clicked() {
                ActionToPerform::RunSavedSearch {
                    id: item.id,
                    target: if new_tab {
                        RunTarget::NewTab
                    } else {
                        RunTarget::ActiveTab
                    },
                }
                .schedule();
            }
            // Drag a row onto another to reorder.
            response.dnd_set_drag_payload(DraggedSavedSearch(item.id));
            if let Some(dragged) = response.dnd_hover_payload::<DraggedSavedSearch>()
                && dragged.0 != item.id
            {
                let y = response.rect.top();
                ui.painter().hline(
                    response.rect.x_range(),
                    y,
                    egui::Stroke::new(2.0, ui::Palette::of(ui).accent),
                );
            }
            if let Some(dragged) = response.dnd_release_payload::<DraggedSavedSearch>() {
                ActionToPerform::MoveSavedSearch {
                    id: dragged.0,
                    to: index,
                }
                .schedule();
            }
            response.context_menu(|ui| saved_search_menu(ui, item, index, count));
        }
    }

    /// Draws the dialog for `ModalWindow::EditSavedSearch`.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn saved_search_editor_ui(&mut self, ctx: &Context) {
        let Some(editor) = &mut self.saved_editor else {
            ActionToPerform::CloseActiveModalWindow.schedule();
            return;
        };
        let mut save = false;
        let mut run = false;
        let mut cancel = false;
        let has_favorites = !ctx
            .data_get_persisted::<Locations>()
            .unwrap_or_default()
            .locations
            .is_empty();
        let name_taken = load_saved(ctx)
            .searches
            .iter()
            .any(|s| s.name == editor.name.trim() && Some(s.id) != editor.id);
        let modal = egui::Modal::new(egui::Id::new(ModalWindow::EditSavedSearch)).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 64.0).clamp(320.0, 560.0));
            ui.heading(if editor.id.is_some() {
                "Edit saved search"
            } else {
                "Save search"
            });
            ui.label("Name");
            let name = ui.add(
                egui::TextEdit::singleline(&mut editor.name)
                    .min_size(ui::text_edit_min_size(ui))
                    .vertical_align(ui::text_edit_align(ui))
                    .desired_width(ui.available_width()),
            );
            if !editor.focused {
                editor.focused = true;
                name.request_focus();
            }
            if name_taken {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    "Another saved search has this name.",
                );
            }

            ui.add_space(6.0);
            ui.label("Find items matching");
            chip_input(
                ui,
                egui::Id::new("saved_search_editor_query"),
                &mut editor.query,
                "Type words or filters like ext:pdf",
                &ChipContext::default(),
            );
            ui.horizontal(|ui| {
                let names = editor.query.chips.iter().filter(|c| c.is_name()).count()
                    + usize::from(!editor.query.value.trim().is_empty());
                if names > 1 {
                    ui.selectable_value(&mut editor.query.match_mode, MatchMode::All, "Match all");
                    ui.selectable_value(&mut editor.query.match_mode, MatchMode::Any, "Match any");
                    ui.separator();
                }
                ui.checkbox(&mut editor.query.case_sensitive, "Match case");
                let mut deep = editor.query.depth > 1;
                if ui.checkbox(&mut deep, "Include subfolders").changed() {
                    editor.query.depth = if deep { 4 } else { 1 };
                }
            });

            ui.add_space(6.0);
            ui.label("Search in");
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(editor.scope == SavedScope::CurrentFolder, "The open folder")
                    .on_hover_text("Runs in whatever folder the tab shows")
                    .clicked()
                {
                    editor.scope = SavedScope::CurrentFolder;
                }
                ui.add_enabled_ui(has_favorites, |ui| {
                    if ui
                        .selectable_label(editor.scope == SavedScope::Favorites, "Favorites")
                        .clicked()
                    {
                        editor.scope = SavedScope::Favorites;
                    }
                });
                if ui
                    .selectable_label(
                        matches!(editor.scope, SavedScope::Folders(_)),
                        "These folders",
                    )
                    .clicked()
                    && !matches!(editor.scope, SavedScope::Folders(_))
                {
                    editor.scope = SavedScope::Folders(Vec::new());
                }
            });
            if let SavedScope::Folders(folders) = &mut editor.scope {
                let mut remove = None;
                for (index, folder) in folders.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui::icon_button(ui, Icon::X, "Remove folder", false).clicked() {
                            remove = Some(index);
                        }
                        ui.add(egui::Label::new(folder.display().to_string()).truncate());
                    });
                }
                if let Some(index) = remove {
                    folders.remove(index);
                }
                ui.horizontal(|ui| {
                    let input = ui.add(
                        egui::TextEdit::singleline(&mut editor.new_folder)
                            .hint_text("Folder path")
                            .desired_width((ui.available_width() - 40.0).max(120.0)),
                    );
                    let path = PathBuf::from(editor.new_folder.trim());
                    let valid = path.is_dir() && !folders.contains(&path);
                    let add = ui
                        .add_enabled(valid, egui::Button::new(ui::glyph(Icon::Plus)))
                        .on_hover_text("Add folder")
                        .clicked()
                        || (valid
                            && input.lost_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                    if add {
                        folders.push(path);
                        editor.new_folder.clear();
                    }
                });
            }

            ui.add_space(6.0);
            let preview = query::format(&editor.query);
            ui.weak(if preview.is_empty() {
                "Matches everything".to_owned()
            } else {
                format!("Query: {preview}")
            });
            ui.separator();
            let valid = !editor.name.trim().is_empty()
                && !name_taken
                && !matches!(&editor.scope, SavedScope::Folders(f) if f.is_empty());
            ui.horizontal(|ui| {
                save = ui.add_enabled(valid, egui::Button::new("Save")).clicked();
                run = ui
                    .add_enabled(valid, egui::Button::new("Save and run"))
                    .clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        if (save || run)
            && let Some(editor) = self.saved_editor.take()
        {
            self.finish_saved_search_editor(ctx, editor, run);
            ActionToPerform::CloseActiveModalWindow.schedule();
        } else if cancel || modal.should_close() || super::file_ops::escape_pressed(ctx) {
            self.saved_editor = None;
            ActionToPerform::CloseActiveModalWindow.schedule();
        }
    }

    pub(crate) fn finish_saved_search_editor(
        &mut self,
        ctx: &Context,
        editor: SavedSearchEditor,
        run: bool,
    ) {
        let mut query = editor.query;
        query.commit_text();
        query.saved_from = None;
        let name = editor.name.trim().to_owned();
        let id = editor.id.unwrap_or_else(|| load_saved(ctx).next_id());
        let item = SavedSearch {
            id,
            name: name.clone(),
            query,
            scope: editor.scope,
        };
        let label = if editor.id.is_some() {
            format!("edit saved search \"{name}\"")
        } else {
            format!("save search \"{name}\"")
        };
        self.update_saved_searches(ctx, &label, |list| {
            match list.iter_mut().find(|s| s.id == id) {
                Some(existing) => *existing = item,
                None => list.push(item),
            }
        });
        if run {
            self.run_saved_search(ctx, id, RunTarget::ActiveTab);
        } else if let Some(tab_id) = editor.attach_to_tab
            && let Some(tab) = self.tabs.get_tab_by_id(tab_id)
            && let Some(search) = &mut tab.search
        {
            // The open search is now that saved search.
            search.saved_from = Some(id);
            toast!(Success, "Saved \"{name}\"");
        }
    }
}

fn saved_search_menu(ui: &mut Ui, item: &SavedSearch, index: usize, count: usize) {
    let id = item.id;
    if ui.button("Run").clicked() {
        ActionToPerform::RunSavedSearch {
            id,
            target: RunTarget::ActiveTab,
        }
        .schedule();
        ui.close();
    }
    if ui.button("Run in new tab").clicked() {
        ActionToPerform::RunSavedSearch {
            id,
            target: RunTarget::NewTab,
        }
        .schedule();
        ui.close();
    }
    ui.separator();
    if ui.button("Edit…").clicked() {
        ActionToPerform::EditSavedSearch(id).schedule();
        ui.close();
    }
    if ui.button("Duplicate").clicked() {
        ActionToPerform::DuplicateSavedSearch(id).schedule();
        ui.close();
    }
    if ui.button("Copy query").clicked() {
        ui.ctx().copy_text(query::format(&item.query));
        ui.close();
    }
    ui.separator();
    if ui
        .add_enabled(index > 0, egui::Button::new("Move up"))
        .clicked()
    {
        ActionToPerform::MoveSavedSearch { id, to: index - 1 }.schedule();
        ui.close();
    }
    if ui
        .add_enabled(index + 1 < count, egui::Button::new("Move down"))
        .clicked()
    {
        ActionToPerform::MoveSavedSearch { id, to: index + 1 }.schedule();
        ui.close();
    }
    ui.separator();
    if ui.button("Delete").clicked() {
        ActionToPerform::DeleteSavedSearch(id).schedule();
        ui.close();
    }
}
