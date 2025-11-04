use egui::{Context, Shadow};

use super::App;

impl App {
    #[allow(clippy::too_many_lines)] // todo refactor
    pub(crate) fn central_panel(&mut self, ctx: &Context) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("central_panel");
        let frame = egui::Frame::central_panel(&ctx.style())
            .shadow(Shadow::NONE)
            .inner_margin(egui::Margin::ZERO)
            .outer_margin(egui::Margin::ZERO);
        let response = egui::CentralPanel::default()
            .frame(frame)
            .show(ctx, |ui| {
                self.tabs.ui(ui);
            })
            .response;
        let focused = response.has_focus() || response.hovered();
        self.tabs.focused = self.display_modal.is_none() && focused;
    }
}
