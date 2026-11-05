//! The "Select thumbnail" dialog: a large preview of the selected frame next
//! to a contact sheet of the video's frames in time order, with a `+` between
//! any two of them to look closer (see [`super::thumbnail_timeline`]). The
//! chosen frame becomes the video's thumbnail (see [`super::thumbnail_choice`]).
use std::path::Path;
use std::time::Duration;

use egui::{Color32, Context, Key, Modifiers, Rect, RichText, Sense, Stroke, StrokeKind, Ui, Vec2};

use super::App;
use super::assets::PickerFrame;
use super::commands::{ActionToPerform, ModalWindow};
use super::file_ops::escape_pressed;
use super::thumbnail_choice;
use super::thumbnail_timeline::{FrameTimeline, Gap, Slot};
use super::ui::{DialogButton, Palette, dialog_buttons, dialog_frame};
use crate::data::files::DirEntry;

pub struct ThumbnailPicker {
    entry: DirEntry,
    /// The choice when the dialog opened, in milliseconds; `None` is the
    /// default frame.
    current: Option<u64>,
    /// The frame clicked in the dialog, in milliseconds.
    selected: Option<u64>,
    /// The selection moved by keyboard: the contact sheet must follow it.
    reveal_selected: bool,
    /// How many frames the contact sheet last fit in a row.
    columns: usize,
}

/// Narrowest a frame on the contact sheet gets.
const MIN_CELL_WIDTH: f32 = 128.0;
/// Room for the `+` between two frames.
const GUTTER: f32 = 22.0;
/// Below this the large preview goes above the contact sheet.
const TWO_PANES_FROM: f32 = 640.0;
const CORNER: f32 = 4.0;

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

/// [`clock`] with tenths of a second, for frames too close to tell apart
/// without them.
fn fine_clock(at: Duration) -> String {
    format!("{}.{}", clock(at), at.subsec_millis() / 100)
}

/// How to label `ms` on a sheet where its neighbours are at `previous` and
/// `next`: finer once they are less than a second away.
fn cell_label(ms: u64, previous: Option<u64>, next: Option<u64>) -> String {
    let close = |other: Option<u64>| other.is_some_and(|other| other.abs_diff(ms) < 1_000);
    let at = Duration::from_millis(ms);
    if close(previous) || close(next) {
        fine_clock(at)
    } else {
        clock(at)
    }
}

fn gap_tooltip(gap: Gap) -> String {
    let label = |ms| {
        let at = Duration::from_millis(ms);
        if gap.to - gap.from < 5_000 {
            fine_clock(at)
        } else {
            clock(at)
        }
    };
    format!(
        "Add frames between {} and {}",
        label(gap.from),
        label(gap.to)
    )
}

/// What to make the thumbnail before closing the dialog.
#[derive(Clone, Copy)]
enum Apply {
    Frame(u64),
    Default,
}

/// What the user did in the dialog this frame.
#[derive(Default)]
struct Outcome {
    apply: Option<Apply>,
    fill: Option<Gap>,
    close: bool,
}

/// `size` scaled to fit in `area`, centered.
fn letterbox(area: Rect, size: Vec2) -> Rect {
    let scale = (area.width() / size.x).min(area.height() / size.y);
    Rect::from_center_size(area.center(), size * scale)
}

fn paint_frame(ui: &Ui, area: Rect, frame: &PickerFrame) {
    egui::Image::new(&frame.texture)
        .corner_radius(CORNER)
        .paint_at(ui, letterbox(area, frame.texture.size_vec2()));
}

/// A spinner in the middle of `area` that leaves the layout alone.
fn paint_spinner(ui: &Ui, area: Rect, size: f32) {
    ui.ctx().request_repaint();
    egui::Spinner::new()
        .size(size)
        .paint_at(ui, Rect::from_center_size(area.center(), Vec2::splat(size)));
}

/// A small label over a corner of a frame.
fn paint_badge(ui: &Ui, area: Rect, text: &str) {
    // Light on dark, whatever the frame underneath looks like.
    let (fill, color) = (Color32::from_black_alpha(190), Color32::WHITE);
    let font = egui::TextStyle::Small.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    let rect = Rect::from_min_size(
        area.min + Vec2::splat(4.0),
        galley.size() + Vec2::new(8.0, 2.0),
    );
    ui.painter().rect_filled(rect, CORNER, fill);
    ui.painter()
        .galley(rect.min + Vec2::new(4.0, 1.0), galley, color);
}

impl ThumbnailPicker {
    /// The frame shown as selected: without a choice, the default one.
    fn highlighted(&self, timeline: &FrameTimeline<PickerFrame>) -> Option<u64> {
        self.selected
            .or(self.current)
            .or_else(|| timeline.default_ms())
    }

    /// `Current`, `Default` or nothing for the frame at `ms`.
    fn badge(&self, timeline: &FrameTimeline<PickerFrame>, ms: u64) -> Option<&'static str> {
        if self.current == Some(ms) {
            Some("Current")
        } else if timeline.default_ms() == Some(ms) {
            Some("Default")
        } else {
            None
        }
    }

    /// Whether confirming would change anything.
    fn can_apply(&self, timeline: &FrameTimeline<PickerFrame>) -> bool {
        self.selected
            .is_some_and(|ms| self.current != Some(ms) && timeline.frame(ms).is_some())
    }

    const fn select_by_keyboard(&mut self, target: Option<u64>) {
        if let Some(target) = target {
            self.selected = Some(target);
            self.reveal_selected = true;
        }
    }

    fn handle_keys(
        &mut self,
        ctx: &Context,
        timeline: &FrameTimeline<PickerFrame>,
        outcome: &mut Outcome,
    ) {
        let Some(from) = self.highlighted(timeline) else {
            return;
        };
        let pressed = |key| ctx.input_mut(|input| input.consume_key(Modifiers::NONE, key));
        // Moves over finished frames only, `steps` of them.
        let walk = |forward: bool, steps: usize| {
            let mut position = from;
            let mut reached = None;
            for _ in 0..steps {
                let Some(next) = timeline.ready_neighbor(position, forward) else {
                    break;
                };
                position = next;
                reached = Some(next);
            }
            reached
        };
        if pressed(Key::ArrowLeft) {
            self.select_by_keyboard(walk(false, 1));
        }
        if pressed(Key::ArrowRight) {
            self.select_by_keyboard(walk(true, 1));
        }
        if pressed(Key::ArrowUp) {
            self.select_by_keyboard(walk(false, self.columns));
        }
        if pressed(Key::ArrowDown) {
            self.select_by_keyboard(walk(true, self.columns));
        }
        // Enter on a focused button belongs to that button.
        let button_focused = ctx.memory(|memory| memory.focused().is_some());
        if !button_focused && self.can_apply(timeline) && pressed(Key::Enter) {
            outcome.apply = self.selected.map(Apply::Frame);
        }
    }

    fn large_preview(
        &mut self,
        ui: &mut Ui,
        timeline: &FrameTimeline<PickerFrame>,
        width: f32,
        outcome: &mut Outcome,
    ) {
        let palette = Palette::of(ui);
        let highlighted = self.highlighted(timeline);
        let (area, _) =
            ui.allocate_exact_size(Vec2::new(width, width * 9.0 / 16.0), Sense::hover());
        ui.painter().rect_filled(area, CORNER, palette.card_bg);
        ui.painter().rect_stroke(
            area,
            CORNER,
            Stroke::new(1.0, palette.border),
            StrokeKind::Inside,
        );
        if let Some(frame) = highlighted.and_then(|ms| timeline.frame(ms)) {
            paint_frame(ui, area, frame);
        } else if highlighted.is_some() {
            paint_spinner(ui, area, 24.0);
        }
        ui.add_space(6.0);
        let Some(ms) = highlighted else {
            return;
        };
        ui.horizontal(|ui| {
            let at = Duration::from_millis(ms);
            let label = if ms % 1000 == 0 {
                clock(at)
            } else {
                fine_clock(at)
            };
            ui.label(RichText::new(label).heading());
            if let Some(badge) = self.badge(timeline, ms) {
                ui.weak(badge);
            }
        });
        ui.horizontal(|ui| {
            let previous = timeline.ready_neighbor(ms, false);
            let next = timeline.ready_neighbor(ms, true);
            if ui
                .add_enabled(previous.is_some(), egui::Button::new("‹ Previous"))
                .clicked()
            {
                self.select_by_keyboard(previous);
            }
            if ui
                .add_enabled(next.is_some(), egui::Button::new("Next ›"))
                .clicked()
            {
                self.select_by_keyboard(next);
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.weak("More frames:");
            for (label, gap) in [
                ("‹ Before", timeline.gap_before(ms)),
                ("After ›", timeline.gap_after(ms)),
            ] {
                let response = ui.add_enabled(gap.is_some(), egui::Button::new(label));
                let response = match gap {
                    Some(gap) => response.on_hover_text(gap_tooltip(gap)),
                    None => response,
                };
                if response.clicked() {
                    outcome.fill = gap;
                }
            }
        });
    }

    fn contact_sheet(
        &mut self,
        ui: &mut Ui,
        timeline: &FrameTimeline<PickerFrame>,
        height: f32,
        outcome: &mut Outcome,
    ) {
        egui::ScrollArea::vertical()
            .id_salt("thumbnail_picker_sheet")
            .max_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let available = ui.available_width() - 8.0;
                let columns = (((available + GUTTER) / (MIN_CELL_WIDTH + GUTTER)) as usize).max(1);
                self.columns = columns;
                let cell_width = available / columns as f32 - GUTTER;
                let image_size = Vec2::new(cell_width, cell_width * 9.0 / 16.0);
                let times: Vec<u64> = timeline.iter().map(|(ms, _)| ms).collect();
                let slots: Vec<(u64, &Slot<PickerFrame>)> = timeline.iter().collect();
                let highlighted = self.highlighted(timeline);
                ui.spacing_mut().item_spacing = Vec2::new(0.0, 8.0);
                for (row, cells) in slots.chunks(columns).enumerate() {
                    ui.horizontal_top(|ui| {
                        for (column, (ms, slot)) in cells.iter().enumerate() {
                            let index = row * columns + column;
                            let label = cell_label(
                                *ms,
                                index.checked_sub(1).map(|index| times[index]),
                                times.get(index + 1).copied(),
                            );
                            self.cell(
                                ui,
                                timeline,
                                (*ms, slot),
                                image_size,
                                &label,
                                highlighted == Some(*ms),
                                outcome,
                            );
                            gap_button(ui, timeline.gap_after(*ms), image_size.y, outcome);
                        }
                    });
                }
            });
    }

    #[allow(clippy::too_many_arguments)]
    fn cell(
        &mut self,
        ui: &mut Ui,
        timeline: &FrameTimeline<PickerFrame>,
        (ms, slot): (u64, &Slot<PickerFrame>),
        image_size: Vec2,
        label: &str,
        highlighted: bool,
        outcome: &mut Outcome,
    ) {
        let palette = Palette::of(ui);
        let line = ui.text_style_height(&egui::TextStyle::Small) + 2.0;
        let sense = if matches!(slot, Slot::Ready(_)) {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, response) = ui.allocate_exact_size(image_size + Vec2::new(0.0, line), sense);
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Frame at {label}"))
        });
        let image = Rect::from_min_size(rect.min, image_size);
        ui.painter().rect_filled(image, CORNER, palette.card_bg);
        match slot {
            Slot::Ready(frame) => paint_frame(ui, image, frame),
            Slot::Loading => paint_spinner(ui, image, 16.0),
            Slot::Queued => {}
        }
        if let Some(badge) = self.badge(timeline, ms) {
            paint_badge(ui, image, badge);
        }
        let accent = ui.visuals().selection.bg_fill;
        let outline = if highlighted {
            Some(Stroke::new(2.0, accent))
        } else if response.hovered() {
            Some(Stroke::new(1.0, palette.text_weak))
        } else {
            None
        };
        if let Some(outline) = outline {
            ui.painter()
                .rect_stroke(image, CORNER, outline, StrokeKind::Outside);
        }
        ui.painter().text(
            egui::pos2(image.center().x, image.bottom() + 2.0),
            egui::Align2::CENTER_TOP,
            label,
            egui::TextStyle::Small.resolve(ui.style()),
            palette.text_weak,
        );
        if response.clicked() {
            self.selected = Some(ms);
        }
        if response.double_clicked() {
            outcome.apply = Some(Apply::Frame(ms));
        }
        if highlighted && self.reveal_selected {
            ui.scroll_to_rect(rect, None);
            self.reveal_selected = false;
        }
    }
}

/// The `+` after a frame, always taking up its room so that the frames stay
/// put. It is also there at the end of a row, where no next frame is in sight.
fn gap_button(ui: &mut Ui, gap: Option<Gap>, height: f32, outcome: &mut Outcome) {
    let size = Vec2::new(GUTTER, height);
    let Some(gap) = gap else {
        ui.allocate_exact_size(size, Sense::hover());
        return;
    };
    let tooltip = gap_tooltip(gap);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tooltip.clone()));
    let palette = Palette::of(ui);
    let color: Color32 = if response.hovered() {
        ui.visuals().strong_text_color()
    } else {
        palette.text_weak.gamma_multiply(0.5)
    };
    let center = rect.center();
    if response.hovered() {
        ui.painter()
            .circle_filled(center, 9.0, ui.visuals().widgets.hovered.bg_fill);
    }
    let stroke = Stroke::new(1.5, color);
    let arm = 4.0;
    ui.painter()
        .hline((center.x - arm)..=(center.x + arm), center.y, stroke);
    ui.painter()
        .vline(center.x, (center.y - arm)..=(center.y + arm), stroke);
    if response.on_hover_text(tooltip).clicked() {
        outcome.fill = Some(gap);
    }
}

/// The line above the buttons: how far along it is and what went wrong.
fn status_line(ui: &mut Ui, timeline: &FrameTimeline<PickerFrame>, waiting: bool) {
    let (done, total) = timeline.progress();
    ui.horizontal(|ui| {
        if waiting {
            ui.spinner();
            ui.weak("Reading the video…");
        } else if done < total {
            ui.add(
                egui::ProgressBar::new(done as f32 / total as f32)
                    .desired_width(120.0)
                    .desired_height(6.0),
            );
            ui.weak(format!("Extracting {done} of {total}, earliest first"));
        }
        let failed = timeline.failed();
        if failed > 0 {
            let frames = if failed == 1 { "frame" } else { "frames" };
            ui.weak(format!("{failed} {frames} could not be extracted"));
        }
        if timeline.at_limit() {
            ui.weak("Frame limit reached: close the dialog to start over");
        }
    });
}

impl App {
    pub(super) fn open_thumbnail_picker(&mut self, entry: DirEntry) {
        let path = entry.get_path();
        let current = thumbnail_choice::get(&path, entry.meta.source_revision, entry.meta.size);
        self.assets.picker_open(&entry.entry_ref(), current);
        let current = current.map(|at| at.as_millis() as u64);
        self.thumbnail_picker = Some(ThumbnailPicker {
            entry,
            current,
            selected: None,
            reveal_selected: current.is_some(),
            columns: 1,
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
        self.assets.picker_close();
        ActionToPerform::CloseActiveModalWindow.schedule();
    }

    /// Draws `ModalWindow::SelectThumbnail`.
    pub(super) fn select_thumbnail_ui(&mut self, ctx: &Context) {
        let Some(picker) = self.thumbnail_picker.as_mut() else {
            self.display_modal = None;
            return;
        };
        self.assets.picker_pump();
        let Some(timeline) = self.assets.picker_timeline() else {
            self.close_thumbnail_picker();
            return;
        };
        let error = self.assets.picker_error();
        let waiting = self.assets.picker_waiting_for_timing();
        let mut outcome = Outcome::default();
        picker.handle_keys(ctx, timeline, &mut outcome);
        let modal = egui::Modal::new(egui::Id::unique(ModalWindow::SelectThumbnail))
            .frame(dialog_frame(ctx))
            .show(ctx, |ui| {
                let content = ctx.content_rect();
                let width = (content.width() - 64.0).clamp(320.0, 960.0);
                let height = (content.height() - 96.0).clamp(280.0, 640.0);
                ui.set_width(width);
                ui.horizontal(|ui| {
                    ui.heading("Choose thumbnail");
                    ui.add(
                        egui::Label::new(RichText::new(&picker.entry.file_name).weak()).truncate(),
                    );
                    if let Some(duration) = timeline.duration() {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.weak(clock(duration));
                        });
                    }
                });
                ui.add_space(8.0);
                if let Some(error) = error {
                    ui.label(format!(
                        "The video's frames could not be extracted: {error}"
                    ));
                } else {
                    // Heading, status line and buttons take what's left.
                    let body = height - 96.0;
                    if width >= TWO_PANES_FROM {
                        let preview_width = width * 0.48;
                        ui.horizontal_top(|ui| {
                            ui.allocate_ui_with_layout(
                                Vec2::new(preview_width, body),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    ui.set_min_height(body);
                                    picker.large_preview(ui, timeline, preview_width, &mut outcome);
                                },
                            );
                            ui.add_space(12.0);
                            ui.vertical(|ui| {
                                picker.contact_sheet(ui, timeline, body, &mut outcome);
                            });
                        });
                    } else {
                        let preview_width = width.min(body * 0.75);
                        picker.large_preview(ui, timeline, preview_width, &mut outcome);
                        ui.add_space(8.0);
                        let sheet = (body - preview_width * 9.0 / 16.0 - 96.0).max(120.0);
                        picker.contact_sheet(ui, timeline, sheet, &mut outcome);
                    }
                }
                ui.add_space(8.0);
                status_line(ui, timeline, waiting);
                ui.add_space(4.0);
                match dialog_buttons(
                    ui,
                    false,
                    &[
                        DialogButton::primary("Use frame").enabled(picker.can_apply(timeline)),
                        DialogButton::plain("Reset to default").enabled(picker.current.is_some()),
                        DialogButton::plain("Cancel"),
                    ],
                ) {
                    Some(0) => outcome.apply = picker.selected.map(Apply::Frame),
                    Some(1) => outcome.apply = Some(Apply::Default),
                    Some(_) => outcome.close = true,
                    None => {}
                }
            });
        if let Some(apply) = outcome.apply {
            let path = picker.entry.get_path();
            let (revision, size) = (picker.entry.meta.source_revision, picker.entry.meta.size);
            let at = match apply {
                Apply::Frame(ms) => Some(Duration::from_millis(ms)),
                Apply::Default => None,
            };
            self.set_thumbnail_choice_for(&path, revision, size, at);
            self.close_thumbnail_picker();
        } else if outcome.close || modal.should_close() || escape_pressed(ctx) {
            self.close_thumbnail_picker();
        } else if let Some(gap) = outcome.fill {
            self.assets.picker_fill_gap(gap);
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

    #[test]
    fn close_frames_are_labelled_with_tenths() {
        assert_eq!(fine_clock(Duration::from_millis(4_950)), "0:04.9");
        assert_eq!(cell_label(4_900, Some(4_500), Some(9_000)), "0:04.9");
        assert_eq!(cell_label(4_900, Some(1_000), Some(5_100)), "0:04.9");
        assert_eq!(cell_label(4_900, Some(1_000), Some(9_000)), "0:04");
        assert_eq!(cell_label(4_900, None, None), "0:04");
    }

    #[test]
    fn gap_tooltips_show_tenths_only_for_short_gaps() {
        assert_eq!(
            gap_tooltip(Gap {
                from: 15_000,
                to: 57_000
            }),
            "Add frames between 0:15 and 0:57"
        );
        assert_eq!(
            gap_tooltip(Gap {
                from: 1_500,
                to: 2_000
            }),
            "Add frames between 0:01.5 and 0:02.0"
        );
    }

    #[test]
    fn letterbox_keeps_the_aspect_ratio_and_centers() {
        let area = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(160.0, 90.0));
        let portrait = letterbox(area, Vec2::new(90.0, 160.0));
        assert!((portrait.height() - 90.0).abs() < 0.01);
        assert!((portrait.width() - 50.625).abs() < 0.01);
        assert!((portrait.center().x - 80.0).abs() < 0.01);
    }
}
