use super::{
    ActionToPerform, App, DataSource, DisplayType, MatchMode, SearchTerm, SearchTermType, Sort,
    commands::{ModalWindow, TabAction},
    directory_path_info::DirectoryPathInfo,
    directory_view_settings::{DirectoryShowHidden, DirectoryViewSettings},
    dock::{Selected, TabData},
    ui::{self, Palette},
};
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
    pub has_subdirectories: bool,
}

impl TopDisplayPath {
    pub fn build(&mut self, current_path: &Path, show_hidden: bool) {
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
            let has_subdirectories =
                crate::app::dir_handling::has_subdirectories(Path::new(&path), show_hidden);
            self.0.push(TopDisplayPathPart {
                text,
                path: path.clone(),
                has_subdirectories,
            });
        }
    }
}

impl App {
    pub(crate) fn top_display_editable(index: u32, current_path: &Path, ui: &mut Ui) {
        use crate::widgets::autocomplete_text::AutoCompleteTextEdit;
        let Some(mut info) = ui.data_get_tab::<DirectoryPathInfo>(index) else {
            return;
        };
        let response = ui.add_sized(
            [ui.available_width().max(32.0), 28.0],
            AutoCompleteTextEdit::new(&mut info.text_input, &info.possible_options)
                .max_suggestions(10)
                .set_text_edit_properties(move |edit| {
                    edit.id(egui::Id::new(("path_editor", index)))
                })
                .highlight_matches(true),
        );
        if !response.has_focus() && !ui.ctx().memory(|m| m.focused().is_some()) {
            response.request_focus();
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            ActionToPerform::ToggleTopEdit.schedule();
        } else if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let path = Path::new(&info.text_input);
            if path.is_dir() {
                if path != current_path {
                    TabAction::ChangePaths(path.to_path_buf().into()).schedule_tab(index);
                }
                ActionToPerform::ToggleTopEdit.schedule();
            } else {
                crate::toast!(Warning, "Directory does not exist");
            }
        }
        ui.data_set_tab(index, info);
    }
    pub(crate) fn top_display(tab: &TabData, ui: &mut Ui) {
        let parts = &tab.top_display_path;
        if parts.is_empty() {
            ui.label(tab.current_path.get_name_from_path());
            return;
        }
        let widths: Vec<f32> = parts
            .iter()
            .map(|p| {
                ui.fonts_mut(|f| {
                    f.layout_no_wrap(
                        p.text.clone(),
                        egui::FontId::proportional(13.0),
                        Palette::of(ui).text,
                    )
                })
                .size()
                .x + 36.0
            })
            .collect();
        let collapse = widths.iter().sum::<f32>() > ui.available_width();
        if collapse && parts.len() > 1 {
            ui.menu_button(ui::glyph(Icon::Ellipsis), |ui| {
                for part in parts.iter().take(parts.len() - 1) {
                    breadcrumb_menu_entry(ui, part);
                }
            })
            .response
            .on_hover_text("Ancestor folders");
        }
        for (i, part) in parts.iter().enumerate() {
            if collapse && i + 1 != parts.len() {
                continue;
            }
            if i > 0 && !collapse {
                ui.label(
                    ui::glyph(Icon::ChevronRight)
                        .size(12.0)
                        .color(Palette::of(ui).secondary),
                );
            }
            let label = if part.text.is_empty() {
                "/"
            } else {
                &part.text
            };
            let response = ui
                .add(
                    egui::Button::new(egui::RichText::new(label).strong())
                        .frame(false)
                        .truncate(),
                )
                .on_hover_text(&part.path);
            if response.clicked()
                && let Some(action) =
                    ActionToPerform::path_from_str(&part.path, ui.input(|i| i.modifiers.command))
            {
                action.schedule();
            }
            response.context_menu(|ui| {
                breadcrumb_menu_entry(ui, part);
                if ui.button("Open in new tab").clicked() {
                    ActionToPerform::NewTab(PathBuf::from(&part.path)).schedule();
                    ui.close();
                }
                if part.has_subdirectories {
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .max_height(240.0)
                        .show(ui, |ui| {
                            for path in super::dir_handling::get_directories_recursive(
                                Path::new(&part.path),
                                tab.show_hidden,
                                1,
                            ) {
                                if path != part.path
                                    && ui
                                        .button(
                                            Path::new(path.as_ref())
                                                .file_name()
                                                .unwrap_or_default()
                                                .to_string_lossy(),
                                        )
                                        .clicked()
                                {
                                    TabAction::ChangePaths(PathBuf::from(path.as_ref()).into())
                                        .schedule_tab(tab.id);
                                    ui.close();
                                }
                            }
                        });
                }
            });
        }
    }
    #[allow(clippy::too_many_lines)]
    pub(crate) fn top_panel(&mut self, root: &mut Ui) {
        let ctx = &root.ctx().clone();
        let compact = ctx.content_rect().width() < 800.0;
        egui::Panel::top("top_panel")
            .frame(
                egui::Frame::new()
                    .fill(Palette::for_theme(ctx.global_style().visuals.dark_mode).toolbar)
                    .inner_margin(egui::Margin::symmetric(8, 6)),
            )
            .show_inside(root, |ui| {
                ui.horizontal_centered(|ui| {
                    if ui::icon_button(
                        ui,
                        Icon::PanelLeft,
                        "Toggle sidebar · Alt+Cmd/Ctrl+S",
                        self.settings.sidebar_visible,
                    )
                    .clicked()
                    {
                        ActionToPerform::ToggleSidebar.schedule();
                    }
                    let Some(tab) = self.tabs.get_current_tab() else {
                        return;
                    };
                    let searching = tab.is_searching();
                    ui.add_enabled_ui(tab.can_undo() && !searching, |ui| {
                        if ui::icon_button(ui, Icon::ArrowLeft, "Go back", false).clicked()
                            && let Some(action) = tab.undo()
                        {
                            action.schedule();
                        }
                    });
                    ui.add_enabled_ui(tab.can_redo() && !searching, |ui| {
                        if ui::icon_button(ui, Icon::ArrowRight, "Go forward", false).clicked()
                            && let Some(action) = tab.redo()
                        {
                            action.schedule();
                        }
                    });
                    ui.add_enabled_ui(tab.current_path.parent().is_some() && !searching, |ui| {
                        if ui::icon_button(ui, Icon::ArrowUp, "Parent folder", false).clicked()
                            && let Some(parent) = tab.current_path.parent()
                        {
                            TabAction::ChangePaths(parent.into()).schedule_tab(tab.id);
                        }
                    });
                    let trailing = if compact { 116.0 } else { 224.0 };
                    let width = (ui.available_width() - trailing).max(48.0);
                    let (path_rect, _) =
                        ui.allocate_exact_size(Vec2::new(width, 28.0), egui::Sense::hover());
                    let mut path_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .id_salt("path_controls")
                            .max_rect(path_rect)
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );
                    path_ui.set_clip_rect(path_rect);
                    if let Some(path) = tab.current_path.single_path() {
                        if path_ui.data_has_tab::<DirectoryPathInfo>(tab.id) {
                            Self::top_display_editable(tab.id, &path, &mut path_ui);
                        } else {
                            Self::top_display(tab, &mut path_ui);
                        }
                    } else {
                        path_ui.add(
                            egui::Label::new(tab.current_path.get_name_from_path()).truncate(),
                        );
                    }
                    if ui::icon_button(
                        ui,
                        Icon::Search,
                        "Search · Cmd/Ctrl+F",
                        tab.search.is_some(),
                    )
                    .clicked()
                    {
                        tab.toggle_search(ctx);
                        TabAction::FilterChanged.schedule_tab(tab.id);
                    }
                    if !compact {
                        view_controls(ui, tab);
                    }
                    let menu = ui.menu_button(ui::glyph(Icon::Ellipsis), |ui| {
                        if compact {
                            view_controls(ui, tab);
                            ui.separator();
                        }
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
                        if ui.button("Refresh · F5").clicked() {
                            TabAction::ForceRefresh.schedule_tab(tab.id);
                            ui.close();
                        }
                        if ui.button("Edit path · Cmd/Ctrl+L").clicked() {
                            ActionToPerform::ToggleTopEdit.schedule();
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Settings · Cmd/Ctrl+,").clicked() {
                            ActionToPerform::ToggleModalWindow(ModalWindow::Settings).schedule();
                            ui.close();
                        }
                        if ui.button("Commands · Cmd/Ctrl+R").clicked() {
                            if let Some(path) = tab.current_path.single_path() {
                                self.command_palette.build_for_path(
                                    &tab.current_path,
                                    &path,
                                    &ui.data_get_persisted::<Locations>().unwrap_or_default(),
                                );
                            }
                            ActionToPerform::ToggleModalWindow(ModalWindow::Commands).schedule();
                            ui.close();
                        }
                    });
                    menu.response.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "More actions")
                    });
                    menu.response.on_hover_text("More actions");
                    if ui::icon_button(
                        ui,
                        Icon::PanelRight,
                        "Toggle inspector · Alt+Cmd/Ctrl+I",
                        self.settings.inspector_visible,
                    )
                    .clicked()
                    {
                        ActionToPerform::ToggleInspector.schedule();
                    }
                });
            });
        self.search_panel(root);
    }
    #[allow(clippy::too_many_lines)]
    fn search_panel(&mut self, root: &mut Ui) {
        let ctx = &root.ctx().clone();
        let Some(tab) = self.tabs.get_current_tab() else {
            return;
        };
        if tab.search.is_none() {
            ctx.data_mut(|data| data.remove::<bool>(egui::Id::new(("search_focus", tab.id))));
            return;
        }
        let tab_id = tab.id;
        let favorites_scope = tab.current_path.multiple_paths();
        let search = tab.search.as_mut().expect("search exists");
        let mut filter_changed = false;
        let mut target_changed = false;
        let mut close = false;
        egui::Panel::top("search_row")
            .frame(
                egui::Frame::new()
                    .fill(Palette::for_theme(ctx.global_style().visuals.dark_mode).toolbar)
                    .inner_margin(egui::Margin::symmetric(12, 6)),
            )
            .show_inside(root, |ui| {
                ui.horizontal(|ui| {
                    ui.label(ui::glyph(Icon::Search));
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut search.value)
                            .hint_text("Search files…")
                            .desired_width((ui.available_width() - 150.0).max(80.0)),
                    );
                    let focus_id = egui::Id::new(("search_focus", tab_id));
                    if !ctx
                        .data(|data| data.get_temp::<bool>(focus_id))
                        .unwrap_or(false)
                    {
                        response.request_focus();
                        ctx.data_mut(|data| data.insert_temp(focus_id, true));
                    }
                    filter_changed |= response.changed();
                    let advanced = search.case_sensitive
                        || search.term_type != SearchTermType::Plain
                        || search.depth > 1
                        || !search.terms.is_empty()
                        || !search.extra_dirs.is_empty()
                        || favorites_scope;
                    ui.menu_button(if advanced { "Options •" } else { "Options" }, |ui| {
                        ui.set_width(300.0);
                        egui::ScrollArea::vertical()
                            .max_height((ctx.content_rect().height() - 140.0).max(180.0))
                            .show(ui, |ui| {
                                filter_changed |= ui
                                    .checkbox(&mut search.case_sensitive, "Case sensitive")
                                    .changed();
                                ui::form_row(ui, "Pattern", |ui| {
                                    for mode in [
                                        SearchTermType::Plain,
                                        SearchTermType::Glob,
                                        SearchTermType::Regex,
                                    ] {
                                        filter_changed |= ui
                                            .selectable_value(
                                                &mut search.term_type,
                                                mode,
                                                format!("{mode:?}"),
                                            )
                                            .changed();
                                    }
                                });
                                target_changed |= ui
                                    .add(egui::Slider::new(&mut search.depth, 1..=7).text("Depth"))
                                    .changed();
                                let mut scope = favorites_scope;
                                ui.add_enabled_ui(
                                    !ui.data_get_persisted::<Locations>()
                                        .unwrap_or_default()
                                        .locations
                                        .is_empty(),
                                    |ui| {
                                        if ui.checkbox(&mut scope, "Search favorites").changed() {
                                            TabAction::SearchInFavorites(scope)
                                                .schedule_tab(tab_id);
                                        }
                                    },
                                );
                                ui::section(ui, "Search terms");
                                ui.horizontal(|ui| {
                                    filter_changed |= ui
                                        .selectable_value(
                                            &mut search.match_mode,
                                            MatchMode::All,
                                            "Match all (AND)",
                                        )
                                        .changed();
                                    filter_changed |= ui
                                        .selectable_value(
                                            &mut search.match_mode,
                                            MatchMode::Any,
                                            "Match any (OR)",
                                        )
                                        .changed();
                                });
                                if ui
                                    .add_enabled(
                                        !search.value.is_empty(),
                                        egui::Button::new("Add current query as term"),
                                    )
                                    .clicked()
                                {
                                    search.terms.push(SearchTerm {
                                        pattern: std::mem::take(&mut search.value),
                                        term_type: search.term_type,
                                    });
                                    filter_changed = true;
                                }
                                let mut remove = None;
                                for (index, term) in search.terms.iter().enumerate() {
                                    ui.horizontal(|ui| {
                                        if ui::icon_button(ui, Icon::X, "Remove term", false)
                                            .clicked()
                                        {
                                            remove = Some(index);
                                        }
                                        ui.label(format!("{:?}: {}", term.term_type, term.pattern));
                                    });
                                }
                                if let Some(index) = remove {
                                    search.terms.remove(index);
                                    filter_changed = true;
                                }
                                ui::section(ui, "Additional folders");
                                let mut remove = None;
                                for (index, path) in search.extra_dirs.iter().enumerate() {
                                    ui.horizontal(|ui| {
                                        if ui::icon_button(ui, Icon::X, "Remove folder", false)
                                            .clicked()
                                        {
                                            remove = Some(index);
                                        }
                                        ui.add(egui::Label::new(path.to_string_lossy()).truncate())
                                            .on_hover_text(path.to_string_lossy());
                                    });
                                }
                                if let Some(index) = remove {
                                    TabAction::RemoveSearchDir(index).schedule_tab(tab_id);
                                }
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut search.new_dir_input)
                                            .hint_text("Folder path")
                                            .desired_width(230.0),
                                    );
                                    if ui::icon_button(ui, Icon::Plus, "Add folder", false)
                                        .clicked()
                                        && !search.new_dir_input.trim().is_empty()
                                    {
                                        TabAction::AddSearchDir(PathBuf::from(
                                            search.new_dir_input.trim(),
                                        ))
                                        .schedule_tab(tab_id);
                                        search.new_dir_input.clear();
                                    }
                                });
                                if advanced && ui.button("Clear advanced options").clicked() {
                                    search.case_sensitive = false;
                                    search.term_type = SearchTermType::Plain;
                                    search.depth = 1;
                                    search.terms.clear();
                                    search.extra_dirs.clear();
                                    search.match_mode = MatchMode::All;
                                    filter_changed = true;
                                    target_changed = true;
                                    if favorites_scope {
                                        TabAction::SearchInFavorites(false).schedule_tab(tab_id);
                                    }
                                }
                                ui::section(ui, "Saved searches");
                                let saved = ui
                                    .data_get_persisted::<super::SavedSearches>()
                                    .unwrap_or_default();
                                for item in saved.searches {
                                    ui.horizontal(|ui| {
                                        if ui.button(&item.name).clicked() {
                                            TabAction::LoadSavedSearch(item.name.clone())
                                                .schedule_tab(tab_id);
                                            ui.close();
                                        }
                                        if ui::icon_button(
                                            ui,
                                            Icon::X,
                                            "Delete saved search",
                                            false,
                                        )
                                        .clicked()
                                        {
                                            TabAction::DeleteSavedSearch(item.name.clone())
                                                .schedule_tab(tab_id);
                                        }
                                    });
                                }
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut search.save_name_input)
                                            .hint_text("Search name")
                                            .desired_width(230.0),
                                    );
                                    if ui
                                        .add_enabled(
                                            !search.save_name_input.trim().is_empty(),
                                            egui::Button::new("Save"),
                                        )
                                        .clicked()
                                    {
                                        TabAction::SaveSearch(
                                            search.save_name_input.trim().to_owned(),
                                        )
                                        .schedule_tab(tab_id);
                                        search.save_name_input.clear();
                                    }
                                });
                            });
                    });
                    close = ui::icon_button(ui, Icon::X, "Close search", false).clicked();
                });
            });
        if close {
            tab.toggle_search(ctx);
            filter_changed = true;
        }
        if target_changed {
            tab.update_visible_entries();
            TabAction::RequestFilesRefresh.schedule_tab(tab_id);
        } else if filter_changed {
            TabAction::FilterChanged.schedule_tab(tab_id);
        }
    }
    pub(crate) fn bottom_panel(&mut self, root: &mut Ui) {
        let ctx = &root.ctx().clone();
        egui::Panel::bottom("bottomPanel")
            .frame(
                egui::Frame::new()
                    .fill(Palette::for_theme(ctx.global_style().visuals.dark_mode).toolbar)
                    .inner_margin(egui::Margin::symmetric(12, 4)),
            )
            .show_inside(root, |ui| {
                ui.horizontal(|ui| {
                    let Some(tab) = self.tabs.get_current_tab() else {
                        return;
                    };
                    let count = tab.visible_entries.len();
                    let total = tab.total_entry_count();
                    ui.weak(if count == total {
                        format!("{count} items")
                    } else {
                        format!("{count} of {total} items")
                    });
                    let selected = ui
                        .data_get_path::<Selected>(&tab.current_path)
                        .unwrap_or_default()
                        .selected_fields
                        .len();
                    if selected > 0 {
                        ui::badge(ui, &format!("{selected} selected"));
                    }
                    if tab.loading {
                        ui.spinner();
                        ui.weak(tab.loading_progress.as_deref().unwrap_or("Loading…"));
                    }
                });
            });
    }
}
fn breadcrumb_menu_entry(ui: &mut Ui, part: &TopDisplayPathPart) {
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
fn view_controls(ui: &mut Ui, tab: &TabData) {
    let mut view = ui
        .data_get_path_or_persisted::<DirectoryViewSettings>(&tab.current_path)
        .data;
    let old = (view.display_type, view.sorting, view.invert_sort);
    if let Some(index) = ui::segmented_icons(
        ui,
        &[(Icon::List, "List view"), (Icon::Grid2x2, "Grid view")],
        usize::from(view.display_type == DisplayType::Icons),
    ) {
        view.display_type = if index == 0 {
            DisplayType::List
        } else {
            DisplayType::Icons
        };
    }
    let sort_menu = ui.menu_button(ui::glyph(Icon::ArrowUpDown), |ui| {
        ui.strong("Sort by");
        for sort in [
            Sort::Name,
            Sort::Created,
            Sort::Modified,
            Sort::Size,
            Sort::Random,
        ] {
            ui.selectable_value(&mut view.sorting, sort, format!("{sort:?}"));
        }
        ui.separator();
        ui.add_enabled_ui(view.sorting != Sort::Random, |ui| {
            ui.checkbox(&mut view.invert_sort, "Reverse order");
        });
    });
    sort_menu.response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), "Sort files")
    });
    sort_menu.response.on_hover_text("Sort files");
    if old != (view.display_type, view.sorting, view.invert_sort) {
        ui.data_set_path(&tab.current_path, view);
        ActionToPerform::ViewSettingsChanged(DataSource::Local).schedule();
    }
}
