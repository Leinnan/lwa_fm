//! The view model, split into parts that keep their identity while they stay
//! the same, so only what changed is drawn again.
//!
//! A window's rows, selection and cursor change far more often than the rest
//! of it, so they are taken out of the window ("chrome") into [`TabRows`].
//! Every part sits behind an `Rc`; a part equal to the one before keeps the
//! old `Rc`, and components compare their inputs by those.
use std::path::{Path, PathBuf};
use std::rc::Rc;

use dirfleet_core::app::{
    FilterGroup, GroupView, RenameView, RowRange, RowView, TabTitle, TabViewModel, ViewModel,
    WindowView,
};

use crate::files::ColumnSet;

use crate::shortcuts::Shortcuts;

/// The rows of the tab a window shows, near what is in sight.
#[derive(Debug, PartialEq, Eq, Default)]
pub struct TabRows {
    pub tab: u32,
    /// The folder shown; with `in_trash`, what makes the list start at its top again.
    pub path: String,
    pub in_trash: bool,
    /// The tab shows what a scan found: it has no folder, and each row names
    /// the whole of its own.
    pub found: bool,
    /// Every row of the folder, in sight or not.
    pub total_rows: u32,
    /// The position of `rows[0]`.
    pub first_row: u32,
    pub rows: Vec<Rc<RowView>>,
    /// The groups the rows are in, each with its heading; none when the
    /// folder is not grouped.
    pub groups: Vec<GroupView>,
    pub selection: Vec<RowRange>,
    pub cursor: Option<u32>,
    pub scroll_to: Option<u32>,
    pub scroll_count: u32,
    /// The row being renamed in place.
    pub renaming: Option<Rc<RenameView>>,
}

/// Whether a tab's path is a folder on this machine, whose entries other
/// programs can be handed, and not a place inside an archive.
pub fn is_on_disk(path: &str) -> bool {
    dirfleet_core::Location::parse(path).as_local().is_some()
}

impl TabRows {
    /// The row at `index` of the folder, when the core sent it.
    pub fn row(&self, index: u32) -> Option<&Rc<RowView>> {
        index
            .checked_sub(self.first_row)
            .and_then(|offset| self.rows.get(offset as usize))
    }

    /// Where the entry `row` is on disk. `None` for what has no path of its
    /// own to act on: an item in the Recycle Bin, a tab without a folder, an
    /// entry inside an archive. What a scan found is in the folder its row
    /// names in full.
    pub fn path_of(&self, row: &RowView) -> Option<PathBuf> {
        (!self.in_trash && (self.found || !self.path.is_empty()) && is_on_disk(&self.path))
            .then(|| Path::new(&self.path).join(&row.folder).join(&row.name))
    }

    /// What dragging the entry at `index` takes along: every selected entry
    /// the core has sent when it is one of them, and otherwise it alone.
    pub fn dragged_from(&self, index: u32) -> Vec<PathBuf> {
        let Some(pressed) = self.row(index) else {
            return Vec::new();
        };
        if pressed.selected {
            self.rows
                .iter()
                .filter(|row| row.selected)
                .filter_map(|row| self.path_of(row))
                .collect()
        } else {
            self.path_of(pressed).into_iter().collect()
        }
    }

    /// Takes the fast-changing parts out of `tab`, keeping rows equal to `old`'s.
    fn take(tab: &mut TabViewModel, old: Option<&Self>) -> Self {
        let old = old.filter(|old| old.tab == tab.id);
        let first_row = std::mem::take(&mut tab.first_row);
        let rows = std::mem::take(&mut tab.rows)
            .into_iter()
            .map(|row| match old.and_then(|old| old.row(row.index)) {
                Some(kept) if **kept == row => Rc::clone(kept),
                _ => Rc::new(row),
            })
            .collect();
        Self {
            tab: tab.id,
            path: tab.path.clone(),
            in_trash: tab.in_trash,
            found: tab.results.is_some(),
            total_rows: tab.total_rows,
            first_row,
            rows,
            groups: std::mem::take(&mut tab.groups),
            selection: std::mem::take(&mut tab.selection),
            cursor: tab.cursor.take(),
            scroll_to: tab.scroll_to.take(),
            scroll_count: tab.scroll_count,
            renaming: tab.renaming.take().map(|rename| {
                match old.and_then(|old| old.renaming.as_ref()) {
                    Some(kept) if **kept == rename => Rc::clone(kept),
                    _ => Rc::new(rename),
                }
            }),
        }
    }
}

/// One pane of a window: its tabs, and the one it shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneParts {
    pub tabs: Rc<Vec<TabTitle>>,
    /// The tab shown, without its rows, selection and cursor.
    pub tab: Rc<TabViewModel>,
    pub rows: Rc<TabRows>,
}

/// One window: what rarely changes, and its panes.
#[derive(Clone, Debug)]
pub struct WindowParts {
    /// The window without its panes; its focused tab without rows.
    pub chrome: Rc<WindowView>,
    pub panes: Vec<PaneParts>,
}

impl WindowParts {
    pub fn id(&self) -> u32 {
        self.chrome.id
    }
}

/// The parts of the last view model taken from the core.
#[derive(Default)]
pub struct Store {
    pub windows: Vec<WindowParts>,
    pub columns: Rc<ColumnSet>,
    /// Everything that belongs to no window: settings, commands, toasts, dialogs.
    pub shared: Rc<ViewModel>,
    pub shortcuts: Rc<Shortcuts>,
    /// The preset filters the search menu offers.
    pub filters: Rc<Vec<FilterGroup>>,
}

impl Store {
    /// Replaces the parts `view` changed, keeping the rest as they are.
    pub fn update(&mut self, mut view: ViewModel) {
        let columns = ColumnSet {
            shown: std::mem::take(&mut view.columns),
            optional: std::mem::take(&mut view.optional_columns),
        };
        if *self.columns != columns {
            self.columns = Rc::new(columns);
        }
        let old = std::mem::take(&mut self.windows);
        self.windows = std::mem::take(&mut view.windows)
            .into_iter()
            .map(|mut window| {
                let before = old.iter().find(|parts| parts.id() == window.id);
                // The focused tab's rows are in its pane too; one copy is enough.
                TabRows::take(&mut window.active, None);
                let panes = std::mem::take(&mut window.panes)
                    .into_iter()
                    .map(|mut pane| {
                        let was = before.and_then(|parts| {
                            parts.panes.iter().find(|was| was.tab.id == pane.shown.id)
                        });
                        let rows = TabRows::take(&mut pane.shown, was.map(|was| &*was.rows));
                        PaneParts {
                            tabs: keep(was.map(|was| &was.tabs), pane.tabs),
                            tab: keep(was.map(|was| &was.tab), pane.shown),
                            rows: keep(was.map(|was| &was.rows), rows),
                        }
                    })
                    .collect();
                WindowParts {
                    chrome: keep(before.map(|parts| &parts.chrome), window),
                    panes,
                }
            })
            .collect();
        let filters = std::mem::take(&mut view.search_filters);
        if *self.filters != filters {
            self.filters = Rc::new(filters);
        }
        let shortcuts = Shortcuts::new(&view.commands, &view.plugin_shortcuts);
        if *self.shortcuts != shortcuts {
            self.shortcuts = Rc::new(shortcuts);
        }
        if *self.shared != view {
            self.shared = Rc::new(view);
        }
    }

    pub fn window(&self, id: u32) -> Option<&WindowParts> {
        self.windows.iter().find(|parts| parts.id() == id)
    }
}

/// `old` when it equals `new`, so whoever holds it sees no change.
fn keep<T: PartialEq>(old: Option<&Rc<T>>, new: T) -> Rc<T> {
    match old {
        Some(old) if **old == new => Rc::clone(old),
        _ => Rc::new(new),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(index: u32, name: &str) -> RowView {
        RowView {
            index,
            name: name.to_owned(),
            is_dir: false,
            cells: Vec::new(),
            selected: false,
            highlights: Vec::new(),
            cut: false,
            thumbnail_choice: dirfleet_core::app::ThumbnailChoice::None,
            folder: String::new(),
            thumbnail: String::new(),
            animation: String::new(),
            tooltip: String::new(),
            detail: String::new(),
            matched: String::new(),
            matched_line: 0,
            matched_highlights: Vec::new(),
            tags: Vec::new(),
        }
    }

    fn tab(first_row: u32, names: &[&str]) -> TabViewModel {
        TabViewModel {
            id: 7,
            first_row,
            total_rows: 100,
            rows: names
                .iter()
                .zip(first_row..)
                .map(|(name, index)| row(index, name))
                .collect(),
            ..TabViewModel::default()
        }
    }

    #[test]
    fn rows_that_stay_the_same_keep_their_identity() {
        let first = TabRows::take(&mut tab(10, &["a", "b", "c"]), None);
        // The window moved one row down and "c" was renamed.
        let second = TabRows::take(&mut tab(11, &["b", "see", "d"]), Some(&first));
        assert!(Rc::ptr_eq(&first.rows[1], &second.rows[0]));
        assert!(!Rc::ptr_eq(&first.rows[2], &second.rows[1]));
        assert_eq!(second.row(12).map(|row| row.name.as_str()), Some("see"));
        assert!(second.row(10).is_none());
    }

    #[test]
    fn taking_the_rows_leaves_the_tab_without_them() {
        let mut tab = tab(3, &["a"]);
        tab.cursor = Some(3);
        let rows = TabRows::take(&mut tab, None);
        assert_eq!(rows.cursor, Some(3));
        assert!(tab.rows.is_empty() && tab.cursor.is_none() && tab.first_row == 0);
    }

    #[test]
    fn another_tabs_rows_are_not_reused() {
        let first = TabRows::take(&mut tab(0, &["a"]), None);
        let mut other = tab(0, &["a"]);
        other.id = 8;
        let second = TabRows::take(&mut other, Some(&first));
        assert!(!Rc::ptr_eq(&first.rows[0], &second.rows[0]));
    }
}
