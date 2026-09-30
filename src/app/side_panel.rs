use super::{
    App,
    ui::{Palette, PanelLayout},
};
use crate::{helper::DataHolder, locations::Locations};
use egui::Ui;

impl App {
    pub(crate) fn locations_ui(&mut self, ui: &mut Ui) {
        let active = self.tabs.get_current_path();
        egui::ScrollArea::vertical().show(ui, |ui| {
            if let Some(data) = ui.data_get_persisted::<Locations>() {
                data.draw_source_list("Favorites", ui, true, active.as_deref());
            }
            self.user_locations
                .draw_source_list("Locations", ui, false, active.as_deref());
            #[cfg(not(target_os = "macos"))]
            self.drives_locations
                .draw_source_list("Drives", ui, false, active.as_deref());
        });
    }
    pub(crate) fn left_side_panel(&mut self, root: &mut Ui) {
        let ctx = &root.ctx().clone();
        let layout = PanelLayout::new(
            ctx.content_rect().width(),
            self.settings.sidebar_visible,
            self.settings.inspector_visible,
        );
        if !layout.sidebar {
            return;
        }
        let panel = egui::Panel::left("leftPanel")
            .default_size(self.settings.sidebar_width.clamp(160.0, 280.0))
            .size_range(160.0..=280.0)
            .resizable(true)
            .frame(
                egui::Frame::new()
                    .fill(Palette::for_theme(ctx.global_style().visuals.dark_mode).sidebar)
                    .inner_margin(8),
            )
            .show_inside(root, |ui| self.locations_ui(ui));
        self.settings.sidebar_width = panel.response.rect.width().clamp(160.0, 280.0);
    }
}
