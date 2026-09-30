use super::{
    App,
    ui::{Palette, PanelLayout},
};
use egui::{Shadow, Ui};
impl App {
    pub(crate) fn central_panel(&mut self, root: &mut Ui) {
        let ctx = &root.ctx().clone();
        let layout = PanelLayout::new(
            ctx.content_rect().width(),
            self.settings.sidebar_visible,
            self.settings.inspector_visible,
        );
        ctx.data_mut(|data| {
            data.insert_temp(
                egui::Id::new("inspector_owns_preview"),
                layout.inspector || self.inspector_overlay,
            )
        });
        // Reserve the right panel first, then resolve its content after dock interactions.
        let inspector_rect = if layout.inspector {
            let response = egui::Panel::right("file_inspector")
                .default_size(self.settings.inspector_width.clamp(240.0, 360.0))
                .size_range(240.0..=360.0)
                .resizable(true)
                .frame(
                    egui::Frame::new()
                        .fill(Palette::for_theme(ctx.global_style().visuals.dark_mode).sidebar)
                        .inner_margin(12),
                )
                .show_inside(root, |ui| {
                    let rect = ui.available_rect_before_wrap();
                    ui.allocate_space(rect.size());
                    rect
                });
            self.settings.inspector_width = response.response.rect.width().clamp(240.0, 360.0);
            Some(response.inner)
        } else {
            None
        };
        let response = egui::CentralPanel::default()
            .frame(
                egui::Frame::central_panel(&ctx.global_style())
                    .shadow(Shadow::NONE)
                    .inner_margin(egui::Margin::ZERO)
                    .outer_margin(egui::Margin::ZERO),
            )
            .show_inside(root, |ui| self.tabs.ui(ui, &mut self.assets))
            .response;
        self.tabs.focused = self.display_modal.is_none()
            && !self.sidebar_overlay
            && !self.inspector_overlay
            && (response.has_focus() || response.hovered());
        if let Some(rect) = inspector_rect {
            egui::Area::new("inspector_content".into())
                .fixed_pos(rect.min)
                .order(egui::Order::Middle)
                .movable(false)
                .constrain(false)
                .show(ctx, |ui| {
                    ui.set_width(rect.width());
                    ui.set_height(rect.height());
                    ui.set_clip_rect(rect);
                    self.inspector_ui(ui);
                });
        }
    }
}
