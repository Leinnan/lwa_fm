use std::{
    ops::Deref,
    path::{Path, PathBuf},
    str::FromStr,
};

use super::{ActionToPerform, App, Sort};
use crate::{
    app::{
        commands::TabAction,
        dir_handling::get_directories_recursive,
        directory_path_info::DirectoryPathInfo,
        directory_view_settings::{DirectoryShowHidden, DirectoryViewSettings},
        dock::TabData,
    },
    consts::{GIT_HASH_INFO, HOMEPAGE, TOP_SIDE_MARGIN, VERSION},
    helper::{DataHolder, KeyWithCommandPressed},
    locations::Locations,
    widgets::{ButtonGroupElement, UiBuilderExt},
};
use egui::{Button, Context, Layout, OpenUrl, Ui, Vec2, style::HandleShape};

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

#[allow(clippy::too_many_lines)]
impl App {
    pub(crate) fn top_display_editable(index: u32, current_path: &Path, ui: &mut Ui) {
        #[cfg(feature = "profiling")]
        puffin::profile_function!("App::top_display_editable");
        use crate::widgets::autocomplete_text::AutoCompleteTextEdit;
        let size = ui.available_size();
        let Some(mut directory_info) = ui.data_get_tab::<DirectoryPathInfo>(index) else {
            return;
        };

        let _ = ui.add_sized(
            [size.x.max(500.0) - 130.0, 24.0],
            AutoCompleteTextEdit::new(
                &mut directory_info.text_input,
                &directory_info.possible_options,
            )
            .max_suggestions(10)
            .set_text_edit_properties(|s| s.frame(false))
            .highlight_matches(true),
        );

        let should_close =
            ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape));
        if should_close {
            ActionToPerform::ToggleTopEdit.schedule();
        } else {
            let path = Path::new(&directory_info.text_input);
            if path.exists()
                && path.is_dir()
                && !path.eq(current_path)
                && let Some(action) =
                    ActionToPerform::path_from_str(directory_info.text_input.clone(), false)
            {
                action.schedule();
            }
        }
        ui.data_set_tab(index, directory_info);
    }
    #[allow(clippy::needless_pass_by_ref_mut)]
    pub(crate) fn top_display(tab: &TabData, ui: &mut Ui) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::top_display");
        let tab_index = tab.id;
        let parts = tab.top_display_path.len();
        #[allow(unused_variables)]
        let command_pressed = ui.command_pressed();

        #[allow(unused_variables)] // not used on linux
        for (i, part) in tab.top_display_path.iter().enumerate() {
            let button_group = if i == parts - 1 {
                ButtonGroupElement::Last
            } else {
                ButtonGroupElement::InTheMiddle
            };

            let button = ui.add(Button::new(&part.text).corner_radius(button_group));
            if button.clicked()
                && let Some(action) =
                    ActionToPerform::path_from_str(&part.path, ui.command_pressed())
            {
                action.schedule();
            }
            button.context_menu(|ui| {
                if ui.button("Open").clicked() {
                    if let Some(action) = ActionToPerform::path_from_str(&part.path, false) {
                        action.schedule();
                    }
                    ui.close();
                }
                if ui.button("Open in new tab").clicked() {
                    if let Some(action) = ActionToPerform::path_from_str(&part.path, true) {
                        action.schedule();
                    }
                    ui.close();
                }
            });

            if part.has_subdirectories {
                let button =
                    ui.add(Button::new(">").corner_radius(ButtonGroupElement::InTheMiddle));
                button.context_menu(|ui| {
                    let dirs =
                        get_directories_recursive(std::path::Path::new(&part.path), false, 1);
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for dir in &dirs {
                            let dir_display = dir.replace(&part.path, "");
                            if dir_display.is_empty() {
                                continue;
                            }
                            if ui.button(dir_display.as_str()).clicked() {
                                TabAction::ChangePaths(
                                    PathBuf::from_str(dir)
                                        .expect("Failed to convert path")
                                        .into(),
                                )
                                .schedule_tab(tab_index);
                                ui.close();
                            }
                        }
                    });
                });
            }
        }
    }

    fn undo_redo_up(&mut self, ui: &mut Ui) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::undo_redo_up");
        let Some(current_tab) = self.tabs.get_current_tab() else {
            return;
        };
        ui.btn_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                let spacing = ui.spacing().item_spacing;
                ui.spacing_mut().item_spacing = Vec2::ZERO;

                ui.add_enabled_ui(current_tab.can_undo(), |ui| {
                    let button = Button::new("⮪").corner_radius(ButtonGroupElement::First);
                    if ui.add(button).on_hover_text("Go back").clicked()
                        && let Some(action) = current_tab.undo()
                    {
                        action.schedule();
                    }
                });
                ui.add_enabled_ui(current_tab.can_redo(), |ui| {
                    let button = Button::new("⮫").corner_radius(ButtonGroupElement::Last);
                    if ui.add(button).on_hover_text("Redo").clicked()
                        && let Some(action) = current_tab.redo()
                    {
                        action.schedule();
                    }
                });
                ui.spacing_mut().item_spacing = spacing;
            });
        });
    }

    pub(crate) fn top_panel(&mut self, ctx: &Context) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::top_panel");
        let is_searching = self
            .tabs
            .get_current_tab()
            .is_some_and(|tab| tab.search.is_some());
        let index = self.tabs.get_current_index().unwrap_or_default();
        egui::TopBottomPanel::top("top_panel")
            .frame(egui::Frame::canvas(&ctx.style()))
            .show(ctx, |ui| {
                ui.with_layout(Layout::left_to_right(egui::Align::Min), |ui| {
                    ui.add_enabled_ui(!is_searching, |ui| self.undo_redo_up(ui));
                    ui.add_space(TOP_SIDE_MARGIN);
                    let available_space = ui.available_size();

                    ui.allocate_ui_with_layout(
                        available_space,
                        Layout::right_to_left(eframe::emath::Align::Min),
                        |ui| {
                            ui.scope_builder(ui.btn_frame_ui(), |ui| {
                                let frame = ui.btn_frame();
                                frame.show(ui, |ui| {
                                    let Some(current_tab) = self.tabs.get_current_tab() else {
                                        return;
                                    };
                                    let spacing = ui.spacing().item_spacing;
                                    ui.spacing_mut().item_spacing = Vec2::new(1.0, spacing.y);

                                    let mut was_favorites =
                                        current_tab.current_path.multiple_paths();
                                    let mut search_visible = is_searching;
                                    ui.toggle_value(&mut search_visible, "🔍")
                                        .on_hover_text("Search");
                                    let mut search_changed = is_searching != search_visible;
                                    if let Some(search) = &mut current_tab.search {
                                        search_changed |= ui
                                            .toggle_value(&mut search.case_sensitive, "🇨")
                                            .on_hover_text("Case sensitive")
                                            .changed();
                                        let search_input = ui.add(
                                            egui::TextEdit::singleline(&mut search.value)
                                                .hint_text("Search"),
                                        );
                                        search_changed |= search_input.changed();
                                        search_changed |= ui
                                            .add(
                                                egui::Slider::new(&mut search.depth, 1..=7)
                                                    .trailing_fill(true)
                                                    .handle_shape(HandleShape::Rect {
                                                        aspect_ratio: 0.4,
                                                    }),
                                            )
                                            .on_hover_text("Search depth")
                                            .changed();
                                        let favorites = ui
                                            .data_get_persisted::<Locations>()
                                            .unwrap_or_default();
                                        if !favorites.locations.is_empty() {
                                            ui.toggle_value(&mut was_favorites, "💕")
                                                .on_hover_text("Search favorites");
                                            if was_favorites
                                                != current_tab.current_path.multiple_paths()
                                            {
                                                TabAction::SearchInFavorites(was_favorites)
                                                    .schedule_tab(current_tab.id);
                                            }
                                        }
                                    } else if let Some(single_path) =
                                        current_tab.current_path.single_path()
                                    {
                                        let button = egui::Button::new("🖳")
                                            .corner_radius(ButtonGroupElement::First);
                                        if ui
                                            .add_enabled(!is_searching, button)
                                            .on_hover_text("Open in terminal")
                                            .clicked()
                                        {
                                            ActionToPerform::OpenInTerminal(single_path).schedule();
                                        }
                                    }

                                    if search_visible != is_searching {
                                        current_tab.toggle_search(ui.ctx());
                                    }
                                    if search_changed {
                                        TabAction::FilterChanged.schedule_tab(current_tab.id);
                                    }
                                    ui.spacing_mut().item_spacing = spacing;
                                })
                            });

                            ui.add_space(TOP_SIDE_MARGIN);
                            if !is_searching {
                                let Some(current_tab) = self.tabs.get_current_tab() else {
                                    return;
                                };
                                let Some(path) = current_tab.current_path.single_path() else {
                                    return;
                                };
                                let is_editing = ui.data_has_tab::<DirectoryPathInfo>(index);
                                let response = ui.scope_builder(
                                    ui.btn_frame_ui()
                                        .layout(Layout::left_to_right(egui::Align::Min)),
                                    |ui| {
                                        let frame = ui.btn_frame();
                                        frame.show(ui, |ui| {
                                            let spacing = ui.spacing().item_spacing;
                                            ui.spacing_mut().item_spacing =
                                                Vec2::new(1.0, spacing.y);
                                            ui.add_enabled_ui(
                                                path.parent().is_some() && !is_editing,
                                                |ui| {
                                                    let button = Button::new("⬆")
                                                        .corner_radius(ButtonGroupElement::First);
                                                    if ui
                                                        .add(button)
                                                        .on_hover_text("Go to parent directory")
                                                        .clicked()
                                                        && let Some(parent) =
                                                            current_tab.current_path.parent()
                                                    {
                                                        TabAction::ChangePaths(parent.into())
                                                            .schedule_tab(index);
                                                    }
                                                },
                                            );
                                            let response = ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Min),
                                                |ui| {
                                                    if ui
                                                        .add(Button::new("✏").corner_radius(
                                                            ButtonGroupElement::Last,
                                                        ))
                                                        .clicked()
                                                    {
                                                        ActionToPerform::ToggleTopEdit.schedule();
                                                    }
                                                    ui.with_layout(
                                                        Layout::left_to_right(egui::Align::Min),
                                                        |ui| {
                                                            if is_editing {
                                                                Self::top_display_editable(
                                                                    index, &path, ui,
                                                                );
                                                            } else {
                                                                Self::top_display(&current_tab, ui);
                                                            }
                                                        },
                                                    );
                                                },
                                            );

                                            ui.spacing_mut().item_spacing = spacing;
                                            response
                                        })
                                    },
                                );
                                if response.response.double_clicked() {
                                    ActionToPerform::ToggleTopEdit.schedule();
                                }
                            }
                        },
                    );
                });
            });
    }

    pub(crate) fn bottom_panel(&mut self, ctx: &Context) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::bottom_panel");
        let mut sort_changed = false;
        egui::TopBottomPanel::bottom("bottomPanel")
            .frame(egui::Frame::canvas(&ctx.style()))
            .show(ctx, |ui| {
                ui.with_layout(Layout::right_to_left(eframe::emath::Align::Min), |ui| {
                    let spacing = ui.spacing().item_spacing;
                    ui.spacing_mut().item_spacing = Vec2::new(1.0, spacing.y);
                    ui.btn_frame().show(ui, |ui| {
                        if ui
                            .add(egui::Button::new("🔧").corner_radius(ButtonGroupElement::Last))
                            .clicked()
                        {
                            ActionToPerform::ToggleModalWindow(
                                crate::app::commands::ModalWindow::Settings,
                            )
                            .schedule();
                        }
                        if ui
                            .add(
                                egui::Button::new(VERSION)
                                    .frame(false)
                                    .corner_radius(ButtonGroupElement::InTheMiddle),
                            )
                            .on_hover_text(GIT_HASH_INFO)
                            .clicked()
                        {
                            ui.ctx().open_url(OpenUrl::new_tab(HOMEPAGE));
                        }
                        egui::widgets::global_theme_preference_switch(ui);
                        let Some(active_tab) = self.tabs.get_current_tab() else {
                            return;
                        };
                        let mut settings: DirectoryViewSettings =
                            ui.data_get_path_or_persisted(&active_tab.current_path).data;
                        let old_value = settings.sorting;
                        let mut display_hidden_changed: bool = false;

                        egui::ComboBox::from_label("")
                            .selected_text(format!("↕ {:?}", settings.sorting))
                            .show_ui(ui, |ui| {
                                ui.label("Sort by");
                                ui.separator();
                                ui.selectable_value(&mut settings.sorting, Sort::Name, "Name");
                                ui.selectable_value(
                                    &mut settings.sorting,
                                    Sort::Created,
                                    "Created",
                                );
                                ui.selectable_value(
                                    &mut settings.sorting,
                                    Sort::Modified,
                                    "Modified",
                                );
                                ui.selectable_value(&mut settings.sorting, Sort::Size, "Size");
                                ui.selectable_value(&mut settings.sorting, Sort::Random, "Random");
                                ui.separator();

                                sort_changed |= ui
                                    .toggle_value(&mut settings.invert_sort, "Inverted Sorting")
                                    .changed();
                                let mut show_hidden = ui
                                    .data_get_path_or_persisted::<DirectoryShowHidden>(
                                        &active_tab.current_path,
                                    )
                                    .data;
                                display_hidden_changed = ui
                                    .toggle_value(&mut show_hidden.0, "Display hidden files")
                                    .changed();
                                sort_changed |= display_hidden_changed;
                                if display_hidden_changed {
                                    TabAction::RequestFilesRefresh.schedule_tab(active_tab.id);
                                    ui.data_set_path(&active_tab.current_path, show_hidden);
                                }
                            });
                        sort_changed |= old_value != settings.sorting;
                        if !display_hidden_changed && sort_changed {
                            ui.data_set_path(&active_tab.current_path, settings);
                        }
                        ui.spacing_mut().item_spacing = spacing;
                    });
                });
            });

        if sort_changed {
            ActionToPerform::ViewSettingsChanged(crate::app::DataSource::Local).schedule();
        }
    }
}
