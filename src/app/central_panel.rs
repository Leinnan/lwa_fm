use egui::{Context, Layout};

use crate::consts::TOP_SIDE_MARGIN;

use super::App;

impl App {
    #[allow(clippy::too_many_lines)] // todo refactor
    pub(crate) fn central_panel(&mut self, ctx: &Context, search_changed: &mut bool) {
        egui::CentralPanel::default().show(ctx, |ui| {
            let Some(active_tab) = self.tabs.get_current_tab() else {
                return;
            };
            if active_tab.settings.search.visible {
                ui.with_layout(Layout::right_to_left(eframe::emath::Align::Min), |ui| {
                    *search_changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut active_tab.settings.search.value)
                                .hint_text("Search"),
                        )
                        .changed();
                    *search_changed |= ui
                        .add(egui::Slider::new(
                            &mut active_tab.settings.search.depth,
                            1..=7,
                        ))
                        .on_hover_text("Search depth")
                        .changed();
                    *search_changed |= ui
                        .toggle_value(&mut active_tab.settings.search.case_sensitive, "🇨")
                        .on_hover_text("Case sensitive")
                        .changed();
                    *search_changed |= ui
                        .toggle_value(&mut active_tab.settings.search.favorites, "💕")
                        .on_hover_text("Search favorites")
                        .changed();
                });
                ui.add_space(TOP_SIDE_MARGIN);
            }
            self.tabs.ui(ui);
        });
    }
}
