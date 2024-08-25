use std::{ffi::OsStr, path::PathBuf};

use egui::{Context, Layout, RichText, Vec2};
use egui_extras::{Column, TableBuilder};

use crate::{
    consts::{TOP_SIDE_MARGIN, VERTICAL_SPACING},
    locations::Location,
    toast,
};

use super::App;

impl App {
    #[allow(clippy::too_many_lines)] // todo refactor
    pub(crate) fn central_panel(
        &mut self,
        ctx: &Context,
        search_changed: &mut bool,
        new_path: &mut Option<PathBuf>,
    ) {
        egui::CentralPanel::default().show(ctx, |ui| {
            let text_height = egui::TextStyle::Body.resolve(ui.style()).size * 2.0;
            if self.search.visible {
                ui.with_layout(Layout::right_to_left(eframe::emath::Align::Min), |ui| {
                    *search_changed |= ui
                        .add(egui::TextEdit::singleline(&mut self.search.value).hint_text("Search"))
                        .changed();
                    *search_changed |= ui
                        .add(egui::Slider::new(&mut self.search.depth, 1..=7))
                        .on_hover_text("Search depth")
                        .changed();
                    *search_changed |= ui
                        .toggle_value(&mut self.search.case_sensitive, "🇨")
                        .on_hover_text("Case sensitive")
                        .changed();
                    *search_changed |= ui
                        .toggle_value(&mut self.search.favorites, "💕")
                        .on_hover_text("Search favorites")
                        .changed();
                });
                ui.add_space(TOP_SIDE_MARGIN);
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                let table = TableBuilder::new(ui)
                    .striped(true)
                    .vscroll(false)
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::remainder().at_least(260.0))
                    .resizable(false);
                table.body(|body| {
                    body.rows(text_height, self.list.len(), |mut row| {
                        let val = &self.list[row.index()];
                        let Ok(meta) = val.metadata() else {
                            return;
                        };
                        row.col(|ui| {
                            ui.add_space(VERTICAL_SPACING);
                            // ui.allocate_space(egui::vec2(available_width, 20.0));
                            let file_type = meta.file_type();
                            #[cfg(target_os = "windows")]
                            let is_dir = {
                                use std::os::windows::fs::FileTypeExt;
                                file_type.is_dir() || file_type.is_symlink_dir()
                            };
                            #[cfg(not(target_os = "windows"))]
                            let is_dir = file_type.is_dir();
                            let text = val.file_name().to_string_lossy();

                            let text = if is_dir {
                                RichText::new(text)
                            } else {
                                RichText::strong(text.into())
                            };
                            let added_button = ui.add(
                                egui::Button::new(text).fill(egui::Color32::from_white_alpha(0)),
                            );

                            if added_button.clicked() {
                                if meta.is_file() {
                                    let _ = open::that_detached(val.path());
                                } else {
                                    let Ok(path) = std::fs::canonicalize(val.path()) else {
                                        return;
                                    };
                                    *new_path = Some(path);
                                }
                            }
                            added_button.context_menu(|ui| {
                                if is_dir {
                                    #[cfg(windows)]
                                    if ui.button("Open in explorer").clicked() {
                                        crate::windows_tools::open_in_explorer(val.path(), true)
                                            .unwrap_or_else(|_| {
                                                toast!(Error, "Could not open in explorer");
                                            });
                                        ui.close_menu();
                                        return;
                                    }
                                    #[cfg(target_os="macos")]
                                    if ui.button("Open in Finder").clicked() {
                                        open::that_detached(val.path()).expect("Failed to open dir in Finder");
                                        ui.close_menu();
                                        return;
                                    }
                                    let Ok(path) = std::fs::canonicalize(val.path()) else {
                                        return;
                                    };

                                    let existing_path =
                                        self.locations.get("Favorites").and_then(|favorites| {
                                            favorites
                                                .locations
                                                .iter()
                                                .enumerate()
                                                .find(|(_i, loc)| path.ends_with(&loc.path))
                                                .map(|(i, _)| i)
                                        });

                                    if existing_path.is_none()
                                        && ui.button("Add to favorites").clicked()
                                    {
                                        if let Some(name) = path.iter().last() {
                                            if let Some(fav) = self.locations.get_mut("Favorites") {
                                                fav.locations.push(Location {
                                                    name: name.to_string_lossy().to_string(),
                                                    path,
                                                });
                                            }
                                        } else {
                                            toast!(Error, "Could not get name of file");
                                        }

                                        ui.close_menu();
                                        return;
                                    }

                                    if existing_path.is_some()
                                        && ui.button("Remove from favorites").clicked()
                                    {
                                        if let Some(fav) = self.locations.get_mut("Favorites") {
                                            if let Some(existing_path) = existing_path {
                                                fav.locations.remove(existing_path);
                                            }
                                        }
                                        ui.close_menu();
                                    }
                                } else {
                                    #[cfg(windows)]
                                    if ui.button("Show in explorer").clicked() {
                                        crate::windows_tools::open_in_explorer(val.path(), false)
                                            .unwrap_or_else(|_| {
                                                toast!(Error, "Could not open in explorer");
                                            });
                                        ui.close_menu();
                                    }
                                }
                                #[cfg(windows)]
                                if ui.button("Properties").clicked() {
                                    crate::windows_tools::open_properties(val.path());
                                    ui.close_menu();
                                }
                            });
                            let ext = val
                                .path()
                                .extension()
                                .unwrap_or_default()
                                .to_ascii_lowercase();
                            if ext.eq(&OsStr::new("png"))
                                || ext.eq(&OsStr::new("jpg"))
                                || ext.eq(&OsStr::new("jpeg"))
                            {
                                added_button.on_hover_ui(|ui| {
                                    let path = std::fs::canonicalize(val.path())
                                        .unwrap_or_else(|_| val.path().to_path_buf())
                                        .to_string_lossy()
                                        .replace("\\\\?\\", "");
                                    let path = format!("file://{path}");
                                    ui.add(
                                        egui::Image::new(path)
                                            .maintain_aspect_ratio(true)
                                            .max_size(Vec2::new(300.0, 300.0)),
                                    );
                                });
                            } else {
                                added_button.on_hover_text(format!(
                                    "{:?}",
                                    // consider caching here
                                    std::fs::canonicalize(val.path())
                                        .unwrap_or_else(|_| val.path().to_path_buf())
                                ));
                            }
                        });
                    });
                });
            });
        });
    }
}
