//! Where entries sit in the list or the icon grid: how many share a line, how
//! tall a line is, which entries a scroll position shows, and where a group's
//! heading stands.
//!
//! Every line is as tall as the next, so a line's place is its number times
//! that height, whatever the view has built so far. A group's heading takes
//! one slot at the group's start: a line of its own in the list, the first
//! cell of the group's first line in the grid.
use std::hash::{Hash, Hasher};

use dirfleet_core::app::IconSize;
use dirfleet_core::view_settings::DisplayType;

/// The height of a list row, in device-independent pixels.
pub const ROW_HEIGHT: f64 = 28.0;

/// Entries `first..=last` of the folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub first: u32,
    pub last: u32,
}

impl Span {
    pub const fn len(self) -> u32 {
        self.last - self.first + 1
    }
}

/// A tile of the icon grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tile {
    /// The edge of the icon or thumbnail.
    pub icon: f64,
    /// The tile itself: what is marked when it is selected.
    pub width: f64,
    /// The share of the line the tile stands in the middle of.
    pub cell: f64,
    pub height: f64,
    /// Whether a second line (size, or the folder of a search result) fits.
    pub detail: bool,
}

impl Tile {
    pub const fn of(size: IconSize) -> Self {
        let (icon, detail) = match size {
            IconSize::Small => (40.0, false),
            IconSize::Medium => (56.0, false),
            IconSize::Large => (80.0, true),
            IconSize::ExtraLarge => (112.0, true),
        };
        Self {
            icon,
            width: icon + 48.0,
            cell: icon + 48.0,
            // The icon, two lines of name, and the detail line when there is one.
            height: icon + if detail { 68.0 } else { 52.0 },
            detail,
        }
    }
}

/// How the entries are laid out: lines of `per_line` slots, each `line_height` tall.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub line_height: f64,
    pub per_line: u32,
    /// The tile, in the icon grid.
    pub tile: Option<Tile>,
}

impl Metrics {
    /// The layout of `display` in a view `width` wide.
    pub fn of(display: DisplayType, icon_size: IconSize, width: f64) -> Self {
        match display {
            DisplayType::List => Self {
                line_height: ROW_HEIGHT,
                per_line: 1,
                tile: None,
            },
            DisplayType::Icons => {
                let tile = Tile::of(icon_size);
                let per_line = ((width / tile.width).floor() as u32).max(1);
                Self {
                    line_height: tile.height,
                    per_line,
                    // The tiles of a line share its whole width.
                    tile: Some(Tile {
                        cell: (width / f64::from(per_line)).max(tile.width),
                        ..tile
                    }),
                }
            }
        }
    }

    /// How many lines a page key moves by: those that fit, less one.
    pub fn page(&self, height: f64) -> u32 {
        ((height / self.line_height).floor() as u32).max(2) - 1
    }
}

/// Entries `first..first + count` of the folder, under a heading of their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Group {
    pub first: u32,
    pub count: u32,
}

/// A run of entries and the lines it takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Section {
    first: u32,
    count: u32,
    /// The slots its heading takes before its entries: 1, or 0 for entries
    /// that are in no group.
    lead: u32,
    first_line: u32,
}

/// What one line holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line {
    /// The group whose heading starts the line.
    pub heading: Option<u32>,
    /// The line's first entry; with no entry on the line, the one after it.
    pub first: u32,
    /// How many entries are on the line.
    pub count: u32,
}

/// The lines `total` entries take, in groups or not.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub metrics: Metrics,
    total: u32,
    sections: Vec<Section>,
    lines: u32,
}

/// A move of the cursor from the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
}

impl Layout {
    /// The lines of `total` entries. `groups` must hold them all, one after
    /// another, or the entries are laid out as if in no group.
    pub fn new(metrics: Metrics, total: u32, groups: &[Group]) -> Self {
        let mut covered = 0;
        let whole = !groups.is_empty()
            && groups.iter().all(|group| {
                let follows = group.first == covered && group.count > 0;
                covered = covered.saturating_add(group.count);
                follows
            })
            && covered == total;
        let plain = [Group {
            first: 0,
            count: total,
        }];
        let (groups, lead) = if whole { (groups, 1) } else { (&plain[..], 0) };
        let mut lines = 0;
        let sections = groups
            .iter()
            .filter(|group| group.count > 0)
            .map(|group| {
                let section = Section {
                    first: group.first,
                    count: group.count,
                    lead,
                    first_line: lines,
                };
                lines += (lead + group.count).div_ceil(metrics.per_line);
                section
            })
            .collect();
        Self {
            metrics,
            total,
            sections,
            lines,
        }
    }

    /// What tells this arrangement of lines from another of the same length.
    pub fn revision(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.metrics.per_line.hash(&mut hasher);
        self.sections.hash(&mut hasher);
        hasher.finish()
    }

    /// How many lines there are.
    pub const fn lines(&self) -> u32 {
        self.lines
    }

    /// The section `entry` is in, and its place among them.
    fn section_of(&self, entry: u32) -> Option<&Section> {
        let after = self
            .sections
            .partition_point(|section| section.first <= entry);
        self.sections.get(after.checked_sub(1)?)
    }

    /// The line and the column `entry` is at.
    fn place(&self, entry: u32) -> (u32, u32) {
        self.section_of(entry).map_or((0, 0), |section| {
            let slot = section.lead + (entry - section.first);
            (
                section.first_line + slot / self.metrics.per_line,
                slot % self.metrics.per_line,
            )
        })
    }

    /// The line `entry` is on.
    pub fn line_of(&self, entry: u32) -> u32 {
        self.place(entry).0
    }

    /// What the line at `line` holds; `None` past the last line.
    pub fn line(&self, line: u32) -> Option<Line> {
        if line >= self.lines {
            return None;
        }
        let after = self
            .sections
            .partition_point(|section| section.first_line <= line);
        let index = after.checked_sub(1)?;
        let section = self.sections.get(index)?;
        let local = line - section.first_line;
        let per_line = self.metrics.per_line;
        let start = (local * per_line).max(section.lead);
        let end = ((local + 1) * per_line).min(section.lead + section.count);
        Some(Line {
            heading: (local == 0 && section.lead > 0).then_some(index as u32),
            first: section.first + start - section.lead,
            count: end.saturating_sub(start),
        })
    }

    /// The entry of `line` at `column`, or the nearest one on that line;
    /// `None` when the line holds no entry.
    fn entry_on(&self, line: u32, column: u32) -> Option<u32> {
        let found = self.line(line).filter(|found| found.count > 0)?;
        // A heading stands in the line's first column.
        let lead = u32::from(found.heading.is_some() && self.metrics.per_line > 1);
        Some(found.first + column.saturating_sub(lead).min(found.count - 1))
    }

    /// The offset that puts `entry`'s line at the top.
    pub fn offset_of(&self, entry: u32) -> f64 {
        f64::from(self.line_of(entry)) * self.metrics.line_height
    }

    /// The height of all the lines.
    pub fn extent(&self) -> f64 {
        f64::from(self.lines) * self.metrics.line_height
    }

    /// The entries at least partly in sight when scrolled to `offset` in a
    /// viewport `height` tall. `None` while there is no height or no entry.
    pub fn in_sight(&self, offset: f64, height: f64) -> Option<Span> {
        if self.total == 0 || height <= 0.0 {
            return None;
        }
        let line_height = self.metrics.line_height;
        let last_line = self.lines - 1;
        let offset = offset.max(0.0);
        let first_line = ((offset / line_height).floor() as u32).min(last_line);
        // The line the bottom edge falls in; an edge exactly between two
        // lines belongs to the one above.
        let bottom = ((offset + height) / line_height).ceil() as u32;
        let end_line = bottom.saturating_sub(1).clamp(first_line, last_line);
        let last_entry = self.total - 1;
        let first = self.line(first_line)?.first.min(last_entry);
        let end = self.line(end_line)?;
        let last = (end.first + end.count.saturating_sub(1)).clamp(first, last_entry);
        Some(Span { first, last })
    }

    /// The offset that brings `entry` fully into a viewport `height` tall,
    /// moving as little as possible from `offset`; `None` when it already is.
    /// A group's first entry brings the group's heading along.
    pub fn offset_showing(&self, entry: u32, offset: f64, height: f64) -> Option<f64> {
        let line_height = self.metrics.line_height;
        let line = self.line_of(entry);
        let top_line = self
            .section_of(entry)
            .filter(|section| section.first == entry)
            .map_or(line, |section| section.first_line);
        let top = f64::from(top_line) * line_height;
        let bottom = f64::from(line + 1) * line_height;
        if top < offset {
            Some(top)
        } else if bottom > offset + height {
            Some((bottom - height).max(0.0))
        } else {
            None
        }
    }

    /// The entry `lines` lines above (`up`) or below the one at `line` and
    /// `column`, in the same column or as near to it as that line goes.
    /// Lines that hold a heading alone are passed over.
    fn across(&self, line: u32, column: u32, lines: u32, up: bool) -> Option<u32> {
        let last_line = self.lines.checked_sub(1)?;
        let target = if up {
            line.saturating_sub(lines)
        } else {
            line.saturating_add(lines).min(last_line)
        };
        let step = |from: u32, forward: bool| {
            if forward {
                from.checked_sub(1)
            } else {
                from.checked_add(1).filter(|&next| next <= last_line)
            }
        };
        // On from the target, the way the move goes; then back towards
        // where it started.
        for forward in [up, !up] {
            let mut candidate = Some(target);
            while let Some(at) = candidate {
                if at == line {
                    break;
                }
                if let Some(entry) = self.entry_on(at, column) {
                    return Some(entry);
                }
                candidate = step(at, forward);
            }
        }
        None
    }

    /// Where `motion` takes the cursor, from `cursor` (`None`: nothing has
    /// the cursor yet). `None` when there is nowhere to go.
    pub fn moved(&self, motion: Motion, cursor: Option<u32>, height: f64) -> Option<u32> {
        let last = self.total.checked_sub(1)?;
        let Some(cursor) = cursor else {
            // The first key lands on an end of the folder.
            return Some(match motion {
                Motion::Up | Motion::End | Motion::PageUp => last,
                _ => 0,
            });
        };
        let cursor = cursor.min(last);
        let (line, column) = self.place(cursor);
        let grid = self.metrics.per_line > 1;
        let target = match motion {
            Motion::Up => self.across(line, column, 1, true)?,
            Motion::Down => self.across(line, column, 1, false)?,
            Motion::Left if grid => cursor.checked_sub(1)?,
            Motion::Right if grid && cursor < last => cursor + 1,
            Motion::Left | Motion::Right => return None,
            Motion::Home => 0,
            Motion::End => last,
            Motion::PageUp => self.across(line, column, self.metrics.page(height), true)?,
            Motion::PageDown => self.across(line, column, self.metrics.page(height), false)?,
        };
        (target != cursor).then_some(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: Metrics = Metrics {
        line_height: 28.0,
        per_line: 1,
        tile: None,
    };
    /// Four tiles to a line, 100 tall.
    const GRID: Metrics = Metrics {
        line_height: 100.0,
        per_line: 4,
        tile: None,
    };

    const fn span(first: u32, last: u32) -> Span {
        Span { first, last }
    }

    /// `total` entries in no group.
    fn plain(metrics: Metrics, total: u32) -> Layout {
        Layout::new(metrics, total, &[])
    }

    /// Groups of the given sizes, one after another.
    fn grouped(metrics: Metrics, sizes: &[u32]) -> Layout {
        let mut first = 0;
        let groups: Vec<Group> = sizes
            .iter()
            .map(|&count| {
                let group = Group { first, count };
                first += count;
                group
            })
            .collect();
        Layout::new(metrics, first, &groups)
    }

    #[test]
    fn whole_rows_at_the_top() {
        assert_eq!(plain(LIST, 1000).in_sight(0.0, 280.0), Some(span(0, 9)));
    }

    #[test]
    fn rows_cut_by_either_edge_count() {
        // Half of row 1 at the top, half of row 11 at the bottom.
        assert_eq!(plain(LIST, 1000).in_sight(42.0, 280.0), Some(span(1, 11)));
    }

    #[test]
    fn a_short_folder_ends_at_its_last_row() {
        assert_eq!(plain(LIST, 3).in_sight(0.0, 280.0), Some(span(0, 2)));
        assert_eq!(span(0, 2).len(), 3);
    }

    #[test]
    fn an_offset_past_the_end_shows_the_last_row() {
        assert_eq!(plain(LIST, 5).in_sight(10_000.0, 280.0), Some(span(4, 4)));
    }

    #[test]
    fn nothing_is_in_sight_without_height_or_rows() {
        assert_eq!(plain(LIST, 10).in_sight(0.0, 0.0), None);
        assert_eq!(plain(LIST, 0).in_sight(0.0, 280.0), None);
        assert_eq!(plain(LIST, 0).lines(), 0);
    }

    #[test]
    fn a_row_goes_to_the_top_at_its_offset() {
        let layout = plain(LIST, 100_000);
        let offset = layout.offset_of(50_000);
        assert_eq!(
            layout.in_sight(offset, 280.0).map(|span| span.first),
            Some(50_000)
        );
    }

    #[test]
    fn the_grid_counts_whole_lines_of_tiles() {
        // Lines 1 and 2 are in sight; the folder's last line holds 2 tiles.
        let layout = plain(GRID, 10);
        assert_eq!(layout.in_sight(100.0, 200.0), Some(span(4, 9)));
        assert_eq!(layout.lines(), 3);
        assert_eq!(layout.extent(), 300.0);
        assert_eq!(layout.offset_of(9), 200.0);
    }

    #[test]
    fn the_tiles_of_a_line_share_its_whole_width() {
        // Medium tiles are 104 wide: 5 fit in 600, each in a 120 wide share.
        let metrics = Metrics::of(DisplayType::Icons, IconSize::Medium, 600.0);
        assert_eq!(metrics.per_line, 5);
        assert_eq!(metrics.tile.map(|tile| tile.cell), Some(120.0));
        // Too narrow for one: the tile keeps its own width.
        let narrow = Metrics::of(DisplayType::Icons, IconSize::Medium, 50.0);
        assert_eq!(narrow.tile.map(|tile| tile.cell), Some(104.0));
    }

    #[test]
    fn the_grid_fits_as_many_tiles_as_the_width_allows() {
        let metrics = Metrics::of(DisplayType::Icons, IconSize::Medium, 500.0);
        assert_eq!(metrics.per_line, 4);
        assert_eq!(
            Metrics::of(DisplayType::Icons, IconSize::Medium, 10.0).per_line,
            1
        );
        assert_eq!(
            Metrics::of(DisplayType::List, IconSize::Medium, 500.0).per_line,
            1
        );
    }

    #[test]
    fn an_entry_out_of_sight_is_shown_with_the_least_scrolling() {
        // Above: its top goes to the top. Below: its bottom to the bottom.
        let layout = plain(LIST, 100);
        assert_eq!(layout.offset_showing(2, 280.0, 280.0), Some(56.0));
        assert_eq!(
            layout.offset_showing(30, 0.0, 280.0),
            Some(31.0f64.mul_add(28.0, -280.0))
        );
        assert_eq!(layout.offset_showing(5, 0.0, 280.0), None);
    }

    #[test]
    fn arrows_move_by_one_entry_or_one_line() {
        let (list, grid) = (plain(LIST, 10), plain(GRID, 10));
        assert_eq!(list.moved(Motion::Down, Some(3), 280.0), Some(4));
        assert_eq!(list.moved(Motion::Up, Some(0), 280.0), None);
        assert_eq!(list.moved(Motion::Right, Some(3), 280.0), None);
        assert_eq!(grid.moved(Motion::Down, Some(1), 300.0), Some(5));
        assert_eq!(grid.moved(Motion::Right, Some(3), 300.0), Some(4));
        assert_eq!(grid.moved(Motion::Left, Some(0), 300.0), None);
    }

    #[test]
    fn down_into_a_short_last_line_lands_on_its_end() {
        // Entry 7 is above nothing: the last line holds entries 8 and 9.
        let grid = plain(GRID, 10);
        assert_eq!(grid.moved(Motion::Down, Some(7), 300.0), Some(9));
        assert_eq!(grid.moved(Motion::Down, Some(9), 300.0), None);
    }

    #[test]
    fn the_first_key_lands_on_an_end_of_the_folder() {
        assert_eq!(plain(LIST, 10).moved(Motion::Down, None, 280.0), Some(0));
        assert_eq!(plain(LIST, 10).moved(Motion::Up, None, 280.0), Some(9));
        assert_eq!(plain(LIST, 0).moved(Motion::Down, None, 280.0), None);
    }

    #[test]
    fn page_keys_move_by_what_fits_less_one_line() {
        let list = plain(LIST, 100);
        assert_eq!(LIST.page(280.0), 9);
        assert_eq!(list.moved(Motion::PageDown, Some(0), 280.0), Some(9));
        assert_eq!(list.moved(Motion::PageUp, Some(4), 280.0), Some(0));
        assert_eq!(GRID.page(300.0), 2);
        // Two lines down, in the same column.
        assert_eq!(
            plain(GRID, 100).moved(Motion::PageDown, Some(5), 300.0),
            Some(13)
        );
    }

    #[test]
    fn in_the_list_each_group_s_heading_takes_a_line_of_its_own() {
        // Lines: heading, 0, 1, heading, 2, 3, 4.
        let layout = grouped(LIST, &[2, 3]);
        assert_eq!(layout.lines(), 7);
        let line = |index| layout.line(index).expect("a line");
        assert_eq!(
            line(0),
            Line {
                heading: Some(0),
                first: 0,
                count: 0
            }
        );
        assert_eq!(
            line(2),
            Line {
                heading: None,
                first: 1,
                count: 1
            }
        );
        assert_eq!(line(3).heading, Some(1));
        assert_eq!((line(4).first, line(6).first), (2, 4));
        assert_eq!(layout.line(7), None);
        assert_eq!(layout.line_of(1), 2);
        assert_eq!(layout.line_of(2), 4);
        assert_eq!(layout.offset_of(4), 6.0 * 28.0);
        assert_eq!(layout.extent(), 7.0 * 28.0);
        // Lines 2 to 4 in sight: entry 1, a heading and entry 2.
        assert_eq!(layout.in_sight(56.0, 84.0), Some(span(1, 2)));
        // A heading alone at the bottom: the entry after it is asked for too.
        assert_eq!(layout.in_sight(0.0, 28.0), Some(span(0, 0)));
        assert_eq!(layout.in_sight(84.0, 28.0), Some(span(2, 2)));
    }

    #[test]
    fn arrows_pass_over_headings_in_the_list() {
        let layout = grouped(LIST, &[2, 3]);
        assert_eq!(layout.moved(Motion::Down, Some(1), 280.0), Some(2));
        assert_eq!(layout.moved(Motion::Up, Some(2), 280.0), Some(1));
        assert_eq!(layout.moved(Motion::Up, Some(0), 280.0), None);
        assert_eq!(layout.moved(Motion::Down, Some(4), 280.0), None);
        // A page up that would land on the first heading stops under it.
        assert_eq!(layout.moved(Motion::PageUp, Some(3), 280.0), Some(0));
        assert_eq!(layout.moved(Motion::PageDown, Some(0), 280.0), Some(4));
    }

    #[test]
    fn a_group_s_first_entry_brings_its_heading_into_sight() {
        let layout = grouped(LIST, &[2, 3]);
        // Entry 2 is on line 4; its heading is line 3.
        assert_eq!(layout.offset_showing(2, 140.0, 56.0), Some(84.0));
        assert_eq!(layout.offset_showing(3, 168.0, 56.0), Some(140.0));
    }

    #[test]
    fn in_the_grid_a_heading_takes_the_first_cell_of_its_group() {
        // Four to a line:  H 0 1 2 / 3 4 . . / H 5 6 .
        let layout = grouped(GRID, &[5, 2]);
        assert_eq!(layout.lines(), 3);
        let line = |index| layout.line(index).expect("a line");
        assert_eq!(
            line(0),
            Line {
                heading: Some(0),
                first: 0,
                count: 3
            }
        );
        assert_eq!(
            line(1),
            Line {
                heading: None,
                first: 3,
                count: 2
            }
        );
        assert_eq!(
            line(2),
            Line {
                heading: Some(1),
                first: 5,
                count: 2
            }
        );
        assert_eq!(layout.in_sight(100.0, 200.0), Some(span(3, 6)));
        // Down keeps the column, or goes as near to it as the line has.
        assert_eq!(layout.moved(Motion::Down, Some(0), 300.0), Some(4));
        assert_eq!(layout.moved(Motion::Down, Some(2), 300.0), Some(4));
        assert_eq!(layout.moved(Motion::Down, Some(4), 300.0), Some(5));
        // Up from under a heading's cell lands on the line's first entry.
        assert_eq!(layout.moved(Motion::Up, Some(3), 300.0), Some(0));
        assert_eq!(layout.moved(Motion::Up, Some(6), 300.0), Some(4));
        // Left and right go from entry to entry, across groups.
        assert_eq!(layout.moved(Motion::Right, Some(4), 300.0), Some(5));
        assert_eq!(layout.moved(Motion::Left, Some(5), 300.0), Some(4));
    }

    #[test]
    fn groups_that_do_not_hold_every_entry_are_not_laid_out() {
        let gap = [Group { first: 1, count: 2 }];
        assert_eq!(Layout::new(LIST, 3, &gap), plain(LIST, 3));
        let short = [Group { first: 0, count: 2 }];
        assert_eq!(Layout::new(LIST, 3, &short), plain(LIST, 3));
        // Another grouping of the same entries is another arrangement.
        assert_ne!(
            grouped(LIST, &[2, 3]).revision(),
            grouped(LIST, &[3, 2]).revision()
        );
        assert_ne!(grouped(LIST, &[5]).revision(), plain(LIST, 5).revision());
    }
}
