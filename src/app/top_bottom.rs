use std::{path::PathBuf, process::Command};

use egui::{Context, Layout};

use crate::{
    consts::{HOMEPAGE, TOP_SIDE_MARGIN, VERSION},
    toast,
};

use super::{App, Sort};

impl App {
    pub(crate) fn top_panel(&mut self, ctx: &Context, new_path: &mut Option<PathBuf>) {
        egui::TopBottomPanel::top("top_panel")
            .frame(egui::Frame::canvas(&ctx.style()))
            .show(ctx, |ui| {
                ui.add_space(TOP_SIDE_MARGIN);
                ui.with_layout(Layout::left_to_right(eframe::emath::Align::Min), |ui| {
                    ui.add_space(TOP_SIDE_MARGIN);
                    if let Some(parent) = self.cur_path.parent() {
                        if ui
                            .button("⬆")
                            .on_hover_text("Go to parent directory")
                            .clicked()
                        {
                            *new_path = Some(parent.into());
                            return;
                        }
                    }
                    ui.add_space(TOP_SIDE_MARGIN);
                    let mut path: String = String::new();

                    #[allow(unused_variables)] // todo fix unwraps here
                    for (i, e) in self.cur_path.iter().enumerate() {
                        #[cfg(windows)]
                        {
                            let text = match &i {
                                0 => {
                                    let last_two_chars: String =
                                        e.to_str().unwrap().chars().rev().take(2).collect();
                                    path += &last_two_chars.chars().rev().collect::<String>();
                                    path.push(std::path::MAIN_SEPARATOR);
                                    continue;
                                }
                                1 => &path,
                                _ => {
                                    path += e.to_str().unwrap();
                                    path.push(std::path::MAIN_SEPARATOR);
                                    e.to_str().unwrap()
                                }
                            };
                            if ui.button(text).clicked() {
                                new_path = Some(path.into());
                                return;
                            }
                        }
                        #[cfg(not(windows))]
                        {
                            if let Some(part) = e.to_str() {
                                if !part.starts_with('/') && !path.ends_with('/') {
                                    path += "/";
                                }
                                path += part;
                            }
                            if ui.button(e.to_string_lossy()).clicked() {
                                *new_path = Some(path.into());
                                return;
                            }
                        }
                    }
                    let size_left = ui.available_size();
                    let amount = if self.dir_has_cargo {
                        size_left.y * 3.0
                    } else {
                        size_left.y * 2.0
                    };
                    let amount = size_left.x - amount;
                    ui.add_space(amount);
                    #[allow(clippy::collapsible_if)]
                    if self.dir_has_cargo && ui.button(">").on_hover_text("Run project").clicked() {
                        match Command::new("cargo").arg("run").arg("--release").spawn() {
                            Ok(_) => {
                                toast!(Success, "Running project");
                            }
                            Err(_) => {
                                toast!(Error, "Failed to run project");
                            }
                        }
                    }
                    ui.toggle_value(&mut self.search.visible, "🔍")
                        .on_hover_text("Search");
                    if ui
                        .toggle_value(&mut self.show_hidden, "👁")
                        .on_hover_text("Display hidden files")
                        .changed()
                    {
                        self.refresh_list();
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
                    egui::widgets::global_dark_light_mode_switch(ui);
                    ui.hyperlink_to(
                        format!("{} v {}", egui::special_emojis::GITHUB, VERSION),
                        HOMEPAGE,
                    );
                    egui::warn_if_debug_build(ui);
                    let old_value = self.sorting;

                    egui::ComboBox::from_label("")
                        .selected_text(format!("↕ {:?}", self.sorting))
                        .show_ui(ui, |ui| {
                            ui.label("Sort by");
                            ui.separator();
                            ui.selectable_value(&mut self.sorting, Sort::Name, "Name");
                            ui.selectable_value(&mut self.sorting, Sort::Created, "Created");
                            ui.selectable_value(&mut self.sorting, Sort::Modified, "Modified");
                            ui.selectable_value(&mut self.sorting, Sort::Size, "Size");
                            ui.selectable_value(&mut self.sorting, Sort::Random, "Random");
                            ui.separator();

                            *search_changed |= ui
                                .toggle_value(&mut self.invert_sort, "Inverted Sorting")
                                .changed();
                        });
                    *search_changed |= old_value != self.sorting;
                });
                ui.add_space(TOP_SIDE_MARGIN);
            });
    }
}
