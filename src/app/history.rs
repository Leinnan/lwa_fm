//! Per-tab back/forward navigation history.
//!
//! Every navigation is recorded, unlike egui's time-based `Undoer`, which
//! dropped folders that were visited for less than its stable time. An entry
//! also remembers the search shown there, so Back returns to earlier results.

use super::Search;
use super::dock::CurrentPath;
use std::path::PathBuf;

/// The most locations remembered per tab.
const MAX_ENTRIES: usize = 100;

/// One place in the history: a folder (or folders) and the search shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub path: CurrentPath,
    pub search: Option<Search>,
}

impl HistoryEntry {
    pub const fn new(path: CurrentPath, search: Option<Search>) -> Self {
        Self { path, search }
    }

    fn same_place(&self, other: &Self) -> bool {
        self.path == other.path
            && match (&self.search, &other.search) {
                (None, None) => true,
                (Some(a), Some(b)) => a.same_query(b),
                // An open but empty search is the same place as no search.
                (Some(s), None) | (None, Some(s)) => !s.has_query(),
            }
    }
}

/// Where you were in a place when you left it, so Back and Forward return
/// you there: the scroll position and the selected items.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Place {
    pub scroll_y: f32,
    pub selected: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct History {
    entries: Vec<(HistoryEntry, Place)>,
    index: usize,
}

impl History {
    /// Records arriving at `entry`. Going back and then somewhere new drops
    /// the forward entries, like a browser. Arriving at the current entry
    /// (for example after `back`) records nothing.
    pub fn visit(&mut self, entry: HistoryEntry) {
        if matches!(entry.path, CurrentPath::None) {
            return;
        }
        if let Some((current, _)) = self.entries.get_mut(self.index)
            && current.same_place(&entry)
        {
            // Keep the latest form, e.g. the search options.
            *current = entry;
            return;
        }
        if !self.entries.is_empty() {
            self.entries.truncate(self.index + 1);
        }
        self.entries.push((entry, Place::default()));
        if self.entries.len() > MAX_ENTRIES {
            self.entries.remove(0);
        }
        self.index = self.entries.len() - 1;
    }

    pub const fn can_go_back(&self) -> bool {
        self.index > 0
    }

    pub const fn can_go_forward(&self) -> bool {
        self.index + 1 < self.entries.len()
    }

    /// Steps back and returns the place to restore.
    pub fn back(&mut self) -> Option<HistoryEntry> {
        if !self.can_go_back() {
            return None;
        }
        self.index -= 1;
        self.entries.get(self.index).map(|(entry, _)| entry.clone())
    }

    /// Steps forward and returns the place to restore.
    pub fn forward(&mut self) -> Option<HistoryEntry> {
        if !self.can_go_forward() {
            return None;
        }
        self.index += 1;
        self.entries.get(self.index).map(|(entry, _)| entry.clone())
    }

    /// Remembers where you are in the current entry, before leaving it.
    pub fn save_place(&mut self, place: Place) {
        if let Some((_, current)) = self.entries.get_mut(self.index) {
            *current = place;
        }
    }

    /// Where you were in the current entry when you last left it.
    pub fn place(&self) -> Option<&Place> {
        self.entries.get(self.index).map(|(_, place)| place)
    }
}

#[cfg(test)]
mod tests {
    use super::{History, HistoryEntry};
    use crate::app::Search;
    use crate::app::dock::CurrentPath;
    use std::path::PathBuf;

    fn at(name: &str) -> HistoryEntry {
        HistoryEntry::new(CurrentPath::One(PathBuf::from(name)), None)
    }

    fn searching(name: &str, text: &str) -> HistoryEntry {
        HistoryEntry::new(
            CurrentPath::One(PathBuf::from(name)),
            Some(Search {
                value: text.into(),
                ..Default::default()
            }),
        )
    }

    #[test]
    fn back_and_forward_walk_every_visit() {
        let mut history = History::default();
        for name in ["a", "b", "c"] {
            history.visit(at(name));
        }
        assert!(!history.can_go_forward());
        let back = history.back().expect("b");
        assert_eq!(back, at("b"));
        // Arriving at the restored location records nothing.
        history.visit(back);
        assert_eq!(history.back(), Some(at("a")));
        assert!(!history.can_go_back());
        assert_eq!(history.forward(), Some(at("b")));
        assert_eq!(history.forward(), Some(at("c")));
        assert_eq!(history.forward(), None);
    }

    #[test]
    fn visiting_after_going_back_drops_forward_entries() {
        let mut history = History::default();
        for name in ["a", "b", "c"] {
            history.visit(at(name));
        }
        history.back();
        history.back();
        history.visit(at("d"));
        assert!(!history.can_go_forward());
        assert_eq!(history.back(), Some(at("a")));
    }

    #[test]
    fn going_back_returns_to_where_you_were() {
        let mut history = History::default();
        history.visit(at("a"));
        let place = super::Place {
            scroll_y: 120.0,
            selected: vec![PathBuf::from("a/x")],
        };
        history.save_place(place.clone());
        history.visit(at("b"));
        assert_eq!(history.place(), Some(&super::Place::default()));
        history.back();
        assert_eq!(history.place(), Some(&place));
    }

    #[test]
    fn searches_are_places_in_the_history() {
        let mut history = History::default();
        history.visit(at("a"));
        history.visit(searching("a", "x"));
        history.visit(searching("a", "xy"));
        history.visit(at("a/sub"));
        assert_eq!(history.back(), Some(searching("a", "xy")));
        assert_eq!(history.back(), Some(searching("a", "x")));
        assert_eq!(history.back(), Some(at("a")));
    }
}
