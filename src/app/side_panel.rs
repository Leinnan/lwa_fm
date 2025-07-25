use std::{path::PathBuf, str::FromStr};

use egui::{Context, Layout, RichText, TextBuffer};

use crate::{consts::TOP_SIDE_MARGIN, helper::KeyWithCommandPressed};

use super::{ActionToPerform, App};

impl App {
    pub(crate) fn left_side_panel(&self, ctx: &Context) -> Option<ActionToPerform> {
        let mut action = None;
        egui::SidePanel::left("leftPanel")
            .frame(egui::Frame::canvas(&ctx.style()))
            .show(ctx, |ui| {
                ui.allocate_space([160.0, TOP_SIDE_MARGIN].into());
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (id, collection) in &self.locations {
                        if collection.locations.is_empty() {
                            continue;
                        }
                        egui::CollapsingHeader::new(id.as_str())
                            .default_open(true)
                            .show(ui, |ui| {
                                for location in &collection.locations {
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
                                                action = if ui.command_pressed() {
                                                    ActionToPerform::NewTab(
                                                        PathBuf::from_str(&location.path).unwrap(),
                                                    )
                                                } else {
                                                    ActionToPerform::ChangePath(
                                                        PathBuf::from_str(&location.path).unwrap(),
                                                    )
                                                }
                                                .into();
                                                return;
                                            }
                                            ui.add_space(10.0);
                                            button.context_menu(|ui| {
                                                if ui.button("Open in new tab").clicked() {
                                                    action = Some(ActionToPerform::NewTab(
                                                        PathBuf::from_str(&location.path).unwrap(),
                                                    ));
                                                    ui.close_menu();
                                                    return;
                                                }
                                                if !collection.editable {
                                                    return;
                                                }
                                                if ui.button("Remove from favorites").clicked() {
                                                    action =
                                                        Some(ActionToPerform::RemoveFromFavorites(
                                                            location.path.clone(),
                                                        ));
                                                    ui.close_menu();
                                                }
                                            });
                                        },
                                    );
                                }
                            });
                        ui.add_space(15.0);
                    }
                });
            });
        action
    }
}
