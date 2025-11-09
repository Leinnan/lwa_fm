use egui::{Color32, FontId, Layout, Response, Sense, Ui, Widget, epaint, text::LayoutJob};

use crate::data::time::{ElapsedTime, TimestampSeconds};

impl Widget for ElapsedTime {
    #[inline]
    fn ui(self, ui: &mut Ui) -> Response {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::MyTabViewer::ui::table_body::time_column::time_label");
        let Some(galley) = crate::app::dock::TIME_POOL.with_borrow(|pool| pool.get(&self).cloned())
        else {
            return ui.response();
        };

        // If the user said "use this specific galley", then just use it:
        let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::hover());
        let galley_pos = rect.center_top();

        if ui.is_rect_visible(response.rect) {
            let response_color = ui.style().visuals.text_color();
            ui.painter().add(
                epaint::TextShape::new(galley_pos, galley, response_color), // .with_underline(underline),
            );
        }

        response
    }
}
#[inline]
pub fn draw_size(ui: &mut Ui, size: u32) -> Response {
    #[cfg(feature = "profiling")]
    puffin::profile_scope!("lwa_fm::MyTabViewer::ui::table_body::size_column::draw_size");
    let Some(galley) = crate::app::dock::SIZES_POOL.with_borrow(|pool| pool.get(&size).cloned())
    else {
        return ui.response();
    };

    // If the user said "use this specific galley", then just use it:
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::empty());
    let galley_pos = rect.center_top();

    if ui.is_rect_visible(response.rect) {
        let response_color = ui.style().visuals.text_color();
        ui.painter().add(
            epaint::TextShape::new(galley_pos, galley, response_color), // .with_underline(underline),
        );
    }

    response
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimeLabel {
    text: LayoutJob,
    timestamp: TimestampSeconds,
}

impl Default for TimeLabel {
    #[inline]
    fn default() -> Self {
        Self {
            text: LayoutJob::simple_singleline(
                String::with_capacity(15),
                FontId::default(),
                Color32::DARK_GRAY,
            ),
            timestamp: TimestampSeconds::default(),
        }
    }
}

impl TimeLabel {
    #[inline]
    pub fn update(&mut self, timestamp: TimestampSeconds) {
        use std::fmt::Write;
        self.text.text.clear();
        let datetime = timestamp.system_time();
        match datetime.elapsed() {
            Ok(elapsed) => {
                let days = elapsed.as_secs() / 86400;
                if days > 0 {
                    _ = self.text.text.write_fmt(format_args!("{days} days ago"));
                } else {
                    let hours = elapsed.as_secs() / 3600;
                    if hours > 0 {
                        _ = self.text.text.write_fmt(format_args!("{hours} hours ago"));
                    } else {
                        let minutes = elapsed.as_secs() / 60;
                        if minutes > 0 {
                            _ = self.text.text.write_fmt(format_args!("{minutes} min ago"));
                        } else {
                            _ = self.text.text.write_str("Just now");
                        }
                    }
                }
            }
            Err(_) => {
                _ = self.text.text.write_str("Future");
            }
        }
    }
}

impl Widget for &TimeLabel {
    fn ui(self, ui: &mut Ui) -> Response {
        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add(
                egui::Label::new(self.text.clone())
                    .wrap_mode(egui::TextWrapMode::Truncate)
                    .selectable(false)
                    .sense(Sense::empty()),
            );
        })
        .response
    }
}
