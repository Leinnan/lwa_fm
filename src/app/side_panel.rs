use egui::Context;

use crate::{consts::TOP_SIDE_MARGIN, helper::DataHolder, locations::Locations};

use super::App;

impl App {
    pub(crate) fn left_side_panel(&mut self, ctx: &Context) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("left_side_panel");
        let enabled = self
            .tabs
            .get_current_tab()
            .is_some_and(|tab| !tab.is_searching());
        egui::SidePanel::left("leftPanel")
            .frame(egui::Frame::canvas(&ctx.style()).inner_margin(10.0))
            .show(ctx, |ui| {
                ui.allocate_space([160.0, TOP_SIDE_MARGIN].into());
                ui.add_enabled_ui(enabled, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if let Some(data) = ui.data_get_persisted::<Locations>() {
                            data.draw_ui("Favorites", ui, true);
                        }
                        self.user_locations.draw_ui("User", ui, false);
                        #[cfg(not(target_os = "macos"))]
                        self.drives_locations.draw_ui("Drives", ui, false);
                    });
                });
            });
    }
}
