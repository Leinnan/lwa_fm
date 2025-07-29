use egui::Context;

use crate::{consts::TOP_SIDE_MARGIN, helper::DataHolder, locations::Locations};

use super::{ActionToPerform, App};

impl App {
    pub(crate) fn left_side_panel(&self, ctx: &Context) -> Option<ActionToPerform> {
        let mut action = None;
        egui::SidePanel::left("leftPanel")
            .frame(egui::Frame::canvas(&ctx.style()))
            .show(ctx, |ui| {
                ui.allocate_space([160.0, TOP_SIDE_MARGIN].into());
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if let Some(data) = ui.data_get_persisted::<Locations>() {
                        action = data.draw_ui("Favorites", ui, true);
                    }
                    if action.is_none() {
                        action = self.user_locations.draw_ui("User", ui, false);
                    }
                    #[cfg(not(target_os = "macos"))]
                    if action.is_none() {
                        action = self.drives_locations.draw_ui("Drives", ui, false);
                    }
                });
            });
        action
    }
}
