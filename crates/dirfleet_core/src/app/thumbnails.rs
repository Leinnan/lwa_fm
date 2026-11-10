//! Thumbnails and animated previews of the images and videos on screen, as
//! egui's.
//!
//! After each event the core asks for the media files each shown tab has in
//! view, sized for how the tab is drawn, and for the inspector's item at a
//! larger size, a few at a time. They come back as image files in the
//! thumbnail cache, which shells draw. A file that changed since gets a new
//! thumbnail.
//!
//! Videos and GIFs also play, as animated GIFs made from their frames: in the
//! inspector always, on a grid tile once the pointer has rested on it for
//! [`HOVER_MILLIS`], and on the one selected tile when
//! `Settings::animate_selected_previews` is on.
use std::collections::{HashMap, VecDeque};
use std::path::Path;

use crux_core::Command;
use crux_core::capability::Operation;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::picker::{self, ChosenFrame, ThumbnailChoice};
use super::{
    DEFAULT_VISIBLE_ROWS, Delay, DirFleet, Effect, Event, Model, OVERSCAN, Tab, ToastKind, tab_id,
};
use crate::DisplayType;
use crate::listing::kinds::{MediaKind, media_kind};
use crate::listing::{DirEntryMetaData, EntryType};

/// Thumbnails asked for and not answered yet, at most.
const IN_FLIGHT: usize = 8;
/// Thumbnails remembered at most; the oldest go first.
const KEPT: usize = 4096;
/// A list row's thumbnail edge, in pixels, as egui's.
pub const LIST_EDGE: u32 = 64;
/// The inspector's preview edge, in pixels.
pub const PREVIEW_EDGE: u32 = 480;
/// Pixels per point on the displays thumbnails are made for.
const SCALE: u32 = 2;
/// How long the pointer rests on a tile before it plays, as egui's.
pub const HOVER_MILLIS: u64 = 250;

/// Makes (or finds in the cache) the thumbnail of `path`, fitted to `edge`
/// pixels.
///
/// With `animated` it is a looping GIF of a video's or GIF's frames.
/// `revision` (modified time in nanoseconds) and `size` say which version of
/// the file it is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thumbnail {
    pub path: String,
    pub edge: u32,
    pub revision: u128,
    pub size: u64,
    pub animated: bool,
}

impl Operation for Thumbnail {
    /// The image file to draw, or `None` when the file has no thumbnail.
    type Output = Option<String>;
}

/// Notices from the media service, such as `FFmpeg` being downloaded, for
/// as long as the app runs. Starting it also fetches `FFmpeg` when it is
/// missing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaNotices;

/// A notice to show as a toast. Opaque to facet: it never leaves the core.
#[derive(Clone, Debug, PartialEq, Eq, Facet)]
#[facet(opaque)]
pub struct MediaNotice {
    pub kind: ToastKind,
    pub text: String,
}

impl Operation for MediaNotices {
    type Output = MediaNotice;
}

/// The inspector's preview of its item.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum PreviewState {
    /// Not an image or a video.
    None,
    Loading,
    /// The image file to draw.
    Ready(String),
    /// It couldn't be made: `RetryPreview` tries again.
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    path: String,
    edge: u32,
    animated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum State {
    Asked,
    Made(String),
    Failed,
}

/// A thumbnail and the version of the file it is for: its revision, size
/// and, for a video's poster, the frame chosen for it.
#[derive(Clone, Debug)]
struct Known {
    revision: u128,
    size: u64,
    frame: Option<u64>,
    state: State,
}

#[derive(Default)]
pub(super) struct Thumbnails {
    known: HashMap<Key, Known>,
    /// Keys in the order they were first asked for.
    order: VecDeque<Key>,
    in_flight: usize,
    /// The frames chosen as videos' thumbnails, as the effects keep them.
    choices: HashMap<String, ChosenFrame>,
}

/// The pointer resting on a grid tile.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Hover {
    row: Option<u32>,
    /// Bumped on every move, so a late dwell for an earlier tile is dropped.
    count: u64,
    /// The pointer has rested long enough for the tile to play.
    rested: bool,
}

impl Thumbnails {
    /// The frame chosen for this version of the video at `path`, if any.
    pub(super) fn chosen(&self, path: &str, meta: &DirEntryMetaData) -> Option<u64> {
        self.chosen_for(path, meta.source_revision, meta.size)
    }

    fn chosen_for(&self, path: &str, revision: u128, size: u64) -> Option<u64> {
        self.choices
            .get(path)
            .filter(|chosen| (chosen.revision, chosen.size) == (revision, size))
            .map(|chosen| chosen.at_ms)
    }

    /// Whether the row's thumbnail can be chosen, and was.
    pub(super) fn choice(&self, path: &str, meta: &DirEntryMetaData) -> ThumbnailChoice {
        if !picker::is_video(path, meta.entry_type) {
            ThumbnailChoice::None
        } else if self.chosen(path, meta).is_some() {
            ThumbnailChoice::Chosen
        } else {
            ThumbnailChoice::Automatic
        }
    }

    /// The choices changed: posters made for others are asked for again.
    pub(super) fn set_choices(&mut self, choices: Vec<(String, ChosenFrame)>) {
        self.choices = choices.into_iter().collect();
    }

    /// The frame a thumbnail is made from: the chosen one for a poster.
    fn frame_of(&self, path: &str, animated: bool, revision: u128, size: u64) -> Option<u64> {
        if animated {
            None
        } else {
            self.chosen_for(path, revision, size)
        }
    }

    fn state(
        &self,
        path: &str,
        edge: u32,
        animated: bool,
        meta: &DirEntryMetaData,
    ) -> Option<&State> {
        let known = self.known.get(&Key {
            path: path.to_owned(),
            edge,
            animated,
        })?;
        let current = (
            meta.source_revision,
            meta.size,
            self.frame_of(path, animated, meta.source_revision, meta.size),
        );
        ((known.revision, known.size, known.frame) == current).then_some(&known.state)
    }

    fn file_of(&self, path: &str, edge: u32, animated: bool, meta: &DirEntryMetaData) -> String {
        match self.state(path, edge, animated, meta) {
            Some(State::Made(file)) => file.clone(),
            _ => String::new(),
        }
    }

    /// The image file for a row, or "" while there is none.
    pub(super) fn file(&self, path: &str, edge: u32, meta: &DirEntryMetaData) -> String {
        self.file_of(path, edge, false, meta)
    }

    /// The animated GIF for a row that plays, or "" while there is none.
    pub(super) fn animation(&self, path: &str, edge: u32, meta: &DirEntryMetaData) -> String {
        self.file_of(path, edge, true, meta)
    }

    pub(super) fn preview(&self, path: &str, meta: &DirEntryMetaData) -> PreviewState {
        if !wants_thumbnail(path, meta) {
            return PreviewState::None;
        }
        match self.state(path, PREVIEW_EDGE, false, meta) {
            Some(State::Made(file)) => PreviewState::Ready(file.clone()),
            Some(State::Failed) => PreviewState::Unavailable,
            Some(State::Asked) | None => PreviewState::Loading,
        }
    }

    /// Forgets the oldest answered thumbnails beyond [`KEPT`].
    fn trim(&mut self) {
        while self.known.len() > KEPT {
            let Some(key) = self.order.pop_front() else {
                return;
            };
            if self
                .known
                .get(&key)
                .is_some_and(|known| known.state == State::Asked)
            {
                self.order.push_back(key);
                continue;
            }
            self.known.remove(&key);
        }
    }
}

fn wants_thumbnail(path: &str, meta: &DirEntryMetaData) -> bool {
    meta.entry_type == EntryType::File && media_kind(Path::new(path)).is_some()
}

pub(super) fn plays(path: &str, meta: &DirEntryMetaData) -> bool {
    meta.entry_type == EntryType::File
        && media_kind(Path::new(path)).is_some_and(MediaKind::is_animated)
}

/// The edge a tab's rows are drawn at, in pixels.
pub(super) const fn row_edge(tab: &Tab, model: &Model) -> u32 {
    match tab.view.display {
        DisplayType::List => LIST_EDGE,
        DisplayType::Icons => model.settings.icon_size.points() * SCALE,
    }
}

impl Tab {
    /// The media files in view (with overscan) that need a thumbnail.
    fn wanted_thumbnails(&self, edge: u32, wanted: &mut Vec<Thumbnail>) {
        let total = self.order.len();
        let visible = if self.visible_rows == 0 {
            DEFAULT_VISIBLE_ROWS
        } else {
            self.visible_rows
        } as usize;
        let first = self.first_row as usize;
        let end = first
            .saturating_add(visible)
            .saturating_add(OVERSCAN as usize)
            .min(total);
        // Rows in view first, then the overscan around them.
        let rows = (first.min(end)..end)
            .chain((first.saturating_sub(OVERSCAN as usize)..first.min(total)).rev());
        for row in rows {
            if let Some(thumbnail) = self.thumbnail_of(row, edge, false) {
                wanted.push(thumbnail);
            }
        }
    }

    /// What row `row` needs, when it is an image or a video (`animated`: a
    /// video or a GIF).
    fn thumbnail_of(&self, row: usize, edge: u32, animated: bool) -> Option<Thumbnail> {
        let meta = self.listing.meta(*self.order.get(row)? as usize);
        let path = self.row_path(row)?.to_string_lossy().into_owned();
        let wanted = if animated {
            plays(&path, meta)
        } else {
            wants_thumbnail(&path, meta)
        };
        wanted.then_some(Thumbnail {
            path,
            edge,
            revision: meta.source_revision,
            size: meta.size,
            animated,
        })
    }

    /// The one item selected, or the cursor when nothing is.
    pub(super) fn single_item(&self) -> Option<usize> {
        match self.selection.len() {
            0 => self.selection.cursor(),
            1 => self.selection.rows().next(),
            _ => None,
        }
    }

    /// The grid tile that plays: the one the pointer rests on, or else the
    /// one selected tile when `selected` previews play.
    pub(super) fn playing_row(&self, selected: bool) -> Option<usize> {
        if self.view.display != DisplayType::Icons {
            return None;
        }
        if self.hover.rested
            && let Some(row) = self.hover.row
        {
            return Some(row as usize);
        }
        if !selected {
            return None;
        }
        match self.selection.rows().collect::<Vec<_>>().as_slice() {
            [row] => Some(*row),
            _ => None,
        }
    }
}

impl DirFleet {
    /// Asks for the thumbnails the shown tabs and the inspector need and
    /// don't have, up to [`IN_FLIGHT`] at once: what plays first, then the
    /// inspector's preview, then the rows in view.
    pub(super) fn request_thumbnails(model: &mut Model) -> Command<Effect, Event> {
        if model.thumbnails.in_flight >= IN_FLIGHT {
            return Command::done();
        }
        let selected = model.settings.animate_selected_previews;
        let mut wanted = Vec::new();
        for window in &model.windows {
            if model.inspector_shown
                && let Some(tab) = model.tabs.get(&window.focused)
                && let Some(row) = tab.single_item()
            {
                wanted.extend(tab.thumbnail_of(row, PREVIEW_EDGE, true));
                wanted.extend(tab.thumbnail_of(row, PREVIEW_EDGE, false));
            }
            for id in window.layout.shown() {
                let Some(tab) = model.tabs.get(&id) else {
                    continue;
                };
                let edge = row_edge(tab, model);
                if let Some(row) = tab.playing_row(selected) {
                    wanted.extend(tab.thumbnail_of(row, edge, true));
                }
                tab.wanted_thumbnails(edge, &mut wanted);
            }
        }
        let thumbnails = &mut model.thumbnails;
        let mut commands = Vec::new();
        for request in wanted {
            if thumbnails.in_flight >= IN_FLIGHT {
                break;
            }
            let key = Key {
                path: request.path.clone(),
                edge: request.edge,
                animated: request.animated,
            };
            let frame = thumbnails.frame_of(
                &request.path,
                request.animated,
                request.revision,
                request.size,
            );
            if thumbnails.known.get(&key).is_some_and(|known| {
                (known.revision, known.size, known.frame) == (request.revision, request.size, frame)
            }) {
                continue;
            }
            if thumbnails
                .known
                .insert(
                    key.clone(),
                    Known {
                        revision: request.revision,
                        size: request.size,
                        frame,
                        state: State::Asked,
                    },
                )
                .is_none()
            {
                thumbnails.order.push_back(key);
            }
            thumbnails.in_flight += 1;
            let Thumbnail {
                path,
                edge,
                revision,
                size,
                animated,
            } = request.clone();
            commands.push(Command::request_from_shell(request).then_send(move |file| {
                Event::ThumbnailMade {
                    path,
                    edge,
                    animated,
                    revision,
                    size,
                    frame,
                    file,
                }
            }));
        }
        thumbnails.trim();
        Command::all(commands)
    }

    /// Listens to the media service from the start.
    pub(super) fn start_media() -> Command<Effect, Event> {
        Command::stream_from_shell(MediaNotices).then_send(|notice| Event::MediaNoticed { notice })
    }

    /// The events thumbnails and previews answer to.
    pub(super) fn thumbnail_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        let thumbnails = &mut model.thumbnails;
        match event {
            Event::ThumbnailMade {
                path,
                edge,
                animated,
                revision,
                size,
                frame,
                file,
            } => {
                thumbnails.in_flight = thumbnails.in_flight.saturating_sub(1);
                let Some(known) = thumbnails.known.get_mut(&Key {
                    path,
                    edge,
                    animated,
                }) else {
                    return Command::done();
                };
                // An answer for an older version of the file, or for the
                // frame chosen before, is dropped.
                if (known.revision, known.size, known.frame) != (revision, size, frame) {
                    return Command::done();
                }
                known.state = file.map_or(State::Failed, State::Made);
                render()
            }
            Event::RetryPreview { path } => {
                for animated in [false, true] {
                    let key = Key {
                        path: path.clone(),
                        edge: PREVIEW_EDGE,
                        animated,
                    };
                    if thumbnails
                        .known
                        .get(&key)
                        .is_some_and(|known| known.state == State::Failed)
                    {
                        thumbnails.known.remove(&key);
                    }
                }
                render()
            }
            Event::HoverRow { tab, row, inside } => Self::hover(model, tab, row, inside),
            Event::HoverRested { tab, count } => {
                let Some(state) = tab_id(tab).and_then(|id| model.tabs.get_mut(&id)) else {
                    return Command::done();
                };
                if state.hover.count != count || state.hover.row.is_none() {
                    return Command::done();
                }
                state.hover.rested = true;
                render()
            }
            Event::MediaNoticed { notice } => {
                // FFmpeg is ready: what failed for want of it is asked again.
                if notice.kind == ToastKind::Success {
                    let thumbnails = &mut model.thumbnails;
                    thumbnails
                        .known
                        .retain(|_, known| known.state != State::Failed);
                    let known = &thumbnails.known;
                    thumbnails.order.retain(|key| known.contains_key(key));
                }
                Self::toast(model, notice.kind, notice.text)
            }
            _ => Command::done(),
        }
    }

    /// The pointer entered (`inside`) or left the tile `row`.
    fn hover(model: &mut Model, tab: u32, row: u32, inside: bool) -> Command<Effect, Event> {
        let Some(state) = tab_id(tab).and_then(|id| model.tabs.get_mut(&id)) else {
            return Command::done();
        };
        // Leaving a tile after entering the next one changes nothing.
        if !inside && state.hover.row != Some(row) {
            return Command::done();
        }
        let was_playing = state.hover.rested;
        state.hover = Hover {
            row: inside.then_some(row),
            count: state.hover.count + 1,
            rested: false,
        };
        let playable = inside
            && state.thumbnail_of(row as usize, 0, true).is_some()
            && state.view.display == DisplayType::Icons;
        let count = state.hover.count;
        let wait = if playable {
            Command::request_from_shell(Delay {
                millis: HOVER_MILLIS,
            })
            .then_send(move |()| Event::HoverRested { tab, count })
        } else {
            Command::done()
        };
        if was_playing {
            render().and(wait)
        } else {
            wait
        }
    }
}
