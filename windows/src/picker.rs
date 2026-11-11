//! Choosing a video's thumbnail: the frame picked so far shown large, and
//! below it the frames to pick from, in the video's order. The frames fill
//! the dialog's width a few to a line, so there is one way to scroll: down.
use dirfleet_core::app::{Event, PickerFrameView, PickerGapView, ThumbnailPickerView};
use windows_reactor::{
    Border, Brush, Button, ButtonStyle, Color, Grid, GridLength, HorizontalAlignment, Image,
    KeyedView, Orientation, ProgressRing, ScrollBarVisibility, ScrollViewer, StackPanel, Stretch,
    TextBlock, TextWrapping, Thickness, TooltipExt as _, VerticalAlignment, View, keyed,
};

use crate::core_link::CoreLink;
use crate::files::glyph;
use crate::theme;

/// How many frames share a line.
const PER_LINE: usize = 3;
/// What a dialog gives its content across. The content takes all of it from
/// the start, or the dialog would grow as the frames' pictures come in.
const WIDTH: f64 = 500.0;
/// The picture of the frame picked so far.
const PREVIEW_HEIGHT: f64 = 180.0;
/// The frames' part of the dialog: two lines and the top of a third, which
/// says there is more below.
const FRAMES_HEIGHT: f64 = 220.0;
/// A frame's picture in the list is as wide as its place and as high as that
/// makes it, within these: the least is what a wide video's comes to, so a
/// line keeps its height while its pictures are made; the most keeps an
/// upright video's from being a line on its own.
const FRAME_HEIGHT: (f64, f64) = (58.0, 96.0);
/// The scroll bar lies over the frames' right edge when the pointer is near.
const SCROLL_BAR: f64 = 12.0;

/// The spinner that stands for a picture being made.
fn waiting() -> View {
    ProgressRing::new()
        .is_indeterminate(true)
        .is_active(true)
        .width(20.0)
        .height(20.0)
        .horizontal_alignment(HorizontalAlignment::Center)
        .vertical_alignment(VerticalAlignment::Center)
        .into()
}

/// The surface a picture lies on, seen around one that does not fill it.
fn backing() -> Border {
    Border::new()
        .background(Color::argb(0x30, 0x00, 0x00, 0x00))
        .corner_radius(4.0)
}

/// A picture from a file in a box of its own size, or a spinner while the
/// file is being made.
fn picture(file: &str, width: f64, height: f64) -> View {
    let inside: View = if file.is_empty() {
        waiting()
    } else {
        Image::new().source_file(file).map_or_else(
            |_| Border::new().into(),
            |image| image.stretch(Stretch::Uniform).into(),
        )
    };
    backing().width(width).height(height).content(inside).into()
}

/// A frame's picture in the list: as wide as the place it is given.
fn small_picture(file: &str) -> View {
    let inside: View = if file.is_empty() {
        waiting()
    } else {
        Image::new().source_file(file).map_or_else(
            |_| Border::new().into(),
            |image| {
                image
                    .stretch(Stretch::Uniform)
                    .max_height(FRAME_HEIGHT.1)
                    .into()
            },
        )
    };
    backing().min_height(FRAME_HEIGHT.0).content(inside).into()
}

/// A time and what the frame at it is ("Current", "Default").
fn caption(label: &str, badge: &str) -> String {
    if badge.is_empty() {
        label.to_owned()
    } else {
        format!("{label}  ·  {badge}")
    }
}

/// A button that asks for more frames in `gap`.
fn more_frames(core: &CoreLink, gap: &PickerGapView) -> View {
    let core = core.clone();
    let (from, to) = (gap.from, gap.to);
    Button::new()
        .style(ButtonStyle::Subtle)
        .automation_name(gap.tooltip.clone())
        .content(glyph("\u{E710}", 11.0))
        .on_click(move || core.send(Event::FillPickerGap { from, to }))
        .tooltip(&gap.tooltip)
}

/// One frame to pick: its picture and time, marked when it is the one picked.
fn frame(core: &CoreLink, frame: &PickerFrameView) -> View {
    let core = core.clone();
    let at_ms = frame.at_ms;
    let edge: Brush = if frame.selected {
        theme::focused_stroke()
    } else {
        Color::transparent().into()
    };
    Border::new()
        .border_brush(edge)
        .border_thickness(Thickness::uniform(2.0))
        .corner_radius(6.0)
        .content(
            Button::new()
                .style(ButtonStyle::Subtle)
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .horizontal_content_alignment(HorizontalAlignment::Stretch)
                .automation_name(format!("Frame at {}", frame.label))
                .content(
                    StackPanel::new()
                        .orientation(Orientation::Vertical)
                        .spacing(4.0)
                        .children((
                            small_picture(&frame.image),
                            TextBlock::new()
                                .text(caption(&frame.label, &frame.badge))
                                .font_size(11.0)
                                .opacity(0.8)
                                .horizontal_alignment(HorizontalAlignment::Center),
                        )),
                )
                .on_click(move || core.send(Event::SelectPickerFrame { at_ms })),
        )
        .into()
}

/// The frames in lines of `PER_LINE`, in one grid, so that every line's
/// frames are as wide as the others'. After a frame comes the button for more
/// between it and the next, when there is room for more.
fn frames(core: &CoreLink, picker: &ThumbnailPickerView) -> Grid {
    let mut cells: Vec<KeyedView> = Vec::new();
    for (line, row) in picker.frames.chunks(PER_LINE).zip(0i32..) {
        for (each, place) in line.iter().zip(0i32..) {
            // Told apart by the frame's time, so a frame added between two
            // moves the ones after it and none is made again.
            let key = each.at_ms.wrapping_mul(2);
            cells.push(keyed(
                key,
                Border::new()
                    .grid_row(row)
                    .grid_column(place * 2)
                    .content(frame(core, each)),
            ));
            if let Some(gap) = each.gap_after.as_ref().filter(|_| !picker.at_limit) {
                cells.push(keyed(
                    key.wrapping_add(1),
                    Border::new()
                        .grid_row(row)
                        .grid_column(place * 2 + 1)
                        .vertical_alignment(VerticalAlignment::Center)
                        .content(more_frames(core, gap)),
                ));
            }
        }
    }
    Grid::new()
        .columns((0..PER_LINE).flat_map(|_| [GridLength::STAR, GridLength::Auto]))
        .rows(picker.frames.chunks(PER_LINE).map(|_| GridLength::Auto))
        .row_spacing(4.0)
        .margin(Thickness::new(0.0, 0.0, SCROLL_BAR, 0.0))
        .keyed_children(cells)
}

/// The dialog's content for `picker`.
pub fn picker(core: &CoreLink, picker: &ThumbnailPickerView) -> View {
    if !picker.error.is_empty() {
        return TextBlock::new()
            .text(picker.error.clone())
            .text_wrapping(TextWrapping::Wrap)
            .into();
    }
    let step = |icon: &str, name: &str, enabled: bool, forward: bool| {
        let core = core.clone();
        Button::new()
            .style(ButtonStyle::Subtle)
            .is_enabled(enabled)
            .automation_name(name)
            .content(glyph(icon, 14.0))
            .on_click(move || core.send(Event::StepPickerFrame { forward, steps: 1 }))
            .tooltip(name)
    };
    // Beside the frame shown: the way to its neighbours, and to more frames
    // between it and them.
    let beside = |step: View, gap: Option<&PickerGapView>| {
        let more: View = gap
            .filter(|_| !picker.at_limit)
            .map_or_else(|| Border::new().into(), |gap| more_frames(core, gap));
        StackPanel::new()
            .orientation(Orientation::Vertical)
            .spacing(4.0)
            .vertical_alignment(VerticalAlignment::Center)
            .children((step, Border::new().content(more)))
    };
    let shown = Grid::new()
        .columns([GridLength::Auto, GridLength::STAR, GridLength::Auto])
        .column_spacing(8.0)
        .children((
            Border::new().content(beside(
                step("\u{E76B}", "Previous frame", picker.previous, false),
                picker.more_before.as_ref(),
            )),
            Border::new()
                .grid_column(1)
                .horizontal_alignment(HorizontalAlignment::Center)
                .content(picture(
                    &picker.preview_image,
                    PREVIEW_HEIGHT * 16.0 / 9.0,
                    PREVIEW_HEIGHT,
                )),
            Border::new().grid_column(2).content(beside(
                step("\u{E76C}", "Next frame", picker.next, true),
                picker.more_after.as_ref(),
            )),
        ));
    let mut notes: Vec<String> = vec![caption(&picker.preview_label, &picker.preview_badge)];
    if !picker.duration.is_empty() {
        notes.push(format!("of {}", picker.duration));
    }
    notes.extend(
        [&picker.status, &picker.failed]
            .into_iter()
            .filter(|note| !note.is_empty())
            .cloned(),
    );
    StackPanel::new()
        .orientation(Orientation::Vertical)
        .spacing(10.0)
        .width(WIDTH)
        .children((
            shown,
            TextBlock::new()
                .text(notes.join("   "))
                .font_size(12.0)
                .opacity(0.8)
                .horizontal_alignment(HorizontalAlignment::Center),
            // Only down: the frames are as wide as the dialog lets them be.
            ScrollViewer::new()
                .height(FRAMES_HEIGHT)
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
                .content(frames(core, picker)),
        ))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frames_caption_names_what_it_is() {
        assert_eq!(caption("0:04", ""), "0:04");
        assert_eq!(caption("0:04", "Current"), "0:04  ·  Current");
    }
}
