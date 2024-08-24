use std::path::PathBuf;

use egui::{Context, Layout, RichText};

use crate::consts::TOP_SIDE_MARGIN;

use super::App;

impl App {
    pub(crate) fn left_side_panel(&mut self, ctx: &Context, new_path: &mut Option<PathBuf>) {
        egui::SidePanel::left("leftPanel")
            .frame(egui::Frame::canvas(&ctx.style()))
            .show(ctx, |ui| {
                ui.allocate_space([160.0, TOP_SIDE_MARGIN].into());
                ui.with_layout(Layout::top_down(eframe::emath::Align::Min), |ui| {
                    for id in ["Favorites", "User", "Drives"] {
                        let Some(collection) = self.locations.get_mut(id) else {
                            continue;
                        };
                        if collection.locations.is_empty() {
                            continue;
                        }
                        egui::CollapsingHeader::new(id)
                            .default_open(true)
                            .show(ui, |ui| {
                                let mut id_to_remove = None;
                                for (i, location) in collection.locations.iter().enumerate() {
                                    ui.with_layout(
                                        Layout::left_to_right(eframe::emath::Align::Min),
                                        |ui| {
                                            let button = ui.add(
                                                egui::Button::new(
                                                    RichText::new(&location.name).strong(),
                                                )
                                                .frame(false)
                                                .fill(egui::Color32::from_white_alpha(0)),
                                            );
                                            if button.clicked() {
                                                *new_path = Some(location.path.clone());
                                                return;
                                            }
                                            ui.add_space(10.0);
                                            if !collection.editable {
                                                return;
                                            }
                                            button.context_menu(|ui| {
                                                if ui.button("Remove").clicked() {
                                                    id_to_remove = Some(i);
                                                    ui.close_menu();
                                                }
                                            });
                                        },
                                    );
                                }
                                if let Some(id) = id_to_remove {
                                    collection.locations.remove(id);
                                }
                            });
                        ui.add_space(15.0);
                    }
                });
            });
    }
}
