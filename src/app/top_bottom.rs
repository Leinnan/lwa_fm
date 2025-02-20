use std::{path::PathBuf, process::Command, str::FromStr};

use super::{App, NewPathRequest, Sort};
use crate::{
    app::dir_handling::{get_directories, get_directories_recursive},
    consts::{GIT_HASH, HOMEPAGE, TOP_SIDE_MARGIN, VERSION},
    helper::PathFixer,
    toast,
};
use egui::{Context, Layout, Ui};

#[allow(clippy::too_many_lines)]
impl App {
    pub(crate) fn top_display_editable(
        &mut self,
        ui: &mut Ui,
        show_hidden: bool,
    ) -> Option<NewPathRequest> {
        use crate::widgets::autocomplete_text::AutoCompleteTextEdit;
        let edit = AutoCompleteTextEdit::new(&mut self.top_edit, &self.possible_options)
            .max_suggestions(20)
            .highlight_matches(true);
        let _response = ui.add(edit);

        let should_close =
            ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape));
        if should_close {
            self.display_edit_top = false;
        }

        let top_edit_path = std::path::Path::new(&self.top_edit);
        if top_edit_path.exists()
            && !self
                .possible_options
                .first()
                .is_some_and(|first| first.eq(&self.top_edit))
        {
            self.possible_options = get_directories(top_edit_path, show_hidden);
            return Some(NewPathRequest {
                new_tab: false,
                path: top_edit_path.to_path_buf(),
            });
        }
        None
    }
    pub(crate) fn top_display(&mut self, ui: &mut Ui) -> Option<NewPathRequest> {
        let mut new_path = None;
        let mut path: String = String::new();
        let current_path = self.tabs.get_current_path();
        let parts = current_path.iter().count();
        #[allow(unused_variables)] // not used on linux
        for (i, e) in current_path.iter().enumerate() {
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
                    new_path = Some(NewPathRequest {
                        new_tab: false,
                        path: path.into(),
                    });
                    return new_path;
                }
            }
            #[cfg(not(windows))]
            {
                let shift_pressed = ui.input(|i| i.modifiers.shift);

                let Some(part) = e.to_str() else {
                    continue;
                };
                if !part.starts_with('/') && !path.ends_with('/') {
                    path += "/";
                }
                path += part;
                let button = ui.button(part);
                if button.clicked() {
                    new_path = Some(NewPathRequest {
                        new_tab: shift_pressed,
                        path: path.into(),
                    });
                    return new_path;
                }
                button.context_menu(|ui| {
                    if ui.button("Open").clicked() {
                        new_path = Some(NewPathRequest {
                            new_tab: false,
                            path: path.clone().into(),
                        });
                        ui.close_menu();
                    }
                    if ui.button("Open in new tab").clicked() {
                        new_path = Some(NewPathRequest {
                            new_tab: true,
                            path: path.clone().into(),
                        });
                        ui.close_menu();
                    }
                    if ui.button("Copy path to clipboard").clicked() {
                        let Ok(mut clipboard) = arboard::Clipboard::new() else {
                            toast!(Error, "Failed to read the clipboard.");
                            return;
                        };
                        clipboard.set_text(path.clone()).unwrap_or_else(|_| {
                            toast!(Error, "Failed to update the clipboard.");
                        });
                        ui.close_menu();
                    }
                });
            }
            if parts - 1 != i {
                ui.menu_button(std::path::MAIN_SEPARATOR.to_string(), |ui| {
                    let p = std::path::Path::new(&path);
                    let dirs = get_directories_recursive(p, false, 1);
                    if dirs.is_empty() {
                        ui.close_menu();
                    }
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for dir in &dirs {
                            let dir_display = dir.replace(&path, "");
                            if dir_display.is_empty() {
                                continue;
                            }
                            if ui.button(dir_display.as_str()).clicked() {
                                new_path = Some(NewPathRequest {
                                    new_tab: false,
                                    path: PathBuf::from_str(dir).expect("Failed to convert path"),
                                });
                                ui.close_menu();
                            }
                        }
                    });
                });
            }
        }
        new_path
    }

    pub(crate) fn top_panel(&mut self, ctx: &Context, new_path: &mut Option<NewPathRequest>) {
        egui::TopBottomPanel::top("top_panel")
            .frame(egui::Frame::canvas(&ctx.style()))
            .show(ctx, |ui| {
                let Some(current_tab) = self.tabs.get_current_tab() else {return;};
                let show_hidden = current_tab.settings.show_hidden;
                let is_searching = current_tab.settings.is_searching();
                let current_path = current_tab.current_path.clone();
                let parent = current_path.parent();

                ui.add_space(TOP_SIDE_MARGIN);
                ui.with_layout(Layout::left_to_right(eframe::emath::Align::Min), |ui| {
                    ui.add_space(TOP_SIDE_MARGIN);
                    ui.add_enabled_ui(parent.is_some() && !self.display_edit_top && !is_searching, |ui| {
                        if ui
                            .button("⬆")
                            .on_hover_text("Go to parent directory")
                            .clicked()
                        {
                            *new_path = Some(NewPathRequest { new_tab: false, path: parent.expect("It should not be possible to click this when parent is None").into() });
                        }
                    });
                    ui.add_space(TOP_SIDE_MARGIN);
                    ui.add_enabled_ui(!is_searching, |ui|{                    if ui.button("✏").clicked() {
                        self.display_edit_top = !self.display_edit_top;
                        if self.display_edit_top {
                            self.top_edit = current_path.to_fixed_string();
                            self.possible_options = get_directories(parent.unwrap_or(current_path.as_path()), show_hidden);
                        }
                    }
                    *new_path = if self.display_edit_top {
                        self.top_display_editable(ui, show_hidden)
                    } else {
                        self.top_display(ui)
                    };
                    });


                    let size_left = ui.available_size();
                    let Some(active_tab) = self.tabs.get_current_tab() else {return;};
                    let amount = size_left.y * 2.0;
                    let amount = size_left.x - amount;
                    ui.add_space(amount);

                    let button = egui::Button::new("🖳").frame(false)
                        .fill(egui::Color32::from_white_alpha(0));
                    if ui.add(button).on_hover_text("Open in terminal").clicked() {
                        match Command::new(&self.settings.terminal_path)
                            .current_dir(&active_tab.current_path).spawn()
                        {
                            Ok(_) => {
                                toast!(Success, "Open in terminal");
                            }
                            Err(_) => {
                                toast!(Error, "Failed to open directory");
                            }
                        }

                    }
                    ui.toggle_value(&mut active_tab.settings.search.visible, "🔍")
                        .on_hover_text("Search");
                });
                ui.add_space(TOP_SIDE_MARGIN);
            });
    }

    pub(crate) fn bottom_panel(&mut self, ctx: &Context, search_changed: &mut bool) {
        egui::TopBottomPanel::bottom("bottomPanel")
            .frame(egui::Frame::canvas(&ctx.style()))
            .show(ctx, |ui| {
                ui.add_space(TOP_SIDE_MARGIN);
                ui.with_layout(Layout::right_to_left(eframe::emath::Align::Min), |ui| {
                    egui::widgets::global_theme_preference_switch(ui);
                    ui.hyperlink_to(
                        format!("{} v {}", egui::special_emojis::GITHUB, VERSION),
                        HOMEPAGE,
                    )
                    .on_hover_text(format!("git revision {GIT_HASH}"));
                    egui::warn_if_debug_build(ui);
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

                            *search_changed |= ui
                                .toggle_value(
                                    &mut active_tab.settings.invert_sort,
                                    "Inverted Sorting",
                                )
                                .changed();

                            *search_changed |= ui
                                .toggle_value(
                                    &mut active_tab.settings.show_hidden,
                                    "Display hidden files",
                                )
                                .changed();
                        });
                    *search_changed |= old_value != active_tab.settings.sorting;
                });
                ui.add_space(TOP_SIDE_MARGIN);
            });
    }
}
