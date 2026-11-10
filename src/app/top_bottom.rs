use super::keymap::{self, Command};
use super::search::{Age, Chip, Cmp, FileKind};
use super::{
    ActionToPerform, App, DataSource, DisplayType, MatchMode, SavedSearches, Search,
    SearchTermType, Sort,
    commands::TabAction,
    directory_view_settings::{DirectoryShowHidden, DirectoryViewSettings},
    dock::TabData,
    ui::{self, Palette},
};
use crate::app::dock::TabId;
use crate::widgets::chip_input;
use crate::{helper::DataHolder, locations::Locations};
use egui::{Ui, Vec2};
use lucide_icons::Icon;
use std::{
    ops::Deref,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default)]
pub struct TopDisplayPath(Vec<TopDisplayPathPart>);

impl Deref for TopDisplayPath {
    type Target = Vec<TopDisplayPathPart>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct TopDisplayPathPart {
    pub text: String,
    pub path: String,
}

impl TopDisplayPath {
    /// Purely lexical: no filesystem access, so it is cheap to call on every
    /// navigation or settings change. Sub-folder lists are read lazily when a
    /// breadcrumb's context menu is opened.
    pub fn build(&mut self, current_path: &Path) {
        #[cfg(feature = "profiling")]
        puffin::profile_function!("TopDisplayPath::build");
        self.0.clear();
        let mut path: String = String::new();

        #[allow(unused_variables)] // not used on linux
        for (i, e) in current_path.iter().enumerate() {
            #[cfg(windows)]
            let text = match &i {
                0 => {
                    let Some(s) = e.to_str() else {
                        continue;
                    };
                    let last_two_chars: String = s.chars().rev().take(2).collect();
                    path += &last_two_chars.chars().rev().collect::<String>();
                    path.push(std::path::MAIN_SEPARATOR);
                    continue;
                }
                1 => path.clone(),
                _ => {
                    let Some(s) = e.to_str() else {
                        return;
                    };
                    path += s;
                    path.push(std::path::MAIN_SEPARATOR);
                    s.to_string()
                }
            };
            #[cfg(not(windows))]
            let text = {
                let Some(part) = e.to_str() else {
                    continue;
                };
                if !part.starts_with('/') && !path.ends_with('/') {
                    path += "/";
                }
                path += part;
                part.to_string()
            };
            self.0.push(TopDisplayPathPart {
                text,
                path: path.clone(),
            });
        }
    }
}

impl App {
    /// The address bar in the Recycle Bin view: its name and "Empty".
    fn trash_display(tab: &TabData, ui: &mut Ui) {
        ui.label(ui::glyph(Icon::Trash2));
        ui.strong(crate::fileops::trash_bin::LABEL);
        if !tab.listing.is_empty() && ui.button("Empty…").clicked() {
            ActionToPerform::ConfirmEmptyTrash.schedule();
        }
    }

    /// The address bar: breadcrumbs in a field whose empty part edits the
    /// path when clicked, as in Explorer.
    fn path_field(tab: &mut TabData, ui: &mut Ui) {
        let side = ui::control_height(ui);
        let (rect, background) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), side), egui::Sense::click());
        let editable = tab.current_path.single_path().is_some() && !tab.current_path.is_trash();
        background
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, editable, "Edit path"));
        let background = background
            .on_hover_text(keymap::tooltip(ui.ctx(), Command::FocusAddress))
            .on_hover_cursor(egui::CursorIcon::Text);
        if background.clicked() && editable && tab.path_edit.is_none() {
            ActionToPerform::ToggleTopEdit.schedule();
        }
        let editing = tab.path_edit.is_some();
        if !editing {
            let palette = Palette::of(ui);
            let stroke = if background.hovered() {
                palette.text_weak
            } else {
                palette.border
            };
            ui.painter().rect(
                rect,
                ui::control_radius(ui),
                palette.card_bg,
                egui::Stroke::new(1.0_f32, stroke),
                egui::StrokeKind::Inside,
            );
        }
        let inner = if editing {
            rect
        } else {
            rect.shrink2(Vec2::new(4.0, 0.0))
        };
        let mut path_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("path_controls")
                .max_rect(inner)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        path_ui.set_clip_rect(inner);
        let tab_id = tab.id;
        let show_hidden = tab.show_hidden;
        let mut close_editor = false;
        match (tab.current_path.single_path(), &mut tab.path_edit) {
            (Some(path), Some(info)) => {
                close_editor =
                    super::path_editor::show(tab_id, info, &path, show_hidden, &mut path_ui);
            }
            _ if tab.current_path.is_trash() => Self::trash_display(tab, &mut path_ui),
            (Some(_), None) => {
                // The chevrons between crumbs make the spacing.
                path_ui.spacing_mut().item_spacing.x = 0.0;
                super::breadcrumbs::show(&mut path_ui, tab);
            }
            (None, _) => {
                path_ui.add(egui::Label::new(tab.current_path.get_name_from_path()).truncate());
            }
        }
        if close_editor {
            tab.path_edit = None;
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn top_panel(&mut self, root: &mut Ui) {
        let ctx = &root.ctx().clone();
        let layout = ui::PanelLayout::new(
            ctx.content_rect().width(),
            self.settings.sidebar_visible,
            self.settings.inspector_visible,
        );
        // Show what is on screen, not the preference: narrow windows hide
        // the panels whatever the setting.
        let window = self.tabs.window();
        let sidebar = layout.sidebar || window.sidebar_overlay;
        let inspector = layout.inspector || window.inspector_overlay;
        let undo = self
            .file_ops
            .undo
            .peek_undo()
            .map(super::undo::UndoItem::label);
        let redo = self
            .file_ops
            .undo
            .peek_redo()
            .map(super::undo::UndoItem::label);
        let mut inline_search = false;
        let mut search_out = SearchBarOutput::default();
        // A fixed height: a resizable panel keeps whatever height it once
        // had in memory, and the bar used to grow and never shrink back.
        let margin = ui::toolbar_margin(root);
        let height = ui::control_height(root) + margin.sum().y;
        egui::Panel::top("top_panel")
            .resizable(false)
            .exact_size(height)
            .frame(
                egui::Frame::new()
                    .fill(
                        egui_mev_theme::Theme::of(ctx)
                            .palette(ctx.global_style().visuals.dark_mode)
                            .toolbar_bg,
                    )
                    .inner_margin(margin),
            )
            .show(root, |ui| {
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let Some(tab) = self.tabs.get_current_tab() else {
                        return;
                    };
                    ui.add_enabled_ui(tab.can_go_back(), |ui| {
                        if toolbar_button(ui, Icon::ArrowLeft, Command::Back, false).clicked()
                            && let Some(action) = tab.go_back()
                        {
                            action.schedule();
                        }
                    });
                    ui.add_enabled_ui(tab.can_go_forward(), |ui| {
                        if toolbar_button(ui, Icon::ArrowRight, Command::Forward, false).clicked()
                            && let Some(action) = tab.go_forward()
                        {
                            action.schedule();
                        }
                    });
                    ui.add_enabled_ui(tab.current_path.parent().is_some(), |ui| {
                        if toolbar_button(ui, Icon::ArrowUp, Command::Up, false).clicked()
                            && let Some(parent) = tab.current_path.parent()
                        {
                            TabAction::ChangePaths(crate::app::dock::existing_dir(parent))
                                .schedule_tab(tab.id);
                        }
                    });
                    // Decide from the space left what fits beside the path,
                    // giving up the search field first, then the view controls.
                    // Search and More always stay.
                    let step = ui::control_height(ui) + ui.spacing().item_spacing.x;
                    let spare = 2.0f32.mul_add(-step, ui.available_width());
                    let with_view = 3.0f32.mul_add(-step, spare) >= MIN_PATH_WIDTH;
                    let left = if with_view {
                        3.0f32.mul_add(-step, spare)
                    } else {
                        spare
                    };
                    // The search field takes the bar's space while it is in
                    // use; the path then makes do with less.
                    let gap = ui.spacing().item_spacing.x;
                    let idle_width = (left * 0.25).clamp(240.0, 320.0);
                    let inline = left - idle_width - gap >= MIN_PATH_WIDTH;
                    let field_width = if tab.search.is_some() {
                        (left - gap - MIN_SEARCHING_PATH_WIDTH).clamp(idle_width, 520.0)
                    } else {
                        idle_width
                    };
                    inline_search = inline;
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        more_menu(
                            ui,
                            tab,
                            MoreMenu {
                                undo: undo.as_deref(),
                                redo: redo.as_deref(),
                                with_view: !with_view,
                                sidebar,
                                inspector,
                            },
                        );
                        if with_view {
                            view_group(ui, tab);
                        }
                        if inline {
                            ui.allocate_ui_with_layout(
                                Vec2::new(field_width, ui::control_height(ui)),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| toolbar_search_field(ui, tab, &mut search_out),
                            );
                        } else if toolbar_button(
                            ui,
                            Icon::Search,
                            Command::FocusSearch,
                            tab.search.is_some(),
                        )
                        .clicked()
                        {
                            ActionToPerform::RunCommand(Command::FocusSearch).schedule();
                        }
                        Self::path_field(tab, ui);
                    });
                });
            });
        self.search_panel(root, inline_search, search_out);
    }
    /// The search options under the bar: match case, filters and help, where
    /// to look, saved-search controls and the result count. The input itself
    /// sits in the top bar when there is room (`inline`), else first in here;
    /// `out` carries what the top bar's input did.
    #[allow(clippy::too_many_lines)]
    fn search_panel(&mut self, root: &mut Ui, inline: bool, mut out: SearchBarOutput) {
        let ctx = &root.ctx().clone();
        let saved = super::saved_searches::load_saved(ctx);
        let Some(tab) = self.tabs.get_current_tab() else {
            return;
        };
        if tab.search.is_none() {
            ctx.data_mut(|data| data.remove::<bool>(egui::Id::unique(("search_focus", tab.id))));
            return;
        }
        let tab_id = tab.id;
        let favorites_scope = tab.current_path.multiple_paths();
        let loading = tab.load.is_loading();
        let result_count = tab.visible_entries.len();
        let was_searching = tab.is_searching();
        let favorites = ctx.data_get_persisted::<Locations>().unwrap_or_default();
        let has_favorites = !favorites.locations.is_empty();
        let field_id = egui::Id::unique(("search_field", tab_id));
        let (extensions, folders) = search_suggestions(ctx, tab, field_id);
        let search = tab.search.as_mut().expect("search exists");
        egui::Panel::top("search_row")
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(
                        egui_mev_theme::Theme::of(ctx)
                            .palette(ctx.global_style().visuals.dark_mode)
                            .toolbar_bg,
                    )
                    .inner_margin(egui::Margin::symmetric(12, 6)),
            )
            .show(root, |ui| {
                // ── Row 1: what to find, when the top bar had no room ────
                if !inline {
                    ui.horizontal(|ui| {
                        let input_width = (ui.available_width() - 150.0).max(160.0);
                        ui.allocate_ui(egui::vec2(input_width, ui::control_height(ui)), |ui| {
                            search_input(
                                ui,
                                tab_id,
                                search,
                                "Search by name, or add a filter — e.g. ext:pdf",
                                (&extensions, &folders),
                                &mut out,
                            );
                        });
                        search_tools(ui, field_id, search, &mut out);
                    });
                }

                // ── Row 2: where to look, how to combine, saved searches ─
                ui.horizontal_wrapped(|ui| {
                    if inline {
                        search_tools(ui, field_id, search, &mut out);
                        ui.separator();
                    }
                    ui.weak("Look in");
                    let favorites_tip = if has_favorites {
                        "Search every favorite folder"
                    } else {
                        "Add a folder to Favorites first"
                    };
                    let choice = ui::segmented_text(
                        ui,
                        &[
                            ("This folder", true, "Search the open folder"),
                            ("Favorites", has_favorites, favorites_tip),
                        ],
                        usize::from(favorites_scope),
                    );
                    if let Some(index) = choice {
                        TabAction::SearchInFavorites(index == 1).schedule_tab(tab_id);
                    }
                    wrap_unless_room(ui, SUBFOLDER_OPTIONS_WIDTH);
                    let mut subfolders = search.depth > 1;
                    if ui
                        .checkbox(&mut subfolders, "Include subfolders")
                        .on_hover_text("Also search inside subfolders")
                        .changed()
                    {
                        search.depth = if subfolders { 4 } else { 1 };
                        out.rescan = true;
                        out.commit = Some("search subfolders".to_owned());
                    }
                    if subfolders {
                        let before = search.depth;
                        let selected = format!("{} levels deep", search.depth);
                        let depth = egui::ComboBox::from_id_salt(("search_depth", tab_id))
                            .selected_text(&selected)
                            .show_ui(ui, |ui| {
                                for depth in 2..=7 {
                                    ui.selectable_value(
                                        &mut search.depth,
                                        depth,
                                        format!("{depth} levels deep"),
                                    );
                                }
                            })
                            .response
                            .on_hover_text("How many folder levels to search");
                        labelled_combo(&depth, "Search depth", &selected);
                        if before != search.depth {
                            out.rescan = true;
                            out.commit = Some("search depth".to_owned());
                        }
                    }
                    if search.name_count() > 1 {
                        wrap_unless_room(ui, MATCH_MODE_WIDTH);
                        ui.separator();
                        let before = search.match_mode;
                        let selected = match search.match_mode {
                            MatchMode::All => "Match all words",
                            MatchMode::Any => "Match any word",
                        };
                        let mode = egui::ComboBox::from_id_salt(("match_mode", tab_id))
                            .selected_text(selected)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut search.match_mode,
                                    MatchMode::All,
                                    "Match all words",
                                )
                                .on_hover_text("Names must contain every word");
                                ui.selectable_value(
                                    &mut search.match_mode,
                                    MatchMode::Any,
                                    "Match any word",
                                )
                                .on_hover_text("Names may contain any of the words");
                            })
                            .response;
                        labelled_combo(&mode, "Match words", selected);
                        if before != search.match_mode {
                            out.commit = Some("match all / any".to_owned());
                        }
                    }

                    // Too little room left on this line: put the status and
                    // the saved-search controls on their own line instead of
                    // over the options.
                    wrap_unless_room(ui, SEARCH_STATUS_WIDTH);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        out.close |=
                            ui::icon_button(ui, Icon::X, "Close search (Esc)", false).clicked();
                        if loading {
                            if ui.button("Stop").on_hover_text("Stop searching").clicked() {
                                out.stop = true;
                            }
                            ui.spinner();
                        }
                        if was_searching {
                            ui.weak(match result_count {
                                0 if loading => "Searching…".to_owned(),
                                0 => "No results".to_owned(),
                                1 => "1 result".to_owned(),
                                n => format!("{n} results"),
                            });
                        }
                        ui.separator();
                        saved_controls(ui, search, &saved, &mut out);
                    });
                });

                // When nothing matched, offer to widen the search.
                if was_searching && !loading && result_count == 0 && search.depth <= 1 {
                    ui.horizontal(|ui| {
                        ui.weak("Nothing matched in this folder.");
                        if ui.button("Search subfolders").clicked() {
                            search.depth = 4;
                            out.rescan = true;
                            out.commit = Some("search subfolders".to_owned());
                        }
                        if has_favorites
                            && !favorites_scope
                            && ui.button("Search favorites").clicked()
                        {
                            TabAction::SearchInFavorites(true).schedule_tab(tab_id);
                        }
                    });
                }
            });

        // Typing into or clearing the box of a deep search changes what has
        // to be scanned, since an empty query only lists the folder itself.
        let now_searching = search.has_query();
        if search.depth > 1 && now_searching != was_searching {
            out.rescan = true;
        }
        // Adding or removing `in:` folders changes what is scanned.
        out.rescan |= out
            .commit
            .as_deref()
            .is_some_and(|label| label.contains(" in:"));
        if out.stop {
            tab.refresh_generation
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tab.cancel_token
                .store(true, std::sync::atomic::Ordering::SeqCst);
            tab.load.cancel_rerun();
        }
        if out.to_results && !tab.visible_entries.is_empty() {
            tab.selection.select_only(0);
        }
        if out.close {
            tab.toggle_search();
            out.commit = Some("close search".to_owned());
        }
        if out.rescan {
            tab.update_visible_entries();
            TabAction::RequestFilesRefresh.schedule_tab(tab_id);
        } else if out.typed || out.preview || out.commit.is_some() {
            TabAction::FilterChanged.schedule_tab(tab_id);
        }
        if out.typed {
            self.search_typed(tab_id);
        }
        if let Some(label) = out.commit {
            self.commit_search(tab_id, &label);
        }
        match out.saved_action {
            Some(SavedAction::Save) => self.open_save_dialog(ctx),
            Some(SavedAction::Update) => self.update_saved_from_tab(ctx),
            Some(SavedAction::Revert) => self.revert_to_saved(ctx),
            Some(SavedAction::Edit(id)) => self.open_edit_dialog(ctx, id),
            None => {}
        }
    }
    pub(crate) fn bottom_panel(&mut self, root: &mut Ui) {
        let ctx = &root.ctx().clone();
        egui::Panel::bottom("bottomPanel")
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(
                        egui_mev_theme::Theme::of(ctx)
                            .palette(ctx.global_style().visuals.dark_mode)
                            .toolbar_bg,
                    )
                    .inner_margin(egui::Margin::symmetric(12, 4)),
            )
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    let cut = self.tabs.cut.len();
                    let Some(tab) = self.tabs.get_current_tab() else {
                        return;
                    };
                    let folder = tab.current_path.single_path();
                    let count = tab.visible_entries.len();
                    let total = tab.total_entry_count();
                    ui.weak(if count == total {
                        format!("{count} items")
                    } else {
                        format!("{count} of {total} items")
                    });
                    let selected = tab.selection.len();
                    if selected > 0 {
                        let bytes: u64 = tab
                            .selection
                            .rows()
                            .filter_map(|row| tab.visible_entries.get(row))
                            .map(|&index| tab.listing.meta(index as usize).size)
                            .sum();
                        let text = if bytes > 0 {
                            format!(
                                "{selected} selected · {}",
                                egui_mev_core::format::format_bytes(bytes)
                            )
                        } else {
                            format!("{selected} selected")
                        };
                        ui::badge(ui, &text);
                    }
                    if cut > 0 {
                        ui.weak(format!(
                            "{cut} cut · Ctrl+V to move {}",
                            if cut == 1 { "it" } else { "them" }
                        ));
                    }
                    if tab.load.is_loading() {
                        ui.spinner();
                        ui.weak(tab.load.progress().unwrap_or("Loading…"));
                    }
                    self.file_ops_status(ui);
                    if let Some(panel) = crate::plugins::ui::jobs_status(ui, &mut self.plugins) {
                        self.plugins.show_panel(&panel);
                        self.display_modal = Some(super::commands::ModalWindow::PluginResults);
                    }
                    self.drive_status(ui, folder.as_deref());
                });
            });
    }
}
pub(super) fn breadcrumb_menu_entry(ui: &mut Ui, part: &TopDisplayPathPart) {
    if ui
        .button(if part.text.is_empty() {
            "/"
        } else {
            &part.text
        })
        .clicked()
    {
        if let Some(action) = ActionToPerform::path_from_str(&part.path, false) {
            action.schedule();
        }
        ui.close();
    }
}
/// Sort radio buttons and the reverse toggle, shared by every sort menu.
pub fn sort_options(ui: &mut Ui, view: &mut DirectoryViewSettings) {
    for sort in Sort::ALL {
        ui.selectable_value(&mut view.sorting, sort, sort.label());
    }
    ui.separator();
    ui.add_enabled_ui(view.sorting != Sort::Random, |ui| {
        ui.checkbox(&mut view.invert_sort, "Reverse order");
    });
}

/// The Sort submenu used in context menus; saves the change for the folder.
pub fn sort_submenu(ui: &mut Ui, tab: &TabData) {
    let mut view = ui
        .data_get_path_or_persisted::<DirectoryViewSettings>(&tab.current_path)
        .data;
    let old = (view.sorting, view.invert_sort);
    ui.menu_button("Sort by", |ui| sort_options(ui, &mut view));
    if old != (view.sorting, view.invert_sort) {
        ui.data_set_path(&tab.current_path, view);
        ActionToPerform::ViewSettingsChanged(DataSource::Local).schedule();
    }
}

/// The narrowest the path field gets before controls beside it give way,
/// and while a search in the bar is using its space.
const MIN_PATH_WIDTH: f32 = 320.0;
const MIN_SEARCHING_PATH_WIDTH: f32 = 200.0;

/// A top-bar button for `command`: named after it, with its shortcut in the
/// tooltip.
fn toolbar_button(ui: &mut Ui, icon: Icon, command: Command, active: bool) -> egui::Response {
    ui::icon_button_named(
        ui,
        icon,
        command.label(),
        &keymap::tooltip(ui.ctx(), command),
        active,
    )
}

/// Sort, then List and Grid as one segment. The bar lays them out right to
/// left, so they are added in that order.
fn view_group(ui: &mut Ui, tab: &TabData) {
    let mut view = ui
        .data_get_path_or_persisted::<DirectoryViewSettings>(&tab.current_path)
        .data;
    let old = (view.display_type, view.sorting, view.invert_sort);
    ui::icon_menu_button(ui, Icon::ArrowUpDown, "Sort files", |ui| {
        ui.strong("Sort by");
        sort_options(ui, &mut view);
    });
    let grid = view.display_type == DisplayType::Icons;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        if toolbar_button(ui, Icon::Grid2x2, Command::ViewGrid, grid).clicked() {
            view.display_type = DisplayType::Icons;
        }
        if toolbar_button(ui, Icon::List, Command::ViewList, !grid).clicked() {
            view.display_type = DisplayType::List;
        }
    });
    if old != (view.display_type, view.sorting, view.invert_sort) {
        ui.data_set_path(&tab.current_path, view);
        ActionToPerform::ViewSettingsChanged(DataSource::Local).schedule();
    }
}

/// The view choices as menu items, for when the bar has no room for them.
fn view_menu_items(ui: &mut Ui, tab: &TabData) {
    let mut view = ui
        .data_get_path_or_persisted::<DirectoryViewSettings>(&tab.current_path)
        .data;
    let old = (view.display_type, view.sorting, view.invert_sort);
    ui.selectable_value(&mut view.display_type, DisplayType::List, "List view");
    ui.selectable_value(&mut view.display_type, DisplayType::Icons, "Grid view");
    ui.menu_button("Sort by", |ui| sort_options(ui, &mut view));
    if old != (view.display_type, view.sorting, view.invert_sort) {
        ui.data_set_path(&tab.current_path, view);
        ActionToPerform::ViewSettingsChanged(DataSource::Local).schedule();
    }
}

/// What the More menu shows besides its fixed entries.
#[derive(Clone, Copy)]
struct MoreMenu<'a> {
    undo: Option<&'a str>,
    redo: Option<&'a str>,
    /// The view choices, when the bar has no room for them.
    with_view: bool,
    /// Whether the sidebar and the inspector are on screen.
    sidebar: bool,
    inspector: bool,
}

fn more_menu(ui: &mut Ui, tab: &TabData, more: MoreMenu<'_>) {
    let MoreMenu {
        undo,
        redo,
        with_view,
        sidebar,
        inspector,
    } = more;
    ui::icon_menu_button(ui, Icon::Ellipsis, "More actions", |ui| {
        if with_view {
            view_menu_items(ui, tab);
            ui.separator();
        }
        keymap::menu_toggle(ui, Command::ToggleSidebar, sidebar);
        keymap::menu_toggle(ui, Command::ToggleInspector, inspector);
        ui.separator();
        let mut hidden = ui
            .data_get_path_or_persisted::<DirectoryShowHidden>(&tab.current_path)
            .data;
        if ui.checkbox(&mut hidden.0, "Show hidden files").changed() {
            ui.data_set_path(&tab.current_path, hidden);
            TabAction::RequestFilesRefresh.schedule_tab(tab.id);
            ActionToPerform::ViewSettingsChanged(DataSource::Local).schedule();
        }
        if let Some(path) = tab.current_path.single_path()
            && ui.button("Open in terminal").clicked()
        {
            ActionToPerform::OpenInTerminal(path).schedule();
            ui.close();
        }
        keymap::menu_button(ui, Command::Refresh);
        keymap::menu_button(ui, Command::FocusAddress);
        keymap::menu_button(ui, Command::NewFolder);
        keymap::menu_button_labeled(
            ui,
            Command::Undo,
            &undo.map_or_else(|| "Undo".to_owned(), |l| format!("Undo {l}")),
            undo.is_some(),
        );
        keymap::menu_button_labeled(
            ui,
            Command::Redo,
            &redo.map_or_else(|| "Redo".to_owned(), |l| format!("Redo {l}")),
            redo.is_some(),
        );
        ui.separator();
        keymap::menu_button(ui, Command::Settings);
        keymap::menu_button(ui, Command::Palette);
        keymap::menu_button(ui, Command::ShortcutHelp);
    });
}

/// The search field in the top bar. Idle, it looks like the search input and
/// opens the search as soon as it gets focus; open, it is the input.
fn toolbar_search_field(ui: &mut Ui, tab: &mut TabData, out: &mut SearchBarOutput) {
    let ctx = ui.ctx().clone();
    let hint = format!("Search {}", tab.current_path.get_name_from_path());
    let field_id = egui::Id::unique(("search_field", tab.id));
    if tab.search.is_none() {
        let mut idle = Search::default();
        let input = chip_input::chip_input(
            ui,
            egui::Id::unique(("search_field_idle", tab.id)),
            &mut idle,
            &hint,
            &chip_input::ChipContext {
                extensions: &[],
                folders: &[],
            },
        );
        if input.text.is_some_and(|text| text.gained_focus()) {
            ActionToPerform::RunCommand(Command::FocusSearch).schedule();
        }
        return;
    }
    let (extensions, folders) = search_suggestions(&ctx, tab, field_id);
    let search = tab.search.as_mut().expect("search exists");
    search_input(ui, tab.id, search, &hint, (&extensions, &folders), out);
}

/// Suggestions for the pill editors, gathered only while one is open.
fn search_suggestions(
    ctx: &egui::Context,
    tab: &TabData,
    field_id: egui::Id,
) -> (Vec<String>, Vec<(String, PathBuf)>) {
    if !chip_input::is_editing(ctx, field_id) {
        return (Vec::new(), Vec::new());
    }
    let favorites = ctx.data_get_persisted::<Locations>().unwrap_or_default();
    (
        common_extensions(tab),
        chip_input::folder_choices(
            tab.current_path.single_path().as_deref(),
            favorites
                .locations
                .iter()
                .map(|l| (l.name.to_string(), PathBuf::from(l.path.as_ref()))),
        ),
    )
}

/// The search input with its keys: it takes focus when the search opens,
/// Esc clears the text, then the pills, then closes, and Down moves into the
/// results.
fn search_input(
    ui: &mut Ui,
    tab_id: TabId,
    search: &mut Search,
    hint: &str,
    (extensions, folders): (&[String], &[(String, PathBuf)]),
    out: &mut SearchBarOutput,
) {
    let field_id = egui::Id::unique(("search_field", tab_id));
    let input = chip_input::chip_input(
        ui,
        field_id,
        search,
        hint,
        &chip_input::ChipContext {
            extensions,
            folders,
        },
    );
    out.typed |= input.text_changed;
    out.preview |= input.preview;
    if let Some(label) = input.chips_changed {
        out.commit = Some(label);
    }
    if input.submitted {
        out.to_results = true;
    }
    let Some(response) = &input.text else {
        return;
    };
    let ctx = ui.ctx();
    let flag = egui::Id::unique(("search_focus", tab_id));
    if !ctx
        .data(|data| data.get_temp::<bool>(flag))
        .unwrap_or(false)
    {
        response.request_focus();
        ctx.data_mut(|data| data.insert_temp(flag, true));
    }
    if response.lost_focus()
        && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        if !search.value.is_empty() {
            search.value.clear();
            out.typed = true;
            response.request_focus();
        } else if !search.chips.is_empty() {
            search.chips.clear();
            out.commit = Some("clear search".to_owned());
            response.request_focus();
        } else {
            out.close = true;
        }
    }
    if response.has_focus()
        && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown))
    {
        response.surrender_focus();
        out.to_results = true;
    }
}

/// Match case, "+ Filter" and the syntax help.
fn search_tools(ui: &mut Ui, field_id: egui::Id, search: &mut Search, out: &mut SearchBarOutput) {
    let ctx = ui.ctx().clone();
    if ui::icon_button(
        ui,
        Icon::CaseSensitive,
        &keymap::tooltip(&ctx, Command::MatchCase),
        search.case_sensitive,
    )
    .clicked()
    {
        search.case_sensitive = !search.case_sensitive;
        out.commit = Some("match case".to_owned());
    }
    ui.menu_button("+ Filter", |ui| match filter_menu(ui, search) {
        Some(FilterAdded::Changed(label)) => out.commit = Some(label),
        Some(FilterAdded::Edit(index, is_new)) => {
            chip_input::edit_chip(&ctx, field_id, index, is_new);
        }
        None => {}
    })
    .response
    .on_hover_text("Add a filter for type, size, date or folder");
    let help = ui::icon_button(ui, Icon::CircleHelp, "Search help", false);
    egui::Popup::menu(&help).show(search_help);
}

/// Room the search options need on a line: "Include subfolders" with its
/// depth, the match mode, and the status (count, Save, Close).
const SUBFOLDER_OPTIONS_WIDTH: f32 = 260.0;
const MATCH_MODE_WIDTH: f32 = 160.0;
const SEARCH_STATUS_WIDTH: f32 = 260.0;

/// Starts a new line of a wrapping row unless `width` is left on this one.
/// Widgets such as combo boxes do not wrap by themselves.
fn wrap_unless_room(ui: &mut Ui, width: f32) {
    if ui.max_rect().right() - ui.cursor().left() < width {
        ui.end_row();
    }
}

/// Names a combo box for screen readers, which otherwise only hear its value.
fn labelled_combo(response: &egui::Response, label: &str, selected: &str) {
    response.widget_info(|| {
        let mut info = egui::WidgetInfo::labeled(egui::Role::ComboBox, true, label);
        info.current_text_value = Some(selected.to_owned());
        info
    });
}

/// What the search bar asks the app to do after drawing.
#[derive(Default)]
#[allow(clippy::struct_excessive_bools)]
struct SearchBarOutput {
    /// The typed text changed; recorded for undo after a pause.
    typed: bool,
    /// A pill is being edited: filter again, record when the editor closes.
    preview: bool,
    /// A change to record for undo now, with its label.
    commit: Option<String>,
    /// The folders or depth to scan changed.
    rescan: bool,
    close: bool,
    stop: bool,
    to_results: bool,
    saved_action: Option<SavedAction>,
}

enum SavedAction {
    Save,
    Update,
    Revert,
    Edit(u64),
}

/// The search help: the query syntax and how pills work.
fn search_help(ui: &mut Ui) {
    ui.strong("Search syntax");
    egui::Grid::new("search_help")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            for (example, meaning) in super::search::query::HELP {
                ui.monospace(*example);
                ui.label(*meaning);
                ui.end_row();
            }
        });
    ui.add_space(4.0);
    ui.weak("Enter, or a space after a filter, turns text into pills.");
    ui.weak("Click a pill to edit it. Remove it with its ×, a middle-click or its menu.");
    ui.weak("With the cursor at the start: ← selects pills, Delete removes, Enter edits.");
}

/// The extensions most common among the tab's items, for the extension
/// editor's suggestions.
fn common_extensions(tab: &TabData) -> Vec<String> {
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    // A sample is plenty for suggestions and keeps huge listings cheap.
    for index in 0..tab.listing.len().min(5000) {
        if tab.listing.is_file(index)
            && let Some((_, ext)) = tab.listing.name(index).rsplit_once('.')
            && !ext.is_empty()
            && ext.len() <= 8
        {
            *counts.entry(ext.to_lowercase()).or_default() += 1;
        }
    }
    let mut common: Vec<(String, usize)> = counts.into_iter().collect();
    common.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    common.into_iter().map(|(ext, _)| ext).collect()
}

/// What the "+ Filter" menu did.
enum FilterAdded {
    /// The search changed; record it with this label.
    Changed(String),
    /// Open the editor of this pill; `true` when it was just added.
    Edit(usize, bool),
}

/// The "+ Filter" menu: adds filters without knowing the syntax. Presets
/// apply at once (a check marks the ones in use, and choosing one again
/// removes it); the others add a pill and open its editor to ask for the
/// value.
fn filter_menu(ui: &mut Ui, search: &mut Search) -> Option<FilterAdded> {
    let mut result = None;
    preset_filters(ui, search, &mut result);
    ui.separator();
    if ui.button("Extension…").clicked() {
        // One extension pill holds them all; edit it if there is one.
        let existing = search.chips.iter().position(|c| matches!(c, Chip::Ext(_)));
        result = Some(existing.map_or_else(
            || add_and_edit(search, Chip::Ext(Vec::new())),
            |index| FilterAdded::Edit(index, false),
        ));
        ui.close();
    }
    if ui.button("Exclude a word…").clicked() {
        result = Some(add_and_edit(
            search,
            Chip::Name {
                pattern: String::new(),
                kind: SearchTermType::Plain,
                exclude: true,
            },
        ));
        ui.close();
    }
    if ui
        .button("Also search a folder…")
        .on_hover_text("Search another folder together with this one")
        .clicked()
    {
        result = Some(add_and_edit(search, Chip::In(PathBuf::new())));
        ui.close();
    }
    result
}

/// A "+ Filter" submenu of [`Age`] presets for the date chip made by `chip`.
fn age_filter(
    ui: &mut Ui,
    search: &mut Search,
    result: &mut Option<FilterAdded>,
    label: &str,
    chip: fn(Age) -> Chip,
) {
    let current = Age::ALL
        .into_iter()
        .find(|&age| search.chips.contains(&chip(age)));
    ui.menu_button(label, |ui| {
        for value in Age::ALL {
            if ui
                .add(egui::Button::selectable(
                    current == Some(value),
                    value.label(),
                ))
                .clicked()
            {
                let value_chip = chip(value);
                *result = if current == Some(value) {
                    let token = super::search::query::chip_token(&value_chip);
                    search.chips.retain(|chip| *chip != value_chip);
                    Some(FilterAdded::Changed(format!("remove filter {token}")))
                } else {
                    search.add_chip(value_chip).map(FilterAdded::Changed)
                };
                ui.close();
            }
        }
    });
}

/// The Type, Modified, Opened and Size submenus of "+ Filter".
fn preset_filters(ui: &mut Ui, search: &mut Search, result: &mut Option<FilterAdded>) {
    let kinds: Vec<FileKind> = search
        .chips
        .iter()
        .find_map(|chip| match chip {
            Chip::Kind(kinds) => Some(kinds.clone()),
            _ => None,
        })
        .unwrap_or_default();
    ui.menu_button("Type", |ui| {
        for kind in FileKind::ALL {
            let mut on = kinds.contains(&kind);
            if ui.checkbox(&mut on, kind.label()).clicked() {
                *result = Some(FilterAdded::Changed(search.toggle_kind(kind)));
            }
        }
    });
    age_filter(ui, search, result, "Modified", Chip::Modified);
    age_filter(ui, search, result, "Opened", Chip::Opened);
    ui.menu_button("Size", |ui| {
        for (label, cmp, bytes) in [
            ("Under 1 MB", Cmp::Less, 1 << 20),
            ("Over 10 MB", Cmp::Greater, 10 << 20),
            ("Over 100 MB", Cmp::Greater, 100 << 20),
            ("Over 1 GB", Cmp::Greater, 1 << 30),
        ] {
            let chip = Chip::Size { cmp, bytes };
            let on = search.chips.contains(&chip);
            if ui.add(egui::Button::selectable(on, label)).clicked() {
                *result = if on {
                    search.chips.retain(|c| *c != chip);
                    Some(FilterAdded::Changed(format!(
                        "remove filter {}",
                        super::search::query::chip_token(&chip)
                    )))
                } else {
                    search.add_chip(chip).map(FilterAdded::Changed)
                };
                ui.close();
            }
        }
        ui.separator();
        if ui.button("Custom size…").clicked() {
            *result = Some(add_and_edit(
                search,
                Chip::Size {
                    cmp: Cmp::Greater,
                    bytes: 10 << 20,
                },
            ));
            ui.close();
        }
    });
}

/// Adds a pill still to be filled in and asks to open its editor.
fn add_and_edit(search: &mut Search, chip: Chip) -> FilterAdded {
    search.chips.push(chip);
    FilterAdded::Edit(search.chips.len() - 1, true)
}

/// Saved-search controls on the right of the search bar: which saved search
/// is open, whether it changed, and Save / Update / Revert.
fn saved_controls(ui: &mut Ui, search: &Search, saved: &SavedSearches, out: &mut SearchBarOutput) {
    let current = search.saved_from.and_then(|id| saved.get(id));
    match current {
        Some(item) => {
            let changed = !item.query.same_query(&Search {
                saved_from: None,
                ..search.clone()
            });
            if changed
                && ui::icon_button(ui, Icon::Undo2, "Revert to the saved version", false).clicked()
            {
                out.saved_action = Some(SavedAction::Revert);
            }
            ui.menu_button(if changed { "Save ▾" } else { "Saved" }, |ui| {
                if ui
                    .add_enabled(
                        changed,
                        egui::Button::new(format!("Update \"{}\"", item.name)),
                    )
                    .clicked()
                {
                    out.saved_action = Some(SavedAction::Update);
                    ui.close();
                }
                if ui.button("Save as new…").clicked() {
                    out.saved_action = Some(SavedAction::Save);
                    ui.close();
                }
                if ui.button("Edit saved search…").clicked() {
                    out.saved_action = Some(SavedAction::Edit(item.id));
                    ui.close();
                }
            });
            let pill = format!("★ {}{}", item.name, if changed { " •" } else { "" });
            ui.label(egui::RichText::new(pill).strong())
                .on_hover_text(if changed {
                    "Changed since it was saved"
                } else {
                    "This is a saved search"
                });
        }
        None => {
            if ui
                .button("★ Save")
                .on_hover_text("Save this search (Ctrl+S)")
                .clicked()
            {
                out.saved_action = Some(SavedAction::Save);
            }
        }
    }
}
