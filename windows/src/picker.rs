//! Choosing a video's thumbnail: the frame picked so far shown large, and
//! the frames to pick from along the video's length.
use dirfleet_core::app::{Event, PickerFrameView, PickerGapView, ThumbnailPickerView};
use windows_reactor::{
    Border, Brush, Button, ButtonStyle, Color, Grid, GridLength, HorizontalAlignment, Image,
    Orientation, ProgressRing, ScrollBarVisibility, ScrollViewer, StackPanel, Stretch, TextBlock,
    TextWrapping, Thickness, TooltipExt as _, VerticalAlignment, View, keyed,
};

use crate::core_link::CoreLink;
use crate::files::glyph;
use crate::theme;

/// How many frames share a line.
const PER_LINE: usize = 3;
/// A frame's picture in the list.
const FRAME: (f64, f64) = (128.0, 72.0);
/// The picture of the frame picked so far.
const PREVIEW_HEIGHT: f64 = 190.0;

/// A picture from a file, or a spinner while the file is being made.
fn picture(file: &str, width: f64, height: f64) -> View {
    let inside: View = if file.is_empty() {
        ProgressRing::new()
            .is_indeterminate(true)
            .is_active(true)
            .width(20.0)
            .height(20.0)
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .into()
    } else {
        Image::new().source_file(file).map_or_else(
            |_| Border::new().into(),
            |image| image.stretch(Stretch::Uniform).into(),
        )
    };
    Border::new()
        .width(width)
        .height(height)
        .background(Color::argb(0x30, 0x00, 0x00, 0x00))
        .corner_radius(4.0)
        .content(inside)
        .into()
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
                .automation_name(format!("Frame at {}", frame.label))
                .content(
                    StackPanel::new()
                        .orientation(Orientation::Vertical)
                        .spacing(4.0)
                        .children((
                            picture(&frame.image, FRAME.0, FRAME.1),
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
    let shown = Grid::new()
        .columns([GridLength::Auto, GridLength::STAR, GridLength::Auto])
        .column_spacing(8.0)
        .children((
            Border::new()
                .vertical_alignment(VerticalAlignment::Center)
                .content(step("\u{E76B}", "Previous frame", picker.previous, false)),
            Border::new()
                .grid_column(1)
                .horizontal_alignment(HorizontalAlignment::Center)
                .content(picture(
                    &picker.preview_image,
                    PREVIEW_HEIGHT * 16.0 / 9.0,
                    PREVIEW_HEIGHT,
                )),
            Border::new()
                .grid_column(2)
                .vertical_alignment(VerticalAlignment::Center)
                .content(step("\u{E76C}", "Next frame", picker.next, true)),
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
    // The frames in lines; between two, a button for more when there is room.
    let mut cells: Vec<View> = Vec::new();
    if let Some(gap) = &picker.more_before {
        cells.push(more_frames(core, gap));
    }
    for each in &picker.frames {
        cells.push(frame(core, each));
        if let Some(gap) = each.gap_after.as_ref().filter(|_| !picker.at_limit) {
            cells.push(more_frames(core, gap));
        }
    }
    if let Some(gap) = &picker.more_after {
        cells.push(more_frames(core, gap));
    }
    let mut lines: Vec<Vec<View>> = Vec::new();
    for cell in cells {
        match lines.last_mut() {
            // A frame and the button after it count as one place and a bit.
            Some(line) if line.len() < PER_LINE * 2 => line.push(cell),
            _ => lines.push(vec![cell]),
        }
    }
    let frames = lines.into_iter().zip(0u32..).map(|(line, position)| {
        keyed(
            position,
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(2.0)
                .children(line),
        )
    });
    StackPanel::new()
        .orientation(Orientation::Vertical)
        .spacing(10.0)
        .children((
            shown,
            TextBlock::new()
                .text(notes.join("   "))
                .font_size(12.0)
                .opacity(0.8)
                .horizontal_alignment(HorizontalAlignment::Center),
            ScrollViewer::new()
                .height(200.0)
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                .horizontal_scroll_bar_visibility(ScrollBarVisibility::Auto)
                .content(
                    StackPanel::new()
                        .orientation(Orientation::Vertical)
                        .spacing(4.0)
                        .keyed_children(frames),
                ),
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
