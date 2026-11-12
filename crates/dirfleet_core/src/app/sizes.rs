//! Folder sizes. Walking a tree is slow, so a folder is added up only when
//! asked: by the inspector's Calculate button, or, with
//! `Settings::folder_sizes` on, for the folders in view of a list that shows
//! the Size column and for the inspector's folder.
//!
//! A size in view is added up again when the folder itself changed (a newer
//! revision in a listing), when a file operation touched it, and once it is
//! a quarter of an hour old, since nothing tells of changes deep inside a
//! folder; the old size stays on show meanwhile.
use std::collections::{HashMap, HashSet, VecDeque};

use crux_core::Command;
use crux_core::capability::Operation;
use crux_core::render::render;

use super::inspector::SizeInfo;
use super::{DEFAULT_VISIBLE_ROWS, DirFleet, Effect, Event, Model, OVERSCAN, Tab, format};
use crate::DisplayType;
use crate::columns::Column;
use crate::listing::EntryType;

/// Folders added up at once for lists; the inspector's are not counted.
const IN_FLIGHT: usize = 2;
/// Sizes remembered at most; the oldest go first.
const KEPT: usize = 4096;
/// How long a size in view is trusted, in seconds: what changes deep inside
/// a folder without the app's doing is not told to anyone.
const FRESH_SECONDS: u64 = 15 * 60;

/// Add up the disk space under `path`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderSize {
    pub path: String,
}

impl Operation for FolderSize {
    /// Bytes, or `None` when the folder can't be read or the adding up was
    /// stopped.
    type Output = Option<u64>;
}

/// Stop adding up the folder at `path`: nothing waits for its size any more.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StopFolderSize {
    pub path: String,
}

impl Operation for StopFolderSize {
    type Output = ();
}

/// A folder being added up.
#[derive(Clone, Copy)]
struct Asked {
    /// Which asking it is: answers to earlier ones are dropped.
    ticket: u64,
    /// Asked for a list, and stopped once the folder is out of view.
    listed: bool,
}

/// The last answer about a folder.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Total {
    /// Not answered yet.
    #[default]
    Unknown,
    Bytes(u64),
    /// The folder couldn't be read.
    Unreadable,
}

#[derive(Default)]
struct Known {
    /// The folder's revision when it was last asked for from a listing.
    revision: Option<u128>,
    total: Total,
    asked: Option<Asked>,
    /// Something changed inside since `total` was added up.
    stale: bool,
    /// When `total` was added up, by the app's clock; 0 before it ticks.
    at: u64,
}

/// Folder sizes asked for.
#[derive(Default)]
pub(super) struct Sizes {
    known: HashMap<String, Known>,
    /// Paths in the order they were first asked for.
    order: VecDeque<String>,
    /// How many are being added up for lists.
    in_flight: usize,
    tickets: u64,
}

/// Whether `path` is `folder` or lies inside it.
fn within(path: &str, folder: &str) -> bool {
    path.strip_prefix(folder).is_some_and(|rest| {
        rest.is_empty() || rest.starts_with(['/', '\\']) || folder.ends_with(['/', '\\'])
    })
}

impl Sizes {
    /// What the inspector says of the folder at `path`.
    pub(super) fn info(&self, path: &str) -> SizeInfo {
        match self.known.get(path) {
            Some(Known {
                total: Total::Bytes(total),
                ..
            }) => SizeInfo::Known(format::bytes(*total)),
            Some(Known { asked: Some(_), .. }) => SizeInfo::Calculating,
            Some(Known {
                total: Total::Unreadable,
                ..
            }) => SizeInfo::Unavailable,
            _ => SizeInfo::NotCalculated,
        }
    }

    /// The Size column's text for the folder at `path`: its size once known,
    /// an ellipsis while it is first added up.
    pub(super) fn cell(&self, path: &str) -> String {
        match self.known.get(path) {
            Some(Known {
                total: Total::Bytes(total),
                ..
            }) => format::bytes(*total),
            Some(Known { asked: Some(_), .. }) => "\u{2026}".to_owned(),
            _ => String::new(),
        }
    }

    /// The sizes of `dirs`, of the folders around them and of those inside
    /// them are added up again when next wanted.
    pub(super) fn touched(&mut self, dirs: &[String]) {
        for (path, known) in &mut self.known {
            if dirs
                .iter()
                .any(|dir| within(dir, path) || within(path, dir))
            {
                known.stale = true;
            }
        }
    }

    /// Starts adding up `path`, for a list (`listed`) or the inspector.
    fn ask(
        &mut self,
        path: String,
        revision: Option<u128>,
        listed: bool,
    ) -> Command<Effect, Event> {
        self.tickets += 1;
        let ticket = self.tickets;
        if !self.known.contains_key(&path) {
            self.order.push_back(path.clone());
        }
        let known = self.known.entry(path.clone()).or_default();
        if revision.is_some() {
            known.revision = revision;
        }
        known.asked = Some(Asked { ticket, listed });
        known.stale = false;
        if listed {
            self.in_flight += 1;
        }
        Command::request_from_shell(FolderSize { path: path.clone() }).then_send(move |total| {
            Event::SizeCalculated {
                path,
                total,
                ticket,
            }
        })
    }

    /// Whether a listing showing `path` at `revision` needs it added up, at
    /// the time `now`.
    fn wants(&mut self, path: &str, revision: u128, now: u64) -> bool {
        let Some(known) = self.known.get_mut(path) else {
            return true;
        };
        if known.asked.is_some() {
            return false;
        }
        // Asked for by the inspector, which knows no revision: this one.
        let seen = *known.revision.get_or_insert(revision);
        // Only a newer folder: two listings of different ages would
        // otherwise take turns asking for ever.
        let old = now > 0 && known.at > 0 && now.saturating_sub(known.at) > FRESH_SECONDS;
        known.stale || revision > seen || old
    }

    /// Stops what is added up for lists and is in none of `wanted`.
    fn stop_unwanted(&mut self, wanted: &HashSet<&str>) -> Vec<Command<Effect, Event>> {
        let mut stops = Vec::new();
        let mut forgotten = Vec::new();
        for (path, known) in &mut self.known {
            if !known.asked.is_some_and(|asked| asked.listed) || wanted.contains(path.as_str()) {
                continue;
            }
            known.asked = None;
            self.in_flight = self.in_flight.saturating_sub(1);
            if known.total == Total::Unknown {
                forgotten.push(path.clone());
            }
            stops.push(Command::notify_shell(StopFolderSize { path: path.clone() }).into());
        }
        if !forgotten.is_empty() {
            for path in forgotten {
                self.known.remove(&path);
            }
            // Or scrolling back and forth would list a folder many times.
            let known = &self.known;
            self.order.retain(|path| known.contains_key(path));
        }
        stops
    }

    /// Forgets the oldest sizes beyond [`KEPT`].
    fn trim(&mut self) {
        let mut looked = 0;
        while self.known.len() > KEPT && looked < self.order.len() {
            let Some(path) = self.order.pop_front() else {
                return;
            };
            match self.known.get(&path) {
                Some(known) if known.asked.is_some() => {
                    self.order.push_back(path);
                    looked += 1;
                }
                Some(_) => {
                    self.known.remove(&path);
                }
                // Already forgotten.
                None => {}
            }
        }
    }
}

impl Tab {
    /// The folders in view (with overscan), with their revisions: those in
    /// view first.
    fn folders_in_view(&self, wanted: &mut Vec<(String, u128)>) {
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
            wanted.extend(self.folder_at(row));
        }
    }

    /// The folder at `row` on this machine, and its revision.
    fn folder_at(&self, row: usize) -> Option<(String, u128)> {
        let meta = self.listing.meta(*self.order.get(row)? as usize);
        if meta.entry_type != EntryType::Directory || self.trash {
            return None;
        }
        let path = self.row_path(row)?.to_string_lossy().into_owned();
        Some((path, meta.source_revision))
    }
}

impl DirFleet {
    /// With `Settings::folder_sizes` on, asks for the sizes of the folders in
    /// view of the lists that show the Size column, and of the inspector's
    /// folder, up to [`IN_FLIGHT`] at once; stops those no longer in view.
    pub(super) fn request_sizes(model: &mut Model) -> Command<Effect, Event> {
        let on = model.settings.folder_sizes;
        if !on && model.sizes.in_flight == 0 {
            return Command::done();
        }
        let mut wanted = Vec::new();
        if on {
            let in_lists = model.columns.is_shown(Column::Size);
            for window in &model.windows {
                if model.inspector_shown
                    && let Some(tab) = model.tabs.get(&window.focused)
                    && let Some(row) = tab.single_item()
                {
                    wanted.extend(tab.folder_at(row));
                }
                for id in window.layout.shown() {
                    if let Some(tab) = model.tabs.get(&id)
                        && in_lists
                        && tab.view.display == DisplayType::List
                    {
                        tab.folders_in_view(&mut wanted);
                    }
                }
            }
        }
        let now = model.now;
        let sizes = &mut model.sizes;
        let mut commands =
            sizes.stop_unwanted(&wanted.iter().map(|(path, _)| path.as_str()).collect());
        for (path, revision) in wanted {
            if sizes.in_flight >= IN_FLIGHT {
                break;
            }
            if sizes.wants(&path, revision, now) {
                commands.push(sizes.ask(path, Some(revision), true));
            }
        }
        sizes.trim();
        Command::all(commands)
    }

    /// The events folder sizes answer to.
    pub(super) fn size_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        let now = model.now;
        let sizes = &mut model.sizes;
        match event {
            Event::CalculateSize { path } => {
                match sizes
                    .known
                    .get_mut(&path)
                    .and_then(|known| known.asked.as_mut())
                {
                    // Already added up for a list: now nothing stops it.
                    Some(asked) if asked.listed => {
                        asked.listed = false;
                        sizes.in_flight = sizes.in_flight.saturating_sub(1);
                        Command::done()
                    }
                    Some(_) => Command::done(),
                    None => render().and(sizes.ask(path, None, false)),
                }
            }
            Event::SizeCalculated {
                path,
                total,
                ticket,
            } => {
                let Some(known) = sizes.known.get_mut(&path) else {
                    return Command::done();
                };
                let Some(asked) = known.asked.filter(|asked| asked.ticket == ticket) else {
                    return Command::done();
                };
                if asked.listed {
                    sizes.in_flight = sizes.in_flight.saturating_sub(1);
                }
                known.asked = None;
                // Still stale when something changed while it was added up.
                known.total = total.map_or(Total::Unreadable, Total::Bytes);
                known.at = now;
                render()
            }
            _ => Command::done(),
        }
    }
}
