use super::{App, ui::PanelLayout};
use egui::{Shadow, Ui};
impl App {
    pub(crate) fn central_panel(&mut self, root: &mut Ui) {
        let ctx = &root.ctx().clone();
        let layout = PanelLayout::new(
            ctx.content_rect().width(),
            self.settings.sidebar_visible,
            self.settings.inspector_visible,
        );
        let owns_preview = (layout.inspector || self.tabs.window().inspector_overlay)
            && self
                .inspector_view(ctx)
                .is_some_and(|view| view.paths.len() == 1);
        ctx.data_mut(|data| {
            data.insert_temp(egui::Id::unique("inspector_owns_preview"), owns_preview)
        });
        // Reserve the right panel first, then resolve its content after dock interactions.
        let inspector_rect = if layout.inspector {
            let response = egui::Panel::right("file_inspector")
                .default_size(self.settings.inspector_width.clamp(240.0, 360.0))
                .size_range(240.0..=360.0)
                .resizable(true)
                .frame(
                    egui::Frame::new()
                        .fill(
                            egui_mev_theme::Theme::of(ctx)
                                .palette(ctx.global_style().visuals.dark_mode)
                                .sidebar_bg,
                        )
                        .inner_margin(12),
                )
                .show(root, |ui| {
                    let rect = ui.available_rect_before_wrap();
                    ui.allocate_space(rect.size());
                    rect
                });
            self.settings.inspector_width = response.response.rect.width().clamp(240.0, 360.0);
            Some(response.inner)
        } else {
            None
        };
        egui::CentralPanel::default()
            .frame(
                egui::Frame::central_panel(&ctx.global_style())
                    .shadow(Shadow::NONE)
                    .inner_margin(egui::Margin::ZERO)
                    .outer_margin(egui::Margin::ZERO),
            )
            .show(root, |ui| self.tabs.ui(ui, &mut self.assets));
        if let Some(rect) = inspector_rect {
            egui::Area::new(egui::Id::unique("inspector_content"))
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
