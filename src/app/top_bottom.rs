use std::{
    path::{Path, PathBuf},
    str::FromStr,
};

use super::{ActionToPerform, App, Sort};
use crate::{
    app::{dir_handling::get_directories_recursive, directory_path_info::DirectoryPathInfo},
    consts::{GIT_HASH_INFO, HOMEPAGE, TOP_SIDE_MARGIN, VERSION},
    helper::DataHolder,
    locations::Locations,
    widgets::{ButtonGroupElement, UiBuilderExt},
};
use egui::{style::HandleShape, Button, Context, Layout, OpenUrl, Ui, Vec2};

#[allow(clippy::too_many_lines)]
impl App {
    pub(crate) fn top_display_editable(
        index: u32,
        current_path: &Path,
        ui: &mut Ui,
    ) -> Option<ActionToPerform> {
        use crate::widgets::autocomplete_text::AutoCompleteTextEdit;
        let size = ui.available_size();
        let mut directory_info = ui.data_get_tab::<DirectoryPathInfo>(index)?;

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

        let mut action = None;
        let should_close =
            ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape));
        if should_close {
            action = Some(ActionToPerform::ToggleTopEdit);
        } else {
            let path = Path::new(&directory_info.text_input);
            if path.exists() && path.is_dir() && !path.eq(current_path) {
                action = ActionToPerform::path_from_str(directory_info.text_input.clone(), false);
            }
        }
        ui.data_set_tab(index, directory_info);
        action
    }
    #[allow(clippy::needless_pass_by_ref_mut)]
    pub(crate) fn top_display(current_path: &Path, ui: &mut Ui) -> Option<ActionToPerform> {
        let mut new_path = None;
        let mut path: String = String::new();
        let parts = current_path.iter().count();

        #[allow(unused_variables)] // not used on linux
        for (i, e) in current_path.iter().enumerate() {
            let button_group = if i == parts - 1 {
                ButtonGroupElement::Last
            } else {
                ButtonGroupElement::InTheMiddle
            };
            #[cfg(windows)]
            {
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
                    1 => &path,
                    _ => {
                        let s = e.to_str()?;
                        path += s;
                        path.push(std::path::MAIN_SEPARATOR);
                        s
                    }
                };
                if ui.button(text).clicked() {
                    new_path = Some(ActionToPerform::ChangePath(path.into()));
                    return new_path;
                }
            }
            #[cfg(not(windows))]
            {
                use crate::helper::KeyWithCommandPressed;
                let Some(part) = e.to_str() else {
                    continue;
                };
                if !part.starts_with('/') && !path.ends_with('/') {
                    path += "/";
                }
                path += part;
                let button = ui.add(Button::new(part).corner_radius(button_group));
                if button.clicked() {
                    new_path = ActionToPerform::path_from_str(&path, ui.command_pressed());
                    return new_path;
                }
                button.context_menu(|ui| {
                    if ui.button("Open").clicked() {
                        new_path = ActionToPerform::path_from_str(&path, false);
                        ui.close();
                    }
                    if ui.button("Open in new tab").clicked() {
                        new_path = ActionToPerform::path_from_str(&path, true);
                        ui.close();
                    }
                });
                if button_group != ButtonGroupElement::Last {
                    let dirs = get_directories_recursive(std::path::Path::new(&path), false, 1);
                    if !dirs.is_empty() {
                        let button =
                            ui.add(Button::new(">").corner_radius(ButtonGroupElement::InTheMiddle));
                        button.context_menu(|ui| {
                            egui::ScrollArea::vertical().show(ui, |ui| {
                                for dir in &dirs {
                                    let dir_display = dir.replace(&path, "");
                                    if dir_display.is_empty() {
                                        continue;
                                    }
                                    if ui.button(dir_display.as_str()).clicked() {
                                        new_path = Some(ActionToPerform::ChangePaths(
                                            PathBuf::from_str(dir)
                                                .expect("Failed to convert path")
                                                .into(),
                                        ));
                                        ui.close();
                                    }
                                }
                            });
                        });
                    }
                }
            }
            #[cfg(windows)]
            if parts - 1 != i {
                ui.menu_button(std::path::MAIN_SEPARATOR.to_string(), |ui| {
                    let p = std::path::Path::new(&path);
                    let dirs = get_directories_recursive(p, false, 1);
                    if dirs.is_empty() {
                        ui.close();
                    }
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for dir in &dirs {
                            let dir_display = dir.replace(&path, "");
                            if dir_display.is_empty() {
                                continue;
                            }
                            if ui.button(dir_display.as_str()).clicked() {
                                new_path = Some(ActionToPerform::ChangePath(
                                    PathBuf::from_str(dir).expect("Failed to convert path"),
                                ));
                                ui.close();
                            }
                        }
                    });
                });
            }
        }
        new_path
    }

    fn undo_redo_up(&mut self, ui: &mut Ui) -> Option<ActionToPerform> {
        let current_tab = self.tabs.get_current_tab()?;
        let mut action = None;
        ui.btn_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                let spacing = ui.spacing().item_spacing;
                ui.spacing_mut().item_spacing = Vec2::ZERO;

                ui.add_enabled_ui(current_tab.can_undo(), |ui| {
                    let button = Button::new("⮪").corner_radius(ButtonGroupElement::First);
                    if ui.add(button).on_hover_text("Go back").clicked() {
                        action = current_tab.undo();
                    }
                });
                ui.add_enabled_ui(current_tab.can_redo(), |ui| {
                    let button = Button::new("⮫").corner_radius(ButtonGroupElement::Last);
                    if ui.add(button).on_hover_text("Redo").clicked() {
                        action = current_tab.redo();
                    }
                });
                ui.spacing_mut().item_spacing = spacing;
            });
        });
        action
    }

    pub(crate) fn top_panel(&mut self, ctx: &Context) -> Option<ActionToPerform> {
        let is_searching = self
            .tabs
            .get_current_tab()
            .is_some_and(|tab| tab.search.is_some());
        let index = self.tabs.get_current_index().unwrap_or_default();
        let mut action = None;
        egui::TopBottomPanel::top("top_panel")
            .frame(egui::Frame::canvas(&ctx.style()))
            .show(ctx, |ui| {
                ui.with_layout(Layout::left_to_right(egui::Align::Min), |ui| {
                    ui.add_enabled_ui(!is_searching, |ui| {
                        action = self.undo_redo_up(ui);
                    });
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
                                    if let Some(search) = &mut current_tab.search {
                                        let mut search_changed = ui
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
                                        if search_changed {
                                            action = Some(ActionToPerform::FilterChanged);
                                        } else if !favorites.locations.is_empty() {
                                            ui.toggle_value(&mut was_favorites, "💕")
                                                .on_hover_text("Search favorites");
                                            if was_favorites
                                                != current_tab.current_path.multiple_paths()
                                            {
                                                action = Some(ActionToPerform::SearchInFavorites(
                                                    was_favorites,
                                                ));
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
                                            action =
                                                Some(ActionToPerform::OpenInTerminal(single_path));
                                        }
                                    }

                                    if search_visible != is_searching {
                                        current_tab.toggle_search(ui.ctx());
                                    }
                                    ui.spacing_mut().item_spacing = spacing;
                                })
                            });

                            ui.add_space(TOP_SIDE_MARGIN);
                            if action.is_none() && !is_searching {
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
                                                    {
                                                        action = current_tab
                                                            .current_path
                                                            .parent()
                                                            .map(|s| {
                                                                ActionToPerform::ChangePaths(
                                                                    s.into(),
                                                                )
                                                            });
                                                    }
                                                },
                                            );
                                            let response = ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Min),
                                                |ui| {
                                                    if action.is_none()
                                                        && ui
                                                            .add(Button::new("✏").corner_radius(
                                                                ButtonGroupElement::Last,
                                                            ))
                                                            .clicked()
                                                    {
                                                        action =
                                                            Some(ActionToPerform::ToggleTopEdit);
                                                    }
                                                    if action.is_none() {
                                                        ui.with_layout(
                                                            Layout::left_to_right(egui::Align::Min),
                                                            |ui| {
                                                                action = if is_editing {
                                                                    Self::top_display_editable(
                                                                        index, &path, ui,
                                                                    )
                                                                } else {
                                                                    Self::top_display(&path, ui)
                                                                };
                                                            },
                                                        );
                                                    }
                                                },
                                            );

                                            ui.spacing_mut().item_spacing = spacing;
                                            response
                                        })
                                    },
                                );
                                if response.response.double_clicked() {
                                    action = Some(ActionToPerform::ToggleTopEdit);
                                }
                            }
                        },
                    );
                });
            });
        action
    }

    pub(crate) fn bottom_panel(&mut self, ctx: &Context) -> Option<ActionToPerform> {
        let mut search_changed = false;
        let mut action = None;
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
                            action = Some(ActionToPerform::ToggleModalWindow(
                                crate::app::commands::ModalWindow::Settings,
                            ));
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
                        // egui::widgets::global_theme_preference_switch(ui);
                        let Some(active_tab) = self.tabs.get_current_tab() else {
                            return;
                        };
                        let old_value = active_tab.settings.sorting;

                        egui::ComboBox::from_label("")
                            .selected_text(format!("↕ {:?}", active_tab.settings.sorting))
                            .show_ui(ui, |ui| {
                                ui.label("Sort by");
                                ui.separator();
                                ui.selectable_value(
                                    &mut active_tab.settings.sorting,
                                    Sort::Name,
                                    "Name",
                                );
                                ui.selectable_value(
                                    &mut active_tab.settings.sorting,
                                    Sort::Created,
                                    "Created",
                                );
                                ui.selectable_value(
                                    &mut active_tab.settings.sorting,
                                    Sort::Modified,
                                    "Modified",
                                );
                                ui.selectable_value(
                                    &mut active_tab.settings.sorting,
                                    Sort::Size,
                                    "Size",
                                );
                                ui.selectable_value(
                                    &mut active_tab.settings.sorting,
                                    Sort::Random,
                                    "Random",
                                );
                                ui.separator();

                                search_changed |= ui
                                    .toggle_value(
                                        &mut active_tab.settings.invert_sort,
                                        "Inverted Sorting",
                                    )
                                    .changed();

                                search_changed |= ui
                                    .toggle_value(
                                        &mut active_tab.settings.show_hidden,
                                        "Display hidden files",
                                    )
                                    .changed();
                            });
                        search_changed |= old_value != active_tab.settings.sorting;
                        ui.spacing_mut().item_spacing = spacing;
                    });
                });
            });

        if action.is_none() && search_changed {
            dbg!(search_changed);
            action = Some(ActionToPerform::ViewSettingsChanged(
                crate::app::DataSource::Local,
            ));
        }
        action
    }
}
