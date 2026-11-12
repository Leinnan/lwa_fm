//! What two panes do together, and selecting rows by what they are called.
//!
//! A window's "other pane" is the one after its focused pane, the first
//! again after the last. The selection is copied or moved into the folder
//! it shows, the two panes change places, or it is sent to the folder the
//! focused pane shows. Rows are selected by a pattern written as a search
//! is, or by sharing their extension with a selected row.
use std::collections::{BTreeSet, HashSet};

use crux_core::Command;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::{DirFleet, Effect, Event, Model, Tab, Window, tab_id};
use crate::ids::TabId;
use crate::listing::EntryType;
use crate::search::filter::{CompiledQuery, pattern_error};
use crate::search::{Chip, Search, query};

/// The pane a window's two-pane commands act with.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct OtherPane {
    /// The tab it shows.
    pub tab: u32,
    pub title: String,
    /// The folder it shows; "" when it shows none.
    pub path: String,
    /// Files can be copied or moved into it.
    pub writable: bool,
    /// Why they can't, when they can't.
    pub reason: String,
}

/// The Select by Pattern dialog of a tab.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct SelectPatternView {
    pub text: String,
    /// How many of the tab's rows the pattern matches.
    pub matches: u32,
    /// What is wrong with the pattern, when it can't be read.
    pub error: String,
}

impl Window {
    /// The tab the pane after the focused one shows; `None` in a window of
    /// one pane.
    pub(super) fn other_tab(&self) -> Option<TabId> {
        let shown = self.layout.shown();
        if shown.len() < 2 {
            return None;
        }
        let at = shown.iter().position(|&tab| tab == self.focused)?;
        shown.get((at + 1) % shown.len()).copied()
    }
}

impl Model {
    /// The pane `window`'s two-pane commands act with.
    pub(super) fn other_pane(&self, window: &Window) -> Option<OtherPane> {
        let id = window.other_tab()?;
        let tab = self.tabs.get(&id)?;
        let reason = if tab.trash {
            "Things get into the Recycle Bin only by being deleted"
        } else if tab.results.is_some() {
            "A list of results is no folder to put things in"
        } else {
            tab.location()
                .as_ref()
                .and_then(super::archives::why_unchangeable)
                .unwrap_or_default()
        };
        let path = tab.path.clone().unwrap_or_default();
        Some(OtherPane {
            tab: id.number(),
            title: self.remote.top_title(&path).unwrap_or_else(|| tab.title()),
            writable: reason.is_empty() && !path.is_empty(),
            reason: reason.to_owned(),
            path,
        })
    }
}

/// What a name ends with after its last dot, in small letters; "" when it
/// has no such ending.
fn extension(name: &str) -> String {
    name.rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .map_or_else(String::new, |(_, extension)| extension.to_lowercase())
}

impl Tab {
    /// The rows `pattern` matches, written as a search is: `*.jpg`,
    /// `ext:png`, `kind:image size:>1mb`, a word of the name.
    fn rows_matching(&self, pattern: &str, now: u64, tags: &super::tags::Tags) -> Vec<usize> {
        let search = Search {
            value: pattern.to_owned(),
            depth: 1,
            ..Search::default()
        };
        let Some(query) = CompiledQuery::new(Some(&search), now) else {
            return Vec::new();
        };
        let folded = query.needs_folded.then(|| self.listing.folded_names());
        let opened: Vec<u32> = if query.needs_opened {
            self.opened.iter().map(|&(_, last)| last).collect()
        } else {
            Vec::new()
        };
        self.order
            .iter()
            .enumerate()
            .filter(|&(_, &entry)| query.matches(&self.listing, folded, &opened, entry as usize))
            .map(|(row, _)| row)
            // `tag:name` is the tags' to answer.
            .filter(|&row| {
                query.wanted_tags().is_empty()
                    || self.row_op_path(row).is_some_and(|path| {
                        let path = path.to_string_lossy();
                        query
                            .wanted_tags()
                            .iter()
                            .all(|name| tags.has_named(&path, name))
                    })
            })
            .collect()
    }

    pub(super) fn select_pattern_view(
        &self,
        now: u64,
        tags: &super::tags::Tags,
    ) -> Option<SelectPatternView> {
        let text = self.select_pattern.as_ref()?;
        let error = query::parse(text)
            .chips
            .iter()
            .find_map(|chip| match chip {
                Chip::Name { pattern, kind, .. } => pattern_error(pattern, *kind),
                _ => None,
            })
            .unwrap_or_default();
        Some(SelectPatternView {
            matches: u32::try_from(self.rows_matching(text, now, tags).len()).unwrap_or(u32::MAX),
            text: text.clone(),
            error,
        })
    }

    /// Adds the rows `pattern` matches to the selection, or takes them out
    /// of it.
    fn select_matching(&mut self, pattern: &str, select: bool, now: u64, tags: &super::tags::Tags) {
        let matching = self.rows_matching(pattern, now, tags);
        let mut rows: BTreeSet<usize> = self.selection.rows().collect();
        for row in matching {
            if select {
                rows.insert(row);
            } else {
                rows.remove(&row);
            }
        }
        self.selection.set_rows(rows);
    }

    /// Selects every row that ends as one of the selected rows does (the
    /// cursor's, when nothing is selected): files by their extension,
    /// folders with folders. Whether anything was there to go by.
    fn select_same_extension(&mut self) -> bool {
        let kind = |row: usize| {
            let entry = *self.order.get(row)? as usize;
            let folder = self.listing.meta(entry).entry_type == EntryType::Directory;
            Some((
                folder,
                if folder {
                    String::new()
                } else {
                    extension(self.listing.name(entry))
                },
            ))
        };
        let wanted: HashSet<(bool, String)> = if self.selection.is_empty() {
            self.selection.cursor().and_then(kind).into_iter().collect()
        } else {
            self.selection.rows().filter_map(kind).collect()
        };
        if wanted.is_empty() {
            return false;
        }
        let rows: Vec<usize> = (0..self.order.len())
            .filter(|&row| kind(row).is_some_and(|kind| wanted.contains(&kind)))
            .collect();
        self.selection.set_rows(rows);
        true
    }
}

impl DirFleet {
    /// The events of two-pane commands and of selecting by pattern.
    pub(super) fn pane_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        match event {
            Event::CopyToOtherPane { window } => Self::to_other_pane(model, window, true),
            Event::MoveToOtherPane { window } => Self::to_other_pane(model, window, false),
            Event::SwapPanes { window } => {
                let Some(index) = model.window_index(window) else {
                    return Command::done();
                };
                let window = &mut model.windows[index];
                let Some(other) = window.other_tab() else {
                    return Self::notify(model, "This window has one pane".to_owned());
                };
                // The focus stays with its tab, now on the other side.
                window.layout.swap_panes(window.focused, other);
                render()
            }
            Event::MirrorToOtherPane { window } => {
                let Some(index) = model.window_index(window) else {
                    return Command::done();
                };
                let window = &model.windows[index];
                let Some(other) = window.other_tab() else {
                    return Self::notify(model, "This window has one pane".to_owned());
                };
                let shown = |tab: TabId| model.tabs.get(&tab).and_then(|tab| tab.path.clone());
                let Some(path) = shown(window.focused) else {
                    return Command::done();
                };
                if shown(other).as_ref() == Some(&path) {
                    return Command::done();
                }
                Self::navigate(model, other.number(), path)
            }
            Event::SelectSameExtension { tab } => {
                let Some(state) = tab_id(tab).and_then(|id| model.tabs.get_mut(&id)) else {
                    return Command::done();
                };
                if !state.select_same_extension() {
                    return Command::done();
                }
                render().and(Self::selection_moved(model, tab))
            }
            Event::OpenSelectPattern { tab } => {
                let last = model.last_pattern.clone();
                if let Some(state) = tab_id(tab).and_then(|id| model.tabs.get_mut(&id)) {
                    state.select_pattern = Some(last);
                }
                render()
            }
            Event::SetSelectPattern { tab, text } => {
                let Some(pattern) = tab_id(tab)
                    .and_then(|id| model.tabs.get_mut(&id))
                    .and_then(|state| state.select_pattern.as_mut())
                else {
                    return Command::done();
                };
                *pattern = text;
                render()
            }
            Event::ApplySelectPattern { tab, select } => {
                let now = model.now;
                let Model { tabs, tags, .. } = model;
                let Some(state) = tab_id(tab).and_then(|id| tabs.get_mut(&id)) else {
                    return Command::done();
                };
                let Some(pattern) = state.select_pattern.take() else {
                    return Command::done();
                };
                state.select_matching(&pattern, select, now, tags);
                // Offered again the next time, in whichever tab.
                model.last_pattern = pattern;
                render().and(Self::selection_moved(model, tab))
            }
            Event::CloseSelectPattern { tab } => {
                if let Some(state) = tab_id(tab).and_then(|id| model.tabs.get_mut(&id)) {
                    state.select_pattern = None;
                }
                render()
            }
            _ => Command::done(),
        }
    }

    /// Copies or moves the focused tab's selection into the folder the
    /// window's other pane shows, as Copy To and Move To do.
    fn to_other_pane(model: &mut Model, window: u32, copy: bool) -> Command<Effect, Event> {
        let Some(index) = model.window_index(window) else {
            return Command::done();
        };
        let window = &model.windows[index];
        let from = window.focused.number();
        let Some(other) = model.other_pane(window) else {
            let what = if copy { "copy" } else { "move" };
            return Self::notify(model, format!("There is no other pane to {what} to"));
        };
        if !other.writable {
            return Self::notify(model, other.reason);
        }
        Self::send_selection(model, from, other.path, copy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_ends_with_what_follows_its_last_dot() {
        assert_eq!(extension("Photo.JPG"), "jpg");
        assert_eq!(extension("archive.tar.gz"), "gz");
        assert_eq!(extension("README"), "");
        // A name that starts with its only dot has no ending.
        assert_eq!(extension(".gitignore"), "");
    }
}
