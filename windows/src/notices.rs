//! What the app has to say while the user works: operations in progress, and
//! short messages that go away by themselves.
use dirfleet_core::app::{Event, OperationView, ToastKind, ToastView};
use windows_reactor::{
    Border, Button, ButtonStyle, Grid, GridLength, HorizontalAlignment, InfoBar, InfoBarSeverity,
    Orientation, ProgressBar, StackPanel, TextBlock, TextTrimming, Thickness, TooltipExt as _,
    VerticalAlignment, View, keyed,
};

use crate::core_link::CoreLink;
use crate::files::glyph;

/// The height of one operation's line.
pub const OPERATION_HEIGHT: f64 = 30.0;

/// Copies, moves and deletions under way: what each does, how far it is, and a way to stop it.
pub fn operations(core: &CoreLink, operations: &[OperationView]) -> View {
    let lines = operations.iter().map(|operation| {
        let core = core.clone();
        let id = operation.id;
        let line = Grid::new()
            .height(OPERATION_HEIGHT)
            .columns([
                GridLength::Auto,
                GridLength::STAR,
                GridLength::Pixel(160.0),
                GridLength::Auto,
            ])
            .column_spacing(12.0)
            .margin(Thickness::new(14.0, 0.0, 8.0, 0.0))
            .children((
                TextBlock::new()
                    .text(operation.description.clone())
                    .font_size(12.0)
                    .vertical_alignment(VerticalAlignment::Center),
                TextBlock::new()
                    .text(operation.current.clone())
                    .font_size(12.0)
                    .opacity(0.6)
                    .text_trimming(TextTrimming::CharacterEllipsis)
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(1),
                ProgressBar::new()
                    .minimum(0.0)
                    .maximum(1000.0)
                    .value(f64::from(operation.progress))
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(2),
                Border::new().grid_column(3).content(
                    Button::new()
                        .style(ButtonStyle::Subtle)
                        .automation_name(format!("Stop: {}", operation.description))
                        .content(glyph("\u{E711}", 10.0))
                        .on_click(move || core.send(Event::CancelOperation { id }))
                        .tooltip("Stop"),
                ),
            ));
        keyed(id, line)
    });
    StackPanel::new()
        .orientation(Orientation::Vertical)
        .keyed_children(lines)
        .into()
}

/// Short messages, stacked in the window's corner; closing one dismisses it.
pub fn toasts(core: &CoreLink, toasts: &[ToastView]) -> View {
    let bars = toasts.iter().map(|toast| {
        let core = core.clone();
        let id = toast.id;
        keyed(
            id,
            InfoBar::new()
                .is_open(true)
                .is_closable(true)
                .severity(match toast.kind {
                    ToastKind::Info => InfoBarSeverity::Informational,
                    ToastKind::Success => InfoBarSeverity::Success,
                    ToastKind::Warning => InfoBarSeverity::Warning,
                    ToastKind::Error => InfoBarSeverity::Error,
                })
                .message(toast.text.clone())
                .on_closed(move || core.send(Event::DismissToast { id })),
        )
    });
    StackPanel::new()
        .orientation(Orientation::Vertical)
        .spacing(6.0)
        .max_width(420.0)
        .margin(Thickness::new(0.0, 0.0, 16.0, 36.0))
        .horizontal_alignment(HorizontalAlignment::Right)
        .vertical_alignment(VerticalAlignment::Bottom)
        .keyed_children(bars)
        .into()
}
