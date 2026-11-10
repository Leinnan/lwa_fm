//! Choosing a video's thumbnail, as egui's "Select thumbnail" dialog: a large
//! preview of the selected frame beside a sheet of the video's frames in time
//! order, with more frames between any two on request
//! ([`crate::thumbnail_timeline`]). Frames are extracted one at a time.
//!
//! The chosen frame becomes the video's thumbnail for the version of the file
//! it was chosen for; the effects keep the choices and draw thumbnails from
//! them, and the core keeps a copy to tell which thumbnails are current.
use std::time::Duration;

use crux_core::Command;
use crux_core::capability::Operation;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::{DirFleet, Effect, Event, Model, tab_id};
use crate::listing::EntryType;
use crate::listing::kinds::{MediaKind, media_kind};
use crate::thumbnail_timeline::{FrameTimeline, Gap, Slot};

/// A frame chosen as a video's thumbnail, and the version of the file
/// (revision and size) it was chosen for.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[facet(opaque)]
pub struct ChosenFrame {
    pub at_ms: u64,
    pub revision: u128,
    pub size: u64,
}

/// The frames chosen as thumbnails, which the effects keep.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThumbnailChoiceOp {
    /// Every choice, at start.
    Load,
    /// Choose a frame for `path`, or the default one with `None`.
    Set {
        path: String,
        frame: Option<ChosenFrame>,
    },
    /// Items renamed or moved, `(from, to)`: their choices and those of
    /// everything below them follow.
    Rename(Vec<(String, String)>),
    /// Forget every choice.
    Clear,
}

/// Every choice by path, after the operation. Opaque to facet: it never
/// leaves the core.
#[derive(Clone, Debug, PartialEq, Eq, Facet)]
#[facet(opaque)]
pub struct ThumbnailChoices(pub Vec<(String, ChosenFrame)>);

impl Operation for ThumbnailChoiceOp {
    type Output = ThumbnailChoices;
}

/// How long the video is and where its default thumbnail comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerTiming {
    pub path: String,
    pub revision: u128,
    pub size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoLength {
    pub duration_ms: Option<u64>,
    /// Frames per thousand seconds, when known.
    pub frame_rate_milli: Option<u64>,
    /// Where the default thumbnail is.
    pub default_ms: u64,
}

/// The video's length, or why it couldn't be read. Opaque to facet: it never
/// leaves the core.
#[derive(Clone, Debug, PartialEq, Eq, Facet)]
#[facet(opaque)]
pub struct VideoTiming(pub Result<VideoLength, String>);

impl Operation for PickerTiming {
    type Output = VideoTiming;
}

/// One frame of the video at `at_ms`, as an image file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerFrame {
    pub path: String,
    pub revision: u128,
    pub size: u64,
    pub at_ms: u64,
}

impl Operation for PickerFrame {
    /// The image file, or `None` when the frame couldn't be extracted.
    type Output = Option<String>;
}

/// Room for more frames between two, or before the first.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PickerGapView {
    pub from: u64,
    pub to: u64,
    /// "Add frames between 0:15 and 0:57".
    pub tooltip: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PickerFrameView {
    pub at_ms: u64,
    /// "0:04", or "0:04.9" when a neighbour is less than a second away.
    pub label: String,
    /// The frame's image file, or "" while it is extracted.
    pub image: String,
    pub selected: bool,
    /// "Current", "Default" or "".
    pub badge: String,
    /// More frames can go between this frame and the next (or the end).
    pub gap_after: Option<PickerGapView>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ThumbnailPickerView {
    /// The tab whose window shows the dialog.
    pub tab: u32,
    pub name: String,
    /// The video's length, once known.
    pub duration: String,
    pub frames: Vec<PickerFrameView>,
    /// The selected frame (without a selection, the current or default one),
    /// shown large: its image ("" while extracted), time and badge.
    pub preview_image: String,
    pub preview_label: String,
    pub preview_badge: String,
    /// A finished frame comes before or after the one shown.
    pub previous: bool,
    pub next: bool,
    /// More frames before or after the one shown.
    pub more_before: Option<PickerGapView>,
    pub more_after: Option<PickerGapView>,
    /// "Reading the video…", "Extracting 3 of 16, earliest first" or "".
    pub status: String,
    /// "2 frames could not be extracted" or "".
    pub failed: String,
    pub at_limit: bool,
    /// Why the video's frames can't be offered, or "".
    pub error: String,
    /// The selected frame differs from the current one and is ready.
    pub can_apply: bool,
    /// The video has a chosen frame to go back from.
    pub can_reset: bool,
}

/// Whether a row's thumbnail can be chosen, and whether it was.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(C)]
pub enum ThumbnailChoice {
    /// Not a video.
    #[default]
    None,
    /// The video's default frame (a keyword in Swift, hence the name).
    Automatic,
    Chosen,
}

/// The open dialog.
pub(super) struct Picker {
    tab: u32,
    path: String,
    name: String,
    revision: u128,
    size: u64,
    /// The frame chosen before, if any.
    current: Option<u64>,
    /// The frame clicked or stepped to.
    selected: Option<u64>,
    timing_known: bool,
    error: Option<String>,
    /// A frame or the timing is being extracted.
    running: bool,
    /// Bumped per dialog; answers for an earlier one are dropped.
    session: u64,
    timeline: FrameTimeline<String>,
}

/// `m:ss` (or `h:mm:ss`) of a position in the video.
fn clock(ms: u64) -> String {
    let seconds = ms / 1000;
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// [`clock`] with tenths of a second, for frames too close to tell apart
/// without them.
fn fine_clock(ms: u64) -> String {
    format!("{}.{}", clock(ms), ms % 1000 / 100)
}

/// How to label `ms` on a sheet where its neighbours are at `previous` and
/// `next`: finer once they are less than a second away.
fn cell_label(ms: u64, previous: Option<u64>, next: Option<u64>) -> String {
    let close = |other: Option<u64>| other.is_some_and(|other| other.abs_diff(ms) < 1_000);
    if close(previous) || close(next) {
        fine_clock(ms)
    } else {
        clock(ms)
    }
}

fn gap_view(gap: Gap) -> PickerGapView {
    let label = |ms| {
        if gap.to - gap.from < 5_000 {
            fine_clock(ms)
        } else {
            clock(ms)
        }
    };
    PickerGapView {
        from: gap.from,
        to: gap.to,
        tooltip: format!(
            "Add frames between {} and {}",
            label(gap.from),
            label(gap.to)
        ),
    }
}

/// A file the picker can choose a frame of.
pub(super) fn is_video(path: &str, entry_type: EntryType) -> bool {
    entry_type == EntryType::File
        && media_kind(std::path::Path::new(path)) == Some(MediaKind::Video)
}

impl Picker {
    /// The frame shown as selected: without a choice, the default one.
    fn highlighted(&self) -> Option<u64> {
        self.selected
            .or(self.current)
            .or_else(|| self.timeline.default_ms())
    }

    fn badge(&self, ms: u64) -> &'static str {
        if self.current == Some(ms) {
            "Current"
        } else if self.timeline.default_ms() == Some(ms) {
            "Default"
        } else {
            ""
        }
    }

    /// Whether using the selection would change anything.
    fn can_apply(&self) -> bool {
        self.selected
            .is_some_and(|ms| self.current != Some(ms) && self.timeline.frame(ms).is_some())
    }

    /// Selects the finished frame `steps` before or after the shown one.
    fn step(&mut self, forward: bool, steps: u32) {
        let Some(mut position) = self.highlighted() else {
            return;
        };
        for _ in 0..steps.max(1) {
            let Some(next) = self.timeline.ready_neighbor(position, forward) else {
                break;
            };
            position = next;
            self.selected = Some(next);
        }
    }

    /// The next job, unless one is running.
    fn pump(&mut self) -> Command<Effect, Event> {
        if self.running || self.error.is_some() {
            return Command::done();
        }
        let session = self.session;
        let (path, revision, size) = (self.path.clone(), self.revision, self.size);
        if !self.timing_known {
            self.running = true;
            return Command::request_from_shell(PickerTiming {
                path,
                revision,
                size,
            })
            .then_send(move |timing| Event::PickerTimed { session, timing });
        }
        let Some(at_ms) = self.timeline.next_job() else {
            return Command::done();
        };
        self.running = true;
        Command::request_from_shell(PickerFrame {
            path,
            revision,
            size,
            at_ms,
        })
        .then_send(move |file| Event::PickerFrameMade {
            session,
            at_ms,
            file,
        })
    }

    pub(super) fn view(&self) -> ThumbnailPickerView {
        let highlighted = self.highlighted();
        let slots: Vec<(u64, &Slot<String>)> = self.timeline.iter().collect();
        let frames = slots
            .iter()
            .enumerate()
            .map(|(index, &(ms, slot))| PickerFrameView {
                at_ms: ms,
                label: cell_label(
                    ms,
                    index.checked_sub(1).map(|previous| slots[previous].0),
                    slots.get(index + 1).map(|next| next.0),
                ),
                image: match slot {
                    Slot::Ready(file) => file.clone(),
                    Slot::Queued | Slot::Loading => String::new(),
                },
                selected: highlighted == Some(ms),
                badge: self.badge(ms).to_owned(),
                gap_after: self.timeline.gap_after(ms).map(gap_view),
            })
            .collect();
        let (ready, total) = self.timeline.progress();
        let failed = self.timeline.failed();
        ThumbnailPickerView {
            tab: self.tab,
            name: self.name.clone(),
            duration: self
                .timeline
                .duration()
                .map(|length| clock(u64::try_from(length.as_millis()).unwrap_or(u64::MAX)))
                .unwrap_or_default(),
            frames,
            preview_image: highlighted
                .and_then(|ms| self.timeline.frame(ms))
                .cloned()
                .unwrap_or_default(),
            preview_label: highlighted
                .map(|ms| {
                    if ms % 1000 == 0 {
                        clock(ms)
                    } else {
                        fine_clock(ms)
                    }
                })
                .unwrap_or_default(),
            preview_badge: highlighted
                .map(|ms| self.badge(ms))
                .unwrap_or_default()
                .to_owned(),
            previous: highlighted
                .is_some_and(|ms| self.timeline.ready_neighbor(ms, false).is_some()),
            next: highlighted.is_some_and(|ms| self.timeline.ready_neighbor(ms, true).is_some()),
            more_before: highlighted
                .and_then(|ms| self.timeline.gap_before(ms))
                .map(gap_view),
            more_after: highlighted
                .and_then(|ms| self.timeline.gap_after(ms))
                .map(gap_view),
            status: if self.error.is_some() {
                String::new()
            } else if !self.timing_known {
                "Reading the video…".to_owned()
            } else if ready < total {
                format!("Extracting {ready} of {total}, earliest first")
            } else {
                String::new()
            },
            failed: match failed {
                0 => String::new(),
                1 => "1 frame could not be extracted".to_owned(),
                _ => format!("{failed} frames could not be extracted"),
            },
            at_limit: self.timeline.at_limit(),
            error: self
                .error
                .as_ref()
                .map(|error| format!("The video's frames could not be extracted: {error}"))
                .unwrap_or_default(),
            can_apply: self.can_apply(),
            can_reset: self.current.is_some(),
        }
    }
}

impl DirFleet {
    /// Reads the choices at start.
    pub(super) fn load_thumbnail_choices() -> Command<Effect, Event> {
        Self::change_choices(ThumbnailChoiceOp::Load)
    }

    fn change_choices(op: ThumbnailChoiceOp) -> Command<Effect, Event> {
        Command::request_from_shell(op)
            .then_send(|choices| Event::ThumbnailChoicesChanged { choices })
    }

    /// Choices follow items renamed or moved, `(from, to)`.
    pub(super) fn rename_choices(pairs: Vec<(String, String)>) -> Command<Effect, Event> {
        if pairs.is_empty() {
            return Command::done();
        }
        Self::change_choices(ThumbnailChoiceOp::Rename(pairs))
    }

    /// The events of the thumbnail picker and of chosen thumbnails.
    pub(super) fn picker_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        match event {
            Event::SelectThumbnail { tab, row } => Self::open_picker(model, tab, row),
            Event::ResetThumbnail { tab, row } => {
                let Some(path) = tab_id(tab)
                    .and_then(|id| model.tabs.get(&id))
                    .and_then(|state| state.row_path(row as usize))
                else {
                    return Command::done();
                };
                Self::change_choices(ThumbnailChoiceOp::Set {
                    path: path.to_string_lossy().into_owned(),
                    frame: None,
                })
            }
            Event::ResetAllThumbnails => Self::change_choices(ThumbnailChoiceOp::Clear),
            Event::ThumbnailChoicesChanged { choices } => {
                model.thumbnails.set_choices(choices.0);
                render()
            }
            Event::PickerTimed { session, timing } => {
                let Some(picker) = model.picker.as_mut().filter(|p| p.session == session) else {
                    return Command::done();
                };
                picker.running = false;
                picker.timing_known = true;
                match timing.0 {
                    Ok(length) => picker.timeline.start(
                        length.duration_ms.map(|ms| ms as f64 / 1000.0),
                        length.frame_rate_milli.map(|rate| rate as f64 / 1000.0),
                        Duration::from_millis(length.default_ms),
                        picker.current.map(Duration::from_millis),
                    ),
                    Err(error) => picker.error = Some(error),
                }
                render().and(picker.pump())
            }
            Event::PickerFrameMade {
                session,
                at_ms,
                file,
            } => {
                let Some(picker) = model.picker.as_mut().filter(|p| p.session == session) else {
                    return Command::done();
                };
                picker.running = false;
                match file {
                    // The size only bounds egui's textures; files cost nothing here.
                    Some(file) => picker.timeline.finish(at_ms, file, 0),
                    None => picker.timeline.fail(at_ms),
                }
                render().and(picker.pump())
            }
            Event::SelectPickerFrame { at_ms } => {
                if let Some(picker) = model.picker.as_mut() {
                    picker.selected = Some(at_ms);
                }
                render()
            }
            Event::StepPickerFrame { forward, steps } => {
                if let Some(picker) = model.picker.as_mut() {
                    picker.step(forward, steps);
                }
                render()
            }
            Event::FillPickerGap { from, to } => {
                let Some(picker) = model.picker.as_mut() else {
                    return Command::done();
                };
                picker.timeline.fill(Gap { from, to });
                render().and(picker.pump())
            }
            Event::UsePickerFrame => Self::close_picker_with(model, true),
            Event::ResetPickerFrame => Self::close_picker_with(model, false),
            Event::ClosePicker => {
                model.picker = None;
                render()
            }
            _ => Command::done(),
        }
    }

    /// Closes the dialog making the selected frame the thumbnail (`chosen`),
    /// once it is ready, or going back to the default.
    fn close_picker_with(model: &mut Model, chosen: bool) -> Command<Effect, Event> {
        if chosen && !model.picker.as_ref().is_some_and(Picker::can_apply) {
            return Command::done();
        }
        let Some(picker) = model.picker.take() else {
            return Command::done();
        };
        let frame = picker.selected.filter(|_| chosen).map(|at_ms| ChosenFrame {
            at_ms,
            revision: picker.revision,
            size: picker.size,
        });
        render().and(Self::change_choices(ThumbnailChoiceOp::Set {
            path: picker.path,
            frame,
        }))
    }

    /// Opens the dialog for the video at `row`, starting from its chosen frame.
    fn open_picker(model: &mut Model, tab: u32, row: u32) -> Command<Effect, Event> {
        let Some(state) = tab_id(tab).and_then(|id| model.tabs.get(&id)) else {
            return Command::done();
        };
        let Some((path, entry)) = state
            .row_path(row as usize)
            .zip(state.order.get(row as usize).map(|&entry| entry as usize))
        else {
            return Command::done();
        };
        let meta = state.listing.meta(entry);
        let path = path.to_string_lossy().into_owned();
        if !is_video(&path, meta.entry_type) {
            return Command::done();
        }
        model.picker_sessions += 1;
        let mut picker = Picker {
            tab,
            name: state.listing.name(entry).to_owned(),
            current: model.thumbnails.chosen(&path, meta),
            path,
            revision: meta.source_revision,
            size: meta.size,
            selected: None,
            timing_known: false,
            error: None,
            running: false,
            session: model.picker_sessions,
            timeline: FrameTimeline::default(),
        };
        let pump = picker.pump();
        model.picker = Some(picker);
        render().and(pump)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_shows_minutes_and_hours() {
        assert_eq!(clock(4_900), "0:04");
        assert_eq!(clock(125_000), "2:05");
        assert_eq!(clock(3_725_000), "1:02:05");
    }

    #[test]
    fn close_frames_are_labelled_with_tenths() {
        assert_eq!(fine_clock(4_950), "0:04.9");
        assert_eq!(cell_label(4_900, Some(4_500), Some(9_000)), "0:04.9");
        assert_eq!(cell_label(4_900, Some(1_000), Some(5_100)), "0:04.9");
        assert_eq!(cell_label(4_900, Some(1_000), Some(9_000)), "0:04");
        assert_eq!(cell_label(4_900, None, None), "0:04");
    }

    #[test]
    fn gap_tooltips_show_tenths_only_for_short_gaps() {
        let tooltip = |from, to| gap_view(Gap { from, to }).tooltip;
        assert_eq!(tooltip(15_000, 57_000), "Add frames between 0:15 and 0:57");
        assert_eq!(
            tooltip(1_500, 2_000),
            "Add frames between 0:01.5 and 0:02.0"
        );
    }
}
