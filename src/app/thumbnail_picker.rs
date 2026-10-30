//! The "Select thumbnail" dialog: shows the frames of a video's overview and
//! makes the chosen one its thumbnail (see [`super::thumbnail_choice`]).
use std::path::Path;
use std::time::Duration;

use egui::{Context, Vec2};

use super::App;
use super::assets::{OverviewFrame, OverviewFrames};
use super::commands::{ActionToPerform, ModalWindow};
use super::file_ops::escape_pressed;
use super::thumbnail_choice;
use crate::data::files::DirEntry;

pub struct ThumbnailPicker {
    entry: DirEntry,
    /// The choice when the dialog opened; `None` is the default frame.
    current: Option<Duration>,
    /// The frame clicked in the dialog.
    selected: Option<Duration>,
}

const COLUMNS: usize = 4;

/// `m:ss` (or `h:mm:ss`) of a position in the video.
fn clock(at: Duration) -> String {
    let seconds = at.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn same(a: Option<Duration>, b: Option<Duration>) -> bool {
    a.map(|at| at.as_millis()) == b.map(|at| at.as_millis())
}

impl App {
    pub(super) fn open_thumbnail_picker(&mut self, entry: DirEntry) {
        let path = entry.get_path();
        let current = thumbnail_choice::get(&path, entry.meta.source_revision, entry.meta.size);
        self.thumbnail_picker = Some(ThumbnailPicker {
            entry,
            current,
            selected: current,
        });
        self.display_modal = Some(ModalWindow::SelectThumbnail);
    }

    /// Makes `at` the thumbnail frame of `path`, or the default with `None`.
    pub(super) fn set_thumbnail_choice(&mut self, path: &Path, at: Option<Duration>) {
        let Ok(metadata) = std::fs::metadata(path) else {
            return;
        };
        let meta = crate::data::files::DirEntryMetaData::from(metadata);
        self.set_thumbnail_choice_for(path, meta.source_revision, meta.size, at);
    }

    fn set_thumbnail_choice_for(
        &mut self,
        path: &Path,
        revision: u128,
        size: u64,
        at: Option<Duration>,
    ) {
        match at {
            Some(at) => thumbnail_choice::set(path, revision, size, at),
            None => {
                thumbnail_choice::clear(path);
            }
        }
        self.assets
            .thumbnail_choice_changed(path, revision, size, at);
    }

    fn close_thumbnail_picker(&mut self) {
        self.thumbnail_picker = None;
        self.assets.close_overview_frames();
        ActionToPerform::CloseActiveModalWindow.schedule();
    }

    /// Draws `ModalWindow::SelectThumbnail`.
    pub(super) fn select_thumbnail_ui(&mut self, ctx: &Context) {
        let Some(picker) = self.thumbnail_picker.as_mut() else {
            self.display_modal = None;
            return;
        };
        let frames = self
            .assets
            .request_overview_frames(ctx, &picker.entry.entry_ref());
        let mut apply = None;
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new(ModalWindow::SelectThumbnail))
            .frame(super::ui::dialog_frame(ctx))
            .show(ctx, |ui| {
                let width = (ctx.content_rect().width() - 64.0).clamp(320.0, 760.0);
                ui.set_width(width);
                ui.heading(format!("Choose thumbnail for {}", picker.entry.file_name));
                ui.add_space(4.0);
                let (shown, loading): (&[OverviewFrame], bool) = match &frames {
                    OverviewFrames::Ready(frames) => (frames, false),
                    OverviewFrames::Pending(frames) => (frames, true),
                    OverviewFrames::Unavailable(reason) => {
                        ui.label(format!(
                            "The video's frames could not be extracted: {reason}"
                        ));
                        (&[], false)
                    }
                };
                let spacing = ui.spacing().item_spacing.x;
                let cell = spacing.mul_add(-((COLUMNS - 1) as f32), width) / COLUMNS as f32;
                let image_size = Vec2::new(cell, cell * 9.0 / 16.0);
                // Without a choice, the first frame is the thumbnail.
                let highlighted = picker
                    .selected
                    .or_else(|| shown.first().map(|frame| frame.timestamp));
                egui::Grid::new("thumbnail_picker_frames")
                    .num_columns(COLUMNS)
                    .spacing(Vec2::splat(spacing))
                    .show(ui, |ui| {
                        for (index, frame) in shown.iter().enumerate() {
                            ui.vertical_centered(|ui| {
                                let image =
                                    egui::Image::new(&frame.texture).fit_to_exact_size(image_size);
                                let selected = same(highlighted, Some(frame.timestamp));
                                let response = ui
                                    .add(egui::Button::image(image).selected(selected))
                                    .on_hover_text(clock(frame.timestamp));
                                if response.clicked() {
                                    picker.selected = Some(frame.timestamp);
                                }
                                if response.double_clicked() {
                                    apply = Some(Some(frame.timestamp));
                                }
                                ui.weak(clock(frame.timestamp));
                            });
                            if (index + 1) % COLUMNS == 0 {
                                ui.end_row();
                            }
                        }
                    });
                if loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Extracting frames…");
                    });
                }
                ui.add_space(4.0);
                let changed = picker.selected.is_some() && !same(picker.selected, picker.current);
                match super::ui::dialog_buttons(
                    ui,
                    false,
                    &[
                        super::ui::DialogButton::primary("Use frame").enabled(changed),
                        super::ui::DialogButton::plain("Use default")
                            .enabled(picker.current.is_some()),
                        super::ui::DialogButton::plain("Cancel"),
                    ],
                ) {
                    Some(0) => apply = Some(picker.selected),
                    Some(1) => apply = Some(None),
                    Some(_) => close = true,
                    None => {}
                }
            });
        if let Some(at) = apply {
            let path = picker.entry.get_path();
            let (revision, size) = (picker.entry.meta.source_revision, picker.entry.meta.size);
            self.set_thumbnail_choice_for(&path, revision, size, at);
            self.close_thumbnail_picker();
        } else if close || modal.should_close() || escape_pressed(ctx) {
            self.close_thumbnail_picker();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_shows_minutes_and_hours() {
        assert_eq!(clock(Duration::from_millis(4_900)), "0:04");
        assert_eq!(clock(Duration::from_secs(125)), "2:05");
        assert_eq!(clock(Duration::from_secs(3_725)), "1:02:05");
    }
}
