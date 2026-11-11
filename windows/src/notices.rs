//! What the app has to say while the user works: operations in progress, and
//! short messages that go away by themselves.
use dirfleet_core::app::{Event, OperationState, OperationView, ToastKind, ToastView};
use windows_reactor::{
    Border, Button, ButtonStyle, Grid, GridLength, HorizontalAlignment, InfoBar, InfoBarSeverity,
    Orientation, ProgressBar, StackPanel, TextBlock, TextTrimming, Thickness, TooltipExt as _,
    VerticalAlignment, View, keyed,
};

use crate::core_link::CoreLink;
use crate::files::glyph;

/// The height of one operation's line.
pub const OPERATION_HEIGHT: f64 = 30.0;

/// What the button beside an operation's Stop does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Turn {
    Pause,
    Resume,
    /// Start an operation that waits for another to end.
    StartNow,
}

impl Turn {
    /// The button an operation gets: none for one that can't be held (a
    /// rename is over before anyone could press it).
    fn of(operation: &OperationView) -> Option<Self> {
        match operation.state {
            OperationState::Paused => Some(Self::Resume),
            OperationState::Waiting => Some(Self::StartNow),
            OperationState::Running => operation.can_pause.then_some(Self::Pause),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Pause => "Pause",
            Self::Resume => "Resume",
            Self::StartNow => "Start now",
        }
    }

    const fn glyph(self) -> &'static str {
        match self {
            Self::Pause => "\u{E769}",
            Self::Resume | Self::StartNow => "\u{E768}",
        }
    }

    const fn event(self, id: u64) -> Event {
        match self {
            Self::Pause => Event::PauseOperation { id },
            Self::Resume => Event::ResumeOperation { id },
            Self::StartNow => Event::StartOperationNow { id },
        }
    }
}

/// What an operation's line says after its description: the item it is on
/// and how it stands ("report.pdf · 1.2 GB of 4.0 GB · 85.0 MB/s", "Waiting").
fn standing(operation: &OperationView) -> String {
    match (operation.current.is_empty(), operation.detail.is_empty()) {
        (_, true) => operation.current.clone(),
        (true, false) => operation.detail.clone(),
        (false, false) => format!("{} \u{b7} {}", operation.current, operation.detail),
    }
}

/// Copies, moves and deletions under way or waiting their turn: what each
/// does, how far it is, and ways to hold it, let it go and stop it.
pub fn operations(core: &CoreLink, operations: &[OperationView]) -> View {
    let lines = operations.iter().map(|operation| {
        let id = operation.id;
        let turn = Turn::of(operation).map(|turn| {
            let core = core.clone();
            Button::new()
                .style(ButtonStyle::Subtle)
                .automation_name(format!("{}: {}", turn.label(), operation.description))
                .content(glyph(turn.glyph(), 10.0))
                .on_click(move || core.send(turn.event(id)))
                .tooltip(turn.label())
        });
        let turn_slot = Border::new().grid_column(3);
        let turn_slot = match turn {
            Some(button) => turn_slot.content(button),
            None => turn_slot,
        };
        let core = core.clone();
        let line = Grid::new()
            .height(OPERATION_HEIGHT)
            .columns([
                GridLength::Auto,
                GridLength::STAR,
                GridLength::Pixel(160.0),
                GridLength::Auto,
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
                    .text(standing(operation))
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
                turn_slot,
                Border::new().grid_column(4).content(
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

#[cfg(test)]
mod tests {
    use super::*;

    fn operation(state: OperationState, can_pause: bool) -> OperationView {
        OperationView {
            id: 1,
            description: "Copying 1 item to Backup".into(),
            progress: 0,
            current: String::new(),
            state,
            detail: String::new(),
            can_pause,
        }
    }

    #[test]
    fn an_operation_s_button_follows_its_state() {
        let turn = |state, can_pause| Turn::of(&operation(state, can_pause));
        assert_eq!(turn(OperationState::Running, true), Some(Turn::Pause));
        assert_eq!(turn(OperationState::Running, false), None);
        assert_eq!(turn(OperationState::Paused, true), Some(Turn::Resume));
        assert_eq!(turn(OperationState::Waiting, true), Some(Turn::StartNow));
        assert_eq!(Turn::StartNow.event(7), Event::StartOperationNow { id: 7 });
    }

    #[test]
    fn an_operation_s_line_names_its_item_and_how_it_stands() {
        let mut copy = operation(OperationState::Running, true);
        assert_eq!(standing(&copy), "");
        copy.current = "report.pdf".into();
        assert_eq!(standing(&copy), "report.pdf");
        copy.detail = "1.2 GB of 4.0 GB".into();
        assert_eq!(standing(&copy), "report.pdf \u{b7} 1.2 GB of 4.0 GB");
        copy.current.clear();
        copy.detail = "Waiting".into();
        assert_eq!(standing(&copy), "Waiting");
    }
}
