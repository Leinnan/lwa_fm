use std::{ffi::OsStr, path::PathBuf};

use egui::{Context, Layout, RichText, Vec2};
use egui_extras::{Column, TableBuilder};

use crate::{
    consts::{TOP_SIDE_MARGIN, VERTICAL_SPACING},
    locations::Location,
    toast,
};

use super::App;

impl App {
    #[allow(clippy::too_many_lines)] // todo refactor
    pub(crate) fn central_panel(
        &mut self,
        ctx: &Context,
        search_changed: &mut bool,
        new_path: &mut Option<PathBuf>,
    ) {
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.search.visible {
                ui.with_layout(Layout::right_to_left(eframe::emath::Align::Min), |ui| {
                    *search_changed |= ui
                        .add(egui::TextEdit::singleline(&mut self.search.value).hint_text("Search"))
                        .changed();
                    *search_changed |= ui
                        .add(egui::Slider::new(&mut self.search.depth, 1..=7))
                        .on_hover_text("Search depth")
                        .changed();
                    *search_changed |= ui
                        .toggle_value(&mut self.search.case_sensitive, "🇨")
                        .on_hover_text("Case sensitive")
                        .changed();
                    *search_changed |= ui
                        .toggle_value(&mut self.search.favorites, "💕")
                        .on_hover_text("Search favorites")
                        .changed();
                });
                ui.add_space(TOP_SIDE_MARGIN);
            }
            self.tabs.ui(ui);
        });
    }
}
