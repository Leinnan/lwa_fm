use egui::{Context, Layout, RichText};

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
                    let mut locations = self.locations.borrow_mut();
                    for id in ["Favorites", "User", "Drives"] {
                        let Some(collection) = locations.get_mut(id) else {
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
                                                action = match ui.command_pressed() {
                                                    true => ActionToPerform::NewTab(
                                                        location.path.clone(),
                                                    ),
                                                    false => ActionToPerform::ChangePath(
                                                        location.path.clone(),
                                                    ),
                                                }
                                                .into();
                                                return;
                                            }
                                            ui.add_space(10.0);
                                            button.context_menu(|ui| {
                                                if ui.button("Open in new tab").clicked() {
                                                    action = Some(ActionToPerform::NewTab(
                                                        location.path.clone(),
                                                    ));
                                                    ui.close_menu();
                                                    return;
                                                }
                                                if !collection.editable {
                                                    return;
                                                }
                                                if ui.button("Remove from favorites").clicked() {
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
        action
    }
}
