//! How large pictures and videos are in pixels and how long videos and
//! sounds play, for the Dimensions and Duration columns and the inspector.
//!
//! After each event the core asks about the media files in view of the
//! lists that show one of the two columns, and about the inspector's item,
//! a few at a time. A file that changed since is asked about again.
use std::collections::{HashMap, VecDeque};
use std::path::Path;

use crux_core::Command;
use crux_core::capability::Operation;
use crux_core::render::render;

use super::{DEFAULT_VISIBLE_ROWS, DirFleet, Effect, Event, Model, OVERSCAN, Tab, format};
use crate::DisplayType;
use crate::columns::Column;
use crate::listing::kinds::{MediaKind, is_audio, media_kind};
use crate::listing::{DirEntryMetaData, EntryType};

/// Files asked about and not answered yet, at most.
const IN_FLIGHT: usize = 4;
/// Answers remembered at most; the oldest go first.
const KEPT: usize = 4096;

/// Read the pixel size and the playing time of the media file at `path`.
/// `revision` and `size` say which version of the file it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeMedia {
    pub path: String,
    pub revision: u128,
    pub size: u64,
}

/// What was read of a media file; 0 for what it has not or what could not
/// be read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MediaInfo {
    pub width: u32,
    pub height: u32,
    /// How long it plays, in milliseconds.
    pub millis: u64,
}

impl Operation for ProbeMedia {
    type Output = MediaInfo;
}

struct Known {
    revision: u128,
    size: u64,
    /// `None` while asked.
    info: Option<MediaInfo>,
}

#[derive(Default)]
pub(super) struct MediaInfos {
    known: HashMap<String, Known>,
    /// Paths in the order they were first asked about.
    order: VecDeque<String>,
    in_flight: usize,
}

/// What a file has to read: `(dimensions, duration)`.
fn has(path: &str, meta: &DirEntryMetaData) -> (bool, bool) {
    if meta.entry_type != EntryType::File {
        return (false, false);
    }
    let path = Path::new(path);
    match media_kind(path) {
        Some(MediaKind::Image | MediaKind::AnimatedImage) => (true, false),
        Some(MediaKind::Video) => (true, true),
        None => (false, is_audio(path)),
    }
}

impl MediaInfos {
    fn info(&self, path: &str, meta: &DirEntryMetaData) -> Option<MediaInfo> {
        self.known
            .get(path)
            .filter(|known| (known.revision, known.size) == (meta.source_revision, meta.size))
            .and_then(|known| known.info)
    }

    /// "1920 × 1080", or nothing while unknown.
    pub(super) fn dimensions(&self, path: &str, meta: &DirEntryMetaData) -> String {
        self.info(path, meta)
            .filter(|info| info.width > 0 && info.height > 0)
            .map_or_else(String::new, |info| {
                format::dimensions(info.width, info.height)
            })
    }

    /// "3:07", or nothing while unknown.
    pub(super) fn duration(&self, path: &str, meta: &DirEntryMetaData) -> String {
        self.info(path, meta)
            .filter(|info| info.millis > 0)
            .map_or_else(String::new, |info| format::duration(info.millis))
    }

    /// What could not be read is asked about again: the tool that reads
    /// videos and sounds has just arrived.
    pub(super) fn retry_unread(&mut self) {
        self.known
            .retain(|_, known| known.info != Some(MediaInfo::default()));
        let known = &self.known;
        self.order.retain(|path| known.contains_key(path));
    }

    /// Forgets the oldest answers beyond [`KEPT`].
    fn trim(&mut self) {
        let mut looked = 0;
        while self.known.len() > KEPT && looked < self.order.len() {
            let Some(path) = self.order.pop_front() else {
                return;
            };
            if self
                .known
                .get(&path)
                .is_some_and(|known| known.info.is_none())
            {
                self.order.push_back(path);
                looked += 1;
                continue;
            }
            self.known.remove(&path);
        }
    }
}

impl Tab {
    /// What to ask about the file at `row`, when it has dimensions
    /// (`dimensions`) or a duration (`duration`) that is wanted.
    fn probe_of(&self, row: usize, dimensions: bool, duration: bool) -> Option<ProbeMedia> {
        let meta = self.listing.meta(*self.order.get(row)? as usize);
        if self.trash || meta.entry_type != EntryType::File {
            return None;
        }
        let path = self.row_path(row)?.to_string_lossy().into_owned();
        let (sized, timed) = has(&path, meta);
        ((dimensions && sized) || (duration && timed)).then_some(ProbeMedia {
            path,
            revision: meta.source_revision,
            size: meta.size,
        })
    }

    /// The media files in view (with overscan): those in view first.
    fn media_in_view(&self, dimensions: bool, duration: bool, wanted: &mut Vec<ProbeMedia>) {
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
        let rows = (first.min(end)..end)
            .chain((first.saturating_sub(OVERSCAN as usize)..first.min(total)).rev());
        for row in rows {
            wanted.extend(self.probe_of(row, dimensions, duration));
        }
    }
}

impl DirFleet {
    /// Asks about the media files the lists and the inspector show and
    /// nothing is known of, up to [`IN_FLIGHT`] at once.
    pub(super) fn request_media_info(model: &mut Model) -> Command<Effect, Event> {
        let dimensions = model.columns.is_shown(Column::Dimensions);
        let duration = model.columns.is_shown(Column::Duration);
        if model.media.in_flight >= IN_FLIGHT || !(dimensions || duration || model.inspector_shown)
        {
            return Command::done();
        }
        let mut wanted = Vec::new();
        for window in &model.windows {
            if model.inspector_shown
                && let Some(tab) = model.tabs.get(&window.focused)
                && let Some(row) = tab.single_item()
            {
                wanted.extend(tab.probe_of(row, true, true));
            }
            if !(dimensions || duration) {
                continue;
            }
            for id in window.layout.shown() {
                if let Some(tab) = model.tabs.get(&id)
                    && tab.view.display == DisplayType::List
                {
                    tab.media_in_view(dimensions, duration, &mut wanted);
                }
            }
        }
        let media = &mut model.media;
        let mut commands = Vec::new();
        for request in wanted {
            if media.in_flight >= IN_FLIGHT {
                break;
            }
            if media.known.get(&request.path).is_some_and(|known| {
                (known.revision, known.size) == (request.revision, request.size)
            }) {
                continue;
            }
            let known = Known {
                revision: request.revision,
                size: request.size,
                info: None,
            };
            if media.known.insert(request.path.clone(), known).is_none() {
                media.order.push_back(request.path.clone());
            }
            media.in_flight += 1;
            let ProbeMedia {
                path,
                revision,
                size,
            } = request.clone();
            commands.push(Command::request_from_shell(request).then_send(move |info| {
                Event::MediaProbed {
                    path,
                    revision,
                    size,
                    width: info.width,
                    height: info.height,
                    millis: info.millis,
                }
            }));
        }
        media.trim();
        Command::all(commands)
    }

    /// What was read of a media file, unless the file changed meanwhile.
    pub(super) fn media_probed(
        model: &mut Model,
        path: &str,
        version: (u128, u64),
        info: MediaInfo,
    ) -> Command<Effect, Event> {
        let media = &mut model.media;
        media.in_flight = media.in_flight.saturating_sub(1);
        match media.known.get_mut(path) {
            Some(known) if (known.revision, known.size) == version => {
                known.info = Some(info);
                render()
            }
            _ => Command::done(),
        }
    }
}
