use std::process::Command;

use egui::{Color32, Context, Layout};

use crate::{
    consts::{GIT_HASH, HOMEPAGE, TOP_SIDE_MARGIN, VERSION},
    toast,
};

use super::{App, NewPathRequest, Sort};

impl App {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn top_panel(&mut self, ctx: &Context, new_path: &mut Option<NewPathRequest>) {
        egui::TopBottomPanel::top("top_panel")
            .frame(egui::Frame::canvas(&ctx.style()))
            .show(ctx, |ui| {
                ui.add_space(TOP_SIDE_MARGIN);
                ui.with_layout(Layout::left_to_right(eframe::emath::Align::Min), |ui| {
                    ui.add_space(TOP_SIDE_MARGIN);
                    let current_path = self.tabs.get_current_path();
                    let parent = current_path.parent();
                    ui.add_enabled_ui(parent.is_some(), |ui| {
                        if ui
                            .button("⬆")
                            .on_hover_text("Go to parent directory")
                            .clicked()
                        {
                            *new_path = Some(NewPathRequest { new_tab: false, path: parent.expect("It should not be possible to click this when parent is None").into() });
                        }
                    });
                    ui.add_space(TOP_SIDE_MARGIN);
                    let mut path: String = String::new();

                    #[allow(unused_variables)] // not used on linux
                    for (i, e) in self.tabs.get_current_path().iter().enumerate() {
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
                                    let Some(s) = e.to_str() else {
                                        return;
                                    };
                                    path += s;
                                    path.push(std::path::MAIN_SEPARATOR);
                                    s
                                }
                            };
                            if ui.button(text).clicked() {
                                *new_path = Some(NewPathRequest { new_tab: false, path: path.into() });
                                return;
                            }
                        }
                        #[cfg(not(windows))]
                        {
                            let shift_pressed = ui.input(|i| i.modifiers.shift);

                            if let Some(part) = e.to_str() {
                                if !part.starts_with('/') && !path.ends_with('/') {
                                    path += "/";
                                }
                                path += part;
                            }
                            let button = ui.button(e.to_string_lossy());
                            if button.clicked() {
                                *new_path = Some(NewPathRequest { new_tab: shift_pressed, path: path.into() });
                                return;
                            }
                            button.context_menu(|ui|{
                                if ui.button("Open").clicked() {
                                        *new_path = Some(NewPathRequest {
                                            new_tab: false,
                                            path: path.clone().into(),
                                        });
                                        ui.close_menu();
                                        return;
                                    }
                                    if ui.button("Open in new tab").clicked() {
                                            *new_path = Some(NewPathRequest {
                                                new_tab: true,
                                                path: path.clone().into(),
                                            });
                                            ui.close_menu();
                                            return;
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
                    }
                    let size_left = ui.available_size();
                    let Some(active_tab) = self.tabs.get_current_tab() else {return;};
                    let amount = if active_tab.dir_has_cargo {
                        size_left.y * 3.0
                    } else {
                        size_left.y * 2.0
                    };
                    let amount = size_left.x - amount;
                    ui.add_space(amount);
                    let text = egui::RichText::new("▶").color(Color32::from_hex("#E2A735").expect("WRONG COLOR"));
                    let button = egui::Button::new(text).frame(false)
                    .fill(egui::Color32::from_white_alpha(0));

                    if active_tab.dir_has_cargo && ui.add(button).on_hover_text("Run project").clicked() {
                        // todo: add possibility to stop it again
                        match Command::new("cargo")
                            .arg("run")
                            .arg("--release")
                            .current_dir(&active_tab.current_path)
                            .spawn()
                        {
                            Ok(_) => {
                                toast!(Success, "Running project");
                            }
                            Err(_) => {
                                toast!(Error, "Failed to run project");
                            }
                        }
                    }
                    ui.toggle_value(&mut active_tab.settings.search.visible, "🔍")
                        .on_hover_text("Search");
                    if ui
                        .toggle_value(&mut active_tab.settings.show_hidden, "👁")
                        .on_hover_text("Display hidden files")
                        .changed()
                    {
                        // self.refresh_list();
                    }
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
                        });
                    *search_changed |= old_value != active_tab.settings.sorting;
                });
                ui.add_space(TOP_SIDE_MARGIN);
            });
    }
}
