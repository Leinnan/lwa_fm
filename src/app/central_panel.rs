use egui::{Context, Shadow};

use super::{ActionToPerform, App};

impl App {
    #[allow(clippy::too_many_lines)] // todo refactor
    pub(crate) fn central_panel(&mut self, ctx: &Context) -> Option<ActionToPerform> {
        let mut action_to_perform = None;
        let frame = egui::Frame::central_panel(&ctx.style())
            .shadow(Shadow::NONE)
            .inner_margin(egui::Margin::ZERO)
            .outer_margin(egui::Margin::ZERO);
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            action_to_perform = self.tabs.ui(ui);
        });
        action_to_perform
    }
}
