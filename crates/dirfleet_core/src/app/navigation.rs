//! Moving a tab between folders: every visit is recorded, and Back and
//! Forward return to where you were (selection and scroll row), as egui's
//! history does, with the search each entry had.
use std::path::PathBuf;

use crux_core::Command;

use super::{DirFleet, Effect, Event, Model, Tab, tab_id};
use crate::CurrentPath;
use crate::history::{HistoryEntry, Place};
use crate::ids::TabId;
use crate::search::Search;

impl Tab {
    /// Where you are in the folder now: the first row on screen and the
    /// selected entries.
    pub(super) fn current_place(&self) -> Place {
        let selected = self
            .selection
            .rows()
            .filter_map(|row| self.row_path(row))
            .collect();
        Place {
            first_row: self.first_row,
            selected,
            ..Place::default()
        }
    }

    /// Selects and scrolls to `place` in the newly listed folder.
    pub(super) fn return_to(&mut self, place: &Place) {
        let names: std::collections::HashSet<&str> = place
            .selected
            .iter()
            .filter_map(|path| path.file_name()?.to_str())
            .collect();
        let rows: Vec<usize> = (0..self.order.len())
            .filter(|&row| {
                let entry = self.order[row] as usize;
                names.contains(self.listing.name(entry))
            })
            .collect();
        self.selection.set_rows(rows);
        self.scroll_to = Some(place.first_row);
        self.scroll_count = self.scroll_count.wrapping_add(1);
    }
}

/// The history entry of a folder, searched with `search`.
pub(super) fn entry(path: &str, search: Option<Search>) -> HistoryEntry {
    HistoryEntry::new(CurrentPath::One(PathBuf::from(path)), search)
}

impl DirFleet {
    /// Shows `path` in the tab, recording the visit.
    pub(super) fn navigate(model: &mut Model, tab: u32, path: String) -> Command<Effect, Event> {
        let Some((id, state)) = tab_id(tab).and_then(|id| Some((id, model.tabs.get_mut(&id)?)))
        else {
            return Command::done();
        };
        if state.path.is_some() {
            let place = state.current_place();
            state.history.save_place(place);
        }
        state.returning_to = None;
        model.sidebar.visited(&path);
        Self::go(model, id, path, None)
    }

    pub(super) fn back(model: &mut Model, tab: u32) -> Command<Effect, Event> {
        Self::step(model, tab, false)
    }

    pub(super) fn forward(model: &mut Model, tab: u32) -> Command<Effect, Event> {
        Self::step(model, tab, true)
    }

    /// Back or forward one entry, returning to where you were there.
    fn step(model: &mut Model, tab: u32, forward: bool) -> Command<Effect, Event> {
        let Some((id, state)) = tab_id(tab).and_then(|id| Some((id, model.tabs.get_mut(&id)?)))
        else {
            return Command::done();
        };
        let place = state.current_place();
        state.history.save_place(place);
        let target = if forward {
            state.history.forward()
        } else {
            state.history.back()
        };
        state.returning_to = state.history.place().cloned();
        match target.map(|entry| (entry.path, entry.search)) {
            Some((CurrentPath::One(path), search)) => {
                let path = path.to_string_lossy().into_owned();
                model.sidebar.visited(&path);
                Self::go(model, id, path, search)
            }
            Some((CurrentPath::Trash, _)) => Self::go_trash(model, id),
            _ => Command::done(),
        }
    }

    /// Shows the folder holding the tab's folder.
    pub(super) fn up(model: &mut Model, tab: u32) -> Command<Effect, Event> {
        let parent = tab_id(tab)
            .and_then(|id: TabId| model.tabs.get(&id))
            .and_then(Tab::location)
            .and_then(|location| location.parent())
            .map(|parent| parent.to_string());
        parent.map_or_else(Command::done, |path| Self::navigate(model, tab, path))
    }
}
