//! The details view's columns: which are shown, how wide they are and which
//! give way first in a narrow pane.
use facet::Facet;
use serde::{Deserialize, Serialize};

use crate::Sort;

/// Narrowest the Name column gets before other columns are hidden.
pub const NAME_MIN_WIDTH: f32 = 140.0;
/// Narrowest a column can be dragged to.
pub const COLUMN_MIN_WIDTH: f32 = 56.0;
pub const COLUMN_MAX_WIDTH: f32 = 600.0;
/// Horizontal padding inside every cell, header included.
pub const CELL_PADDING: f32 = 10.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Facet)]
#[repr(C)]
pub enum Column {
    Name,
    /// Where a result is, relative to the searched folder. Only while
    /// searching or showing several folders.
    Folder,
    /// The line of the file that a content search matched. Only while file
    /// contents are searched.
    Match,
    Modified,
    Created,
    /// When the file was last opened: in the app, or as the filesystem
    /// tracks it.
    Accessed,
    /// How many times the file was opened from the app.
    OpenCount,
    Kind,
    Size,
    /// A picture's or video's size in pixels.
    Dimensions,
    /// How long a video or a sound plays.
    Duration,
}

impl Column {
    /// Columns the user can show or hide, in menu order.
    pub const OPTIONAL: [Self; 8] = [
        Self::Modified,
        Self::Created,
        Self::Accessed,
        Self::OpenCount,
        Self::Kind,
        Self::Size,
        Self::Dimensions,
        Self::Duration,
    ];

    pub const fn title(self, in_trash: bool) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Folder => "Folder",
            Self::Match => "Match",
            // In the Recycle Bin the date column holds the deletion time.
            Self::Modified if in_trash => "Deleted",
            Self::Modified => "Modified",
            Self::Created => "Created",
            Self::Accessed => "Opened",
            Self::OpenCount => "Opens",
            Self::Kind => "Kind",
            Self::Size => "Size",
            Self::Dimensions => "Dimensions",
            Self::Duration => "Duration",
        }
    }

    pub const fn sort(self) -> Option<Sort> {
        match self {
            Self::Name => Some(Sort::Name),
            // Known only for the files in view: nothing to sort by.
            Self::Folder | Self::Match | Self::Dimensions | Self::Duration => None,
            Self::Modified => Some(Sort::Modified),
            Self::Created => Some(Sort::Created),
            Self::Accessed => Some(Sort::Accessed),
            Self::OpenCount => Some(Sort::MostOpened),
            Self::Kind => Some(Sort::Kind),
            Self::Size => Some(Sort::Size),
        }
    }

    pub const fn default_width(self) -> f32 {
        match self {
            Self::Name => 0.0,
            Self::Folder => 200.0,
            Self::Match => 320.0,
            Self::Modified | Self::Created | Self::Accessed => 130.0,
            Self::Kind | Self::Dimensions => 120.0,
            Self::Size => 90.0,
            Self::OpenCount | Self::Duration => 70.0,
        }
    }

    /// Narrowest a column gets on its own when the pane is too narrow,
    /// before columns are hidden.
    const fn compact_width(self) -> f32 {
        match self {
            Self::Name => 0.0,
            Self::Folder => 120.0,
            Self::Match => 160.0,
            Self::Modified | Self::Created | Self::Accessed | Self::Dimensions => 96.0,
            Self::Kind => 80.0,
            Self::Size | Self::OpenCount | Self::Duration => 64.0,
        }
    }

    /// Numbers read best right-aligned; everything else starts at the left.
    pub const fn right_aligned(self) -> bool {
        matches!(self, Self::Size | Self::OpenCount | Self::Duration)
    }

    /// Lower values are hidden first when the pane is too narrow.
    const fn keep_priority(self) -> u8 {
        match self {
            Self::Created
            | Self::Accessed
            | Self::OpenCount
            | Self::Dimensions
            | Self::Duration => 0,
            Self::Kind => 1,
            Self::Size => 2,
            Self::Modified => 3,
            Self::Folder => 4,
            Self::Match => 5,
            Self::Name => u8::MAX,
        }
    }
}

/// Persisted column choice and widths, shared by every details view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Facet)]
#[serde(default)]
pub struct ListColumns {
    /// Optional columns shown, in display order.
    pub shown: Vec<Column>,
    /// Widths the user dragged; others use [`Column::default_width`].
    pub widths: Vec<(Column, f32)>,
}

/// Widths are clamped and rounded when set, so never NaN: equality is total.
impl Eq for ListColumns {}

impl Default for ListColumns {
    fn default() -> Self {
        Self {
            shown: vec![Column::Modified, Column::Size],
            widths: Vec::new(),
        }
    }
}

impl ListColumns {
    pub fn width(&self, column: Column) -> f32 {
        self.widths
            .iter()
            .find(|(c, _)| *c == column)
            .map_or_else(|| column.default_width(), |(_, width)| *width)
    }

    pub fn set_width(&mut self, column: Column, width: f32) {
        let width = width.clamp(COLUMN_MIN_WIDTH, COLUMN_MAX_WIDTH).round();
        if let Some(entry) = self.widths.iter_mut().find(|(c, _)| *c == column) {
            entry.1 = width;
        } else {
            self.widths.push((column, width));
        }
    }

    pub fn reset_width(&mut self, column: Column) {
        self.widths.retain(|(c, _)| *c != column);
    }

    pub fn is_shown(&self, column: Column) -> bool {
        self.shown.contains(&column)
    }

    /// Shows or hides an optional column, keeping [`Column::OPTIONAL`] order.
    pub fn toggle(&mut self, column: Column) {
        if self.is_shown(column) {
            self.shown.retain(|c| *c != column);
        } else {
            self.shown.push(column);
            self.shown.sort_by_key(|c| {
                Column::OPTIONAL
                    .iter()
                    .position(|o| o == c)
                    .unwrap_or(usize::MAX)
            });
        }
    }

    /// The columns to draw in `available` points, with their widths (the
    /// Name column's is 0: it takes what is left). When Name would get less
    /// than [`NAME_MIN_WIDTH`], the other columns first shrink toward their
    /// compact widths, then the least important are hidden.
    pub fn layout(&self, available: f32, with_folder: bool) -> Vec<(Column, f32)> {
        let mut columns: Vec<(Column, f32)> = std::iter::once((Column::Name, 0.0))
            .chain(with_folder.then(|| (Column::Folder, self.width(Column::Folder))))
            .chain(self.shown.iter().map(|&c| (c, self.width(c))))
            .collect();
        let budget = available - NAME_MIN_WIDTH;
        let compact = |columns: &[(Column, f32)]| {
            columns
                .iter()
                .map(|(c, w)| c.compact_width().min(*w))
                .sum::<f32>()
        };
        #[allow(clippy::while_float)]
        while compact(&columns) > budget {
            let Some(drop) = columns
                .iter()
                .enumerate()
                .filter(|(_, (c, _))| *c != Column::Name)
                .min_by_key(|(_, (c, _))| c.keep_priority())
                .map(|(index, _)| index)
            else {
                break;
            };
            columns.remove(drop);
        }
        // Share the shortfall by how much each column can give.
        let total: f32 = columns.iter().map(|(_, w)| w).sum();
        let slack: f32 = columns
            .iter()
            .map(|(c, w)| (w - c.compact_width()).max(0.0))
            .sum();
        let shortfall = total - budget.max(0.0);
        if shortfall > 0.0 && slack > 0.0 {
            let ratio = (shortfall / slack).min(1.0);
            for (column, width) in columns.iter_mut().skip(1) {
                let give = (*width - column.compact_width()).max(0.0);
                *width = give.mul_add(-ratio, *width).round();
            }
        }
        columns
    }
}
