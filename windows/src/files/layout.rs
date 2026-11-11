//! Where entries sit in the list or the icon grid: how many share a line, how
//! tall a line is, and which entries a scroll position shows.
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

/// How the entries are laid out: lines of `per_line` entries, each `line_height` tall.
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

    /// How many lines `total` entries take.
    pub const fn lines(&self, total: u32) -> u32 {
        total.div_ceil(self.per_line)
    }

    /// The line `entry` is on.
    pub const fn line_of(&self, entry: u32) -> u32 {
        entry / self.per_line
    }

    /// The offset that puts `entry`'s line at the top.
    pub fn offset_of(&self, entry: u32) -> f64 {
        f64::from(self.line_of(entry)) * self.line_height
    }

    /// The height of all `total` entries.
    pub fn extent(&self, total: u32) -> f64 {
        f64::from(self.lines(total)) * self.line_height
    }

    /// The entries at least partly in sight when scrolled to `offset` in a
    /// viewport `height` tall. `None` while there is no height or no entry.
    pub fn in_sight(&self, offset: f64, height: f64, total: u32) -> Option<Span> {
        if total == 0 || height <= 0.0 {
            return None;
        }
        let last_line = self.lines(total) - 1;
        let offset = offset.max(0.0);
        let first_line = ((offset / self.line_height).floor() as u32).min(last_line);
        // The line the bottom edge falls in; an edge exactly between two
        // lines belongs to the one above.
        let bottom = ((offset + height) / self.line_height).ceil() as u32;
        let end_line = bottom.saturating_sub(1).clamp(first_line, last_line);
        Some(Span {
            first: first_line * self.per_line,
            last: ((end_line + 1) * self.per_line - 1).min(total - 1),
        })
    }

    /// The offset that brings `entry` fully into a viewport `height` tall,
    /// moving as little as possible from `offset`; `None` when it already is.
    pub fn offset_showing(&self, entry: u32, offset: f64, height: f64) -> Option<f64> {
        let top = self.offset_of(entry);
        let bottom = top + self.line_height;
        if top < offset {
            Some(top)
        } else if bottom > offset + height {
            Some((bottom - height).max(0.0))
        } else {
            None
        }
    }

    /// How many entries a page key moves by: the lines that fit, less one.
    pub fn page(&self, height: f64) -> u32 {
        let lines = ((height / self.line_height).floor() as u32).max(2) - 1;
        lines * self.per_line
    }
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

impl Metrics {
    /// Where `motion` takes the cursor among `total` entries, from `cursor`
    /// (`None`: nothing has the cursor yet). `None` when there is nowhere to go.
    pub fn moved(
        &self,
        motion: Motion,
        cursor: Option<u32>,
        total: u32,
        height: f64,
    ) -> Option<u32> {
        if total == 0 {
            return None;
        }
        let last = total - 1;
        let Some(cursor) = cursor else {
            // The first key lands on an end of the folder.
            return Some(match motion {
                Motion::Up | Motion::End | Motion::PageUp => last,
                _ => 0,
            });
        };
        let cursor = cursor.min(last);
        let target = match motion {
            Motion::Up => cursor.checked_sub(self.per_line)?,
            Motion::Down => {
                let below = cursor + self.per_line;
                if below <= last {
                    below
                } else if self.line_of(cursor) < self.line_of(last) {
                    // The last line is short: go to its end.
                    last
                } else {
                    return None;
                }
            }
            Motion::Left if self.per_line > 1 => cursor.checked_sub(1)?,
            Motion::Right if self.per_line > 1 && cursor < last => cursor + 1,
            Motion::Left | Motion::Right => return None,
            Motion::Home => 0,
            Motion::End => last,
            Motion::PageUp => cursor.saturating_sub(self.page(height)),
            Motion::PageDown => (cursor + self.page(height)).min(last),
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

    #[test]
    fn whole_rows_at_the_top() {
        assert_eq!(LIST.in_sight(0.0, 280.0, 1000), Some(span(0, 9)));
    }

    #[test]
    fn rows_cut_by_either_edge_count() {
        // Half of row 1 at the top, half of row 11 at the bottom.
        assert_eq!(LIST.in_sight(42.0, 280.0, 1000), Some(span(1, 11)));
    }

    #[test]
    fn a_short_folder_ends_at_its_last_row() {
        assert_eq!(LIST.in_sight(0.0, 280.0, 3), Some(span(0, 2)));
        assert_eq!(span(0, 2).len(), 3);
    }

    #[test]
    fn an_offset_past_the_end_shows_the_last_row() {
        assert_eq!(LIST.in_sight(10_000.0, 280.0, 5), Some(span(4, 4)));
    }

    #[test]
    fn nothing_is_in_sight_without_height_or_rows() {
        assert_eq!(LIST.in_sight(0.0, 0.0, 10), None);
        assert_eq!(LIST.in_sight(0.0, 280.0, 0), None);
    }

    #[test]
    fn a_row_goes_to_the_top_at_its_offset() {
        let offset = LIST.offset_of(50_000);
        assert_eq!(
            LIST.in_sight(offset, 280.0, 100_000).map(|span| span.first),
            Some(50_000)
        );
    }

    #[test]
    fn the_grid_counts_whole_lines_of_tiles() {
        // Lines 1 and 2 are in sight; the folder's last line holds 2 tiles.
        assert_eq!(GRID.in_sight(100.0, 200.0, 10), Some(span(4, 9)));
        assert_eq!(GRID.lines(10), 3);
        assert_eq!(GRID.extent(10), 300.0);
        assert_eq!(GRID.offset_of(9), 200.0);
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
        assert_eq!(LIST.offset_showing(2, 280.0, 280.0), Some(56.0));
        assert_eq!(
            LIST.offset_showing(30, 0.0, 280.0),
            Some(31.0f64.mul_add(28.0, -280.0))
        );
        assert_eq!(LIST.offset_showing(5, 0.0, 280.0), None);
    }

    #[test]
    fn arrows_move_by_one_entry_or_one_line() {
        assert_eq!(LIST.moved(Motion::Down, Some(3), 10, 280.0), Some(4));
        assert_eq!(LIST.moved(Motion::Up, Some(0), 10, 280.0), None);
        assert_eq!(LIST.moved(Motion::Right, Some(3), 10, 280.0), None);
        assert_eq!(GRID.moved(Motion::Down, Some(1), 10, 300.0), Some(5));
        assert_eq!(GRID.moved(Motion::Right, Some(3), 10, 300.0), Some(4));
        assert_eq!(GRID.moved(Motion::Left, Some(0), 10, 300.0), None);
    }

    #[test]
    fn down_into_a_short_last_line_lands_on_its_end() {
        // Entry 7 is above nothing: the last line holds entries 8 and 9.
        assert_eq!(GRID.moved(Motion::Down, Some(7), 10, 300.0), Some(9));
        assert_eq!(GRID.moved(Motion::Down, Some(9), 10, 300.0), None);
    }

    #[test]
    fn the_first_key_lands_on_an_end_of_the_folder() {
        assert_eq!(LIST.moved(Motion::Down, None, 10, 280.0), Some(0));
        assert_eq!(LIST.moved(Motion::Up, None, 10, 280.0), Some(9));
        assert_eq!(LIST.moved(Motion::Down, None, 0, 280.0), None);
    }

    #[test]
    fn page_keys_move_by_what_fits_less_one_line() {
        assert_eq!(LIST.page(280.0), 9);
        assert_eq!(LIST.moved(Motion::PageDown, Some(0), 100, 280.0), Some(9));
        assert_eq!(LIST.moved(Motion::PageUp, Some(4), 100, 280.0), Some(0));
        assert_eq!(GRID.page(300.0), 8);
    }
}
