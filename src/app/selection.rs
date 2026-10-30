//! Per-tab selection model shared by the list and grid views.
//!
//! Rows are positions in `TabData::visible_entries`. The selection keeps an
//! *anchor* (where a Shift range starts) and a *cursor* (the focused row that
//! keyboard navigation moves), following the usual desktop conventions:
//! click selects one row, Ctrl+click toggles, Shift+click selects a range
//! from the anchor and Ctrl+Shift+click adds that range.

use fixedbitset::FixedBitSet;

/// Modifier state relevant to selection gestures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SelectMods {
    /// Ctrl on Windows/Linux, Cmd on macOS.
    pub command: bool,
    pub shift: bool,
}

impl SelectMods {
    pub const fn from_modifiers(modifiers: egui::Modifiers) -> Self {
        Self {
            command: modifiers.command,
            shift: modifiers.shift,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Selection {
    rows: FixedBitSet,
    anchor: Option<usize>,
    cursor: Option<usize>,
    /// Set when the cursor moved and the view should scroll it into view.
    pub just_changed: bool,
}

impl Selection {
    pub fn contains(&self, row: usize) -> bool {
        self.rows.contains(row)
    }

    pub fn len(&self) -> usize {
        self.rows.count_ones(..)
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_clear()
    }

    /// Selected rows in ascending order.
    pub fn rows(&self) -> impl Iterator<Item = usize> + '_ {
        self.rows.ones()
    }

    pub const fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    fn insert(&mut self, row: usize) {
        if row >= self.rows.len() {
            self.rows.grow(row + 1);
        }
        self.rows.insert(row);
    }

    fn insert_range(&mut self, a: usize, b: usize) {
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        if end >= self.rows.len() {
            self.rows.grow(end + 1);
        }
        self.rows.insert_range(start..end + 1);
    }

    pub fn clear(&mut self) {
        if !self.is_empty() || self.anchor.is_some() || self.cursor.is_some() {
            self.rows.clear();
            self.anchor = None;
            self.cursor = None;
        }
    }

    /// Clears the selected rows but keeps the cursor where it is, so keyboard
    /// navigation continues from the same place.
    pub fn deselect_all(&mut self) {
        self.rows.clear();
    }

    /// Replaces the selection with a single row.
    pub fn select_only(&mut self, row: usize) {
        self.rows.clear();
        self.insert(row);
        self.anchor = Some(row);
        self.cursor = Some(row);
        self.just_changed = true;
    }

    pub fn toggle(&mut self, row: usize) {
        if self.contains(row) {
            self.rows.set(row, false);
        } else {
            self.insert(row);
        }
        self.anchor = Some(row);
        self.cursor = Some(row);
        self.just_changed = true;
    }

    /// Selects the range between the anchor and `row`. With `additive` the
    /// existing selection is kept (Ctrl+Shift); otherwise it is replaced.
    pub fn select_range(&mut self, row: usize, additive: bool) {
        let anchor = self.anchor.unwrap_or(row);
        if !additive {
            self.rows.clear();
        }
        self.insert_range(anchor, row);
        self.anchor = Some(anchor);
        self.cursor = Some(row);
        self.just_changed = true;
    }

    /// Applies a mouse click on `row` with the given modifiers.
    pub fn click(&mut self, row: usize, mods: SelectMods) {
        match (mods.command, mods.shift) {
            (false, false) => self.select_only(row),
            (true, false) => self.toggle(row),
            (additive, true) => self.select_range(row, additive),
        }
    }

    /// Moves the keyboard cursor to `row`. Shift extends the range from the
    /// anchor, Ctrl only moves the cursor, plain movement selects the row.
    pub fn move_cursor(&mut self, row: usize, mods: SelectMods) {
        match (mods.command, mods.shift) {
            (_, true) => self.select_range(row, mods.command),
            (true, false) => {
                self.cursor = Some(row);
                self.just_changed = true;
            }
            (false, false) => self.select_only(row),
        }
    }

    /// Moves the cursor without changing the selection or scrolling.
    pub const fn set_cursor(&mut self, row: usize) {
        self.cursor = Some(row);
    }

    /// Toggles the row under the cursor (Ctrl+Space).
    pub fn toggle_cursor(&mut self) {
        if let Some(row) = self.cursor {
            self.toggle(row);
        }
    }

    pub fn select_all(&mut self, len: usize) {
        if len == 0 {
            return;
        }
        self.rows.clear();
        self.insert_range(0, len - 1);
        if self.cursor.is_none_or(|row| row >= len) {
            self.cursor = Some(0);
        }
        self.anchor = self.cursor;
    }

    pub fn invert(&mut self, len: usize) {
        self.rows.grow(len);
        self.rows.toggle_range(..len);
        if self.rows.len() > len {
            self.rows.remove_range(len..);
        }
    }

    /// Replaces the selected rows, e.g. after the rows were re-sorted. Keeps
    /// the cursor and anchor only when they are still selected; otherwise
    /// they move to the first selected row.
    pub fn set_rows(&mut self, rows: impl IntoIterator<Item = usize>) {
        self.rows.clear();
        for row in rows {
            self.insert(row);
        }
        if self.cursor.is_none_or(|row| !self.contains(row)) {
            self.cursor = self.rows.ones().next();
        }
        if self.anchor.is_none_or(|row| !self.contains(row)) {
            self.anchor = self.cursor;
        }
    }

    /// Adds rows to the selection without moving the anchor or cursor
    /// (drag selection).
    pub fn add_rows(&mut self, rows: impl IntoIterator<Item = usize>) {
        for row in rows {
            self.insert(row);
        }
    }

    /// Drops rows that no longer exist after the view shrank.
    pub fn truncate(&mut self, len: usize) {
        if self.rows.len() > len {
            self.rows.remove_range(len..);
        }
        if self.cursor.is_some_and(|row| row >= len) {
            self.cursor = len.checked_sub(1);
        }
        if self.anchor.is_some_and(|row| row >= len) {
            self.anchor = self.cursor;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SelectMods, Selection};

    const PLAIN: SelectMods = SelectMods {
        command: false,
        shift: false,
    };
    const CTRL: SelectMods = SelectMods {
        command: true,
        shift: false,
    };
    const SHIFT: SelectMods = SelectMods {
        command: false,
        shift: true,
    };
    const CTRL_SHIFT: SelectMods = SelectMods {
        command: true,
        shift: true,
    };

    fn rows(selection: &Selection) -> Vec<usize> {
        selection.rows().collect()
    }

    #[test]
    fn click_selects_one_and_ctrl_click_toggles() {
        let mut selection = Selection::default();
        selection.click(3, PLAIN);
        selection.click(5, PLAIN);
        assert_eq!(rows(&selection), vec![5]);
        selection.click(2, CTRL);
        selection.click(2, CTRL);
        selection.click(7, CTRL);
        assert_eq!(rows(&selection), vec![5, 7]);
    }

    #[test]
    fn shift_click_selects_range_from_anchor_in_both_directions() {
        let mut selection = Selection::default();
        selection.click(4, PLAIN);
        selection.click(7, SHIFT);
        assert_eq!(rows(&selection), vec![4, 5, 6, 7]);
        selection.click(2, SHIFT);
        assert_eq!(rows(&selection), vec![2, 3, 4]);
        assert_eq!(selection.anchor, Some(4));
        assert_eq!(selection.cursor(), Some(2));
    }

    #[test]
    fn ctrl_shift_click_adds_a_range() {
        let mut selection = Selection::default();
        selection.click(0, PLAIN);
        selection.click(5, CTRL);
        selection.click(7, CTRL_SHIFT);
        assert_eq!(rows(&selection), vec![0, 5, 6, 7]);
    }

    #[test]
    fn keyboard_movement_follows_modifiers() {
        let mut selection = Selection::default();
        selection.move_cursor(1, PLAIN);
        selection.move_cursor(3, SHIFT);
        assert_eq!(rows(&selection), vec![1, 2, 3]);
        selection.move_cursor(5, CTRL);
        assert_eq!(rows(&selection), vec![1, 2, 3]);
        assert_eq!(selection.cursor(), Some(5));
        selection.toggle_cursor();
        assert_eq!(rows(&selection), vec![1, 2, 3, 5]);
    }

    #[test]
    fn select_all_invert_and_truncate() {
        let mut selection = Selection::default();
        selection.click(1, PLAIN);
        selection.invert(4);
        assert_eq!(rows(&selection), vec![0, 2, 3]);
        selection.select_all(5);
        assert_eq!(selection.len(), 5);
        selection.truncate(2);
        assert_eq!(rows(&selection), vec![0, 1]);
        selection.clear();
        assert!(selection.is_empty());
        assert_eq!(selection.cursor(), None);
    }

    #[test]
    fn set_rows_keeps_cursor_only_when_still_selected() {
        let mut selection = Selection::default();
        selection.click(2, PLAIN);
        selection.set_rows([6, 8]);
        assert_eq!(selection.cursor(), Some(6));
    }
}
