//! Which rows of a tab are selected, kept on the same entries when the
//! rows move (a new sort, a refresh): positions are remembered by name and
//! found again. Gestures follow egui's `Selection`: click selects one,
//! Cmd-click toggles, Shift-click selects from the anchor.
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, PoisonError};

use egui_mev_core::selection::{SelectMods, Selection};
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::Tab;
use super::inspector::SelectionSummary;
use crate::plugins::manifest::TargetInfo;

/// A tab's selection with a count of its changes. What a view derives from
/// the selection (its ranges, the items plugins get, the inspector's
/// summary) is worked out once per change and kept here: with 100,000 rows
/// selected, working it out for every view took tens of milliseconds.
#[derive(Default)]
pub(super) struct Tracked {
    selection: Selection,
    /// Goes up whenever the selection may have changed.
    revision: u64,
    memo: Mutex<Memo>,
}

/// What was derived from the selection, each with the state it was
/// derived from.
#[derive(Default)]
struct Memo {
    ranges: Option<(u64, Vec<RowRange>)>,
    targets: Option<(Stamp, Arc<Vec<TargetInfo>>)>,
    targets_key: Option<(Stamp, u64)>,
    summary: Option<(Stamp, SelectionSummary)>,
}

/// The selection's revision and the listing it selects from: rows keep
/// their positions only while both stay.
type Stamp = (u64, usize, usize);

impl Deref for Tracked {
    type Target = Selection;

    fn deref(&self) -> &Selection {
        &self.selection
    }
}

impl DerefMut for Tracked {
    /// Whoever may change the selection comes through here.
    fn deref_mut(&mut self) -> &mut Selection {
        self.revision = self.revision.wrapping_add(1);
        &mut self.selection
    }
}

impl Tracked {
    fn memo(&self) -> std::sync::MutexGuard<'_, Memo> {
        self.memo.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Rows `start..end` of a tab, in display order.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowRange {
    pub start: u32,
    pub end: u32,
}

/// An entry to find again: its name, and its folder where the rows come from
/// several folders (two of them may hold a file of the same name).
struct KeptEntry {
    dir: Option<Arc<str>>,
    name: String,
}

/// The selection by name, to find again after the rows moved.
pub struct Kept {
    entries: Vec<KeptEntry>,
    cursor: Option<KeptEntry>,
}

/// Rows on each side of the cursor Quick Look can step to.
const QUICK_LOOK_AROUND: usize = 500;

impl Tab {
    /// Selects the next row whose name starts with the typed text, as
    /// egui does; repeating one letter cycles through its matches.
    /// `continuing` says the text extends what was typed just before.
    pub(super) fn type_select(&mut self, prefix: &str, continuing: bool) {
        let repeated = prefix.chars().count() > 1 && {
            let mut chars = prefix.chars();
            let first = chars.next();
            chars.all(|c| Some(c) == first)
        };
        let needle: String = if repeated {
            prefix.chars().take(1).collect()
        } else {
            prefix.to_owned()
        };
        let len = self.order.len();
        if len == 0 || needle.is_empty() {
            return;
        }
        let start = match self.selection.cursor() {
            Some(row) if !continuing || repeated => row + 1,
            Some(row) => row,
            None => 0,
        };
        let folded = self.listing.folded_names();
        let found = (0..len).map(|offset| (start + offset) % len).find(|&row| {
            self.order
                .get(row)
                .is_some_and(|&index| folded.get(index as usize).starts_with(&needle))
        });
        if let Some(row) = found {
            self.selection.select_only(row);
            self.scroll_to = Some(u32::try_from(row).unwrap_or(0));
            self.scroll_count = self.scroll_count.wrapping_add(1);
        }
    }

    fn name_at(&self, position: usize) -> Option<&str> {
        let entry = *self.order.get(position)? as usize;
        Some(self.listing.name(entry))
    }

    /// The listing's rows come from several folders, so a name alone does
    /// not say which entry is meant.
    fn rows_have_folders(&self) -> bool {
        self.listing.dirs().len() > 1
    }

    fn kept_at(&self, position: usize, folders: bool) -> Option<KeptEntry> {
        let entry = *self.order.get(position)? as usize;
        Some(KeptEntry {
            dir: folders.then(|| Arc::clone(self.listing.dir(entry))),
            name: self.listing.name(entry).to_owned(),
        })
    }

    /// The selection by name, before the rows change.
    pub(super) fn keep_selection(&self) -> Kept {
        let folders = self.rows_have_folders();
        Kept {
            entries: self
                .selection
                .rows()
                .filter_map(|row| self.kept_at(row, folders))
                .collect(),
            cursor: self
                .selection
                .cursor()
                .and_then(|row| self.kept_at(row, folders)),
        }
    }

    /// Selects the kept entries again at their new rows; gone ones drop out.
    /// An entry kept with its folder is found by both where the rows still
    /// come from several folders, and by name otherwise.
    pub(super) fn restore_selection(&mut self, kept: &Kept) {
        if kept.entries.is_empty() && kept.cursor.is_none() {
            self.selection.clear();
            return;
        }
        let by_name: HashMap<&str, usize> = (0..self.order.len())
            .filter_map(|row| Some((self.name_at(row)?, row)))
            .collect();
        let by_folder: HashMap<(&str, &str), usize> = if self.rows_have_folders() {
            self.order
                .iter()
                .enumerate()
                .map(|(row, &entry)| {
                    let entry = entry as usize;
                    let dir: &str = self.listing.dir(entry);
                    ((dir, self.listing.name(entry)), row)
                })
                .collect()
        } else {
            HashMap::new()
        };
        let row_of = |entry: &KeptEntry| match &entry.dir {
            Some(dir) if !by_folder.is_empty() => {
                by_folder.get(&(dir.as_ref(), entry.name.as_str())).copied()
            }
            _ => by_name.get(entry.name.as_str()).copied(),
        };
        let selected: Vec<usize> = kept.entries.iter().filter_map(row_of).collect();
        let cursor = kept.cursor.as_ref().and_then(row_of);
        let mut selection = Selection::default();
        if let Some(cursor) = cursor {
            selection.set_cursor(cursor);
        }
        selection.set_rows(selected);
        *self.selection = selection;
    }

    /// What the selection's derivations depend on.
    fn selection_stamp(&self) -> Stamp {
        (
            self.selection.revision,
            Arc::as_ptr(&self.listing) as usize,
            self.order.len(),
        )
    }

    /// The selected rows as ranges: one range however many rows Select All picks.
    pub(super) fn selected_ranges(&self) -> Vec<RowRange> {
        let revision = self.selection.revision;
        let mut memo = self.selection.memo();
        if let Some((at, ranges)) = &memo.ranges
            && *at == revision
        {
            return ranges.clone();
        }
        let mut ranges: Vec<RowRange> = Vec::new();
        for row in self.selection.rows() {
            let row = row as u32;
            match ranges.last_mut() {
                Some(range) if range.end == row => range.end = row + 1,
                _ => ranges.push(RowRange {
                    start: row,
                    end: row + 1,
                }),
            }
        }
        memo.ranges = Some((revision, ranges.clone()));
        ranges
    }

    /// The selection (or the cursor's row) as plugins see it, made once
    /// per selection.
    pub(super) fn plugin_targets(&self) -> Arc<Vec<TargetInfo>> {
        let stamp = self.selection_stamp();
        if let Some((at, targets)) = &self.selection.memo().targets
            && *at == stamp
        {
            return Arc::clone(targets);
        }
        let targets = Arc::new(self.plugin_targets_now());
        self.selection.memo().targets = Some((stamp, Arc::clone(&targets)));
        targets
    }

    /// A number that changes when the items plugins get do.
    pub(super) fn plugin_targets_key(&self) -> u64 {
        let stamp = self.selection_stamp();
        if let Some((at, key)) = self.selection.memo().targets_key
            && at == stamp
        {
            return key;
        }
        let key = super::plugin_views::targets_key(&self.plugin_targets());
        self.selection.memo().targets_key = Some((stamp, key));
        key
    }

    /// The inspector's summary of several selected items, made once per
    /// selection.
    pub(super) fn selection_summary(&self) -> SelectionSummary {
        let stamp = self.selection_stamp();
        if let Some((at, summary)) = &self.selection.memo().summary
            && *at == stamp
        {
            return summary.clone();
        }
        let summary = self.selection_summary_now();
        self.selection.memo().summary = Some((stamp, summary.clone()));
        summary
    }

    pub(super) fn click(&mut self, row: u32, command: bool, shift: bool) {
        if (row as usize) < self.order.len() {
            self.selection
                .click(row as usize, SelectMods { command, shift });
        }
    }

    pub(super) fn move_cursor(&mut self, row: u32, command: bool, shift: bool) {
        if (row as usize) < self.order.len() {
            self.selection
                .move_cursor(row as usize, SelectMods { command, shift });
        }
    }

    /// Replaces the selection with `rows`, as a shell's own list reports it.
    pub(super) fn set_selection(&mut self, rows: &[u32]) {
        let len = self.order.len();
        self.selection.set_rows(
            rows.iter()
                .map(|&row| row as usize)
                .filter(|&row| row < len),
        );
    }

    /// Paths of the selected entries in display order, and the cursor's
    /// place among them. With one or no item selected, the rows around the
    /// cursor, so Quick Look's arrows step through the folder.
    pub(super) fn selected_paths(&self) -> (Vec<String>, usize) {
        if self.path.is_none() && !self.trash {
            return (Vec::new(), 0);
        }
        let path = |row: usize| {
            self.row_path(row)
                .map(|path| path.to_string_lossy().into_owned())
        };
        let single = self.selection.len() <= 1;
        let rows: Vec<usize> = self.selection.cursor().filter(|_| single).map_or_else(
            || self.selection.rows().collect(),
            |cursor| {
                let start = cursor.saturating_sub(QUICK_LOOK_AROUND);
                let end = (cursor + QUICK_LOOK_AROUND + 1).min(self.order.len());
                (start..end).collect()
            },
        );
        let index = self
            .selection
            .cursor()
            .and_then(|cursor| rows.iter().position(|&row| row == cursor))
            .unwrap_or(0);
        (rows.into_iter().filter_map(path).collect(), index)
    }
}
