//! The details view's columns: which are shown, how wide they are and which
//! give way first in a narrow pane.
use crate::app::Sort;
use crate::data::files::EntryType;
use serde::{Deserialize, Serialize};

/// Narrowest the Name column gets before other columns are hidden.
pub const NAME_MIN_WIDTH: f32 = 140.0;
/// Narrowest a column can be dragged to.
pub const COLUMN_MIN_WIDTH: f32 = 56.0;
pub const COLUMN_MAX_WIDTH: f32 = 600.0;
/// Horizontal padding inside every cell, header included.
pub const CELL_PADDING: f32 = 10.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Column {
    Name,
    /// Where a result is, relative to the searched folder. Only while
    /// searching or showing several folders.
    Folder,
    Modified,
    Created,
    Kind,
    Size,
}

impl Column {
    /// Columns the user can show or hide, in menu order.
    pub const OPTIONAL: [Self; 4] = [Self::Modified, Self::Created, Self::Kind, Self::Size];

    pub const fn title(self, in_trash: bool) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Folder => "Folder",
            // In the Recycle Bin the date column holds the deletion time.
            Self::Modified if in_trash => "Deleted",
            Self::Modified => "Modified",
            Self::Created => "Created",
            Self::Kind => "Kind",
            Self::Size => "Size",
        }
    }

    pub const fn sort(self) -> Option<Sort> {
        match self {
            Self::Name => Some(Sort::Name),
            Self::Folder => None,
            Self::Modified => Some(Sort::Modified),
            Self::Created => Some(Sort::Created),
            Self::Kind => Some(Sort::Kind),
            Self::Size => Some(Sort::Size),
        }
    }

    pub const fn default_width(self) -> f32 {
        match self {
            Self::Name => 0.0,
            Self::Folder => 200.0,
            Self::Modified | Self::Created => 130.0,
            Self::Kind => 120.0,
            Self::Size => 90.0,
        }
    }

    /// Narrowest a column gets on its own when the pane is too narrow,
    /// before columns are hidden.
    const fn compact_width(self) -> f32 {
        match self {
            Self::Name => 0.0,
            Self::Folder => 120.0,
            Self::Modified | Self::Created => 96.0,
            Self::Kind => 80.0,
            Self::Size => 64.0,
        }
    }

    /// Numbers read best right-aligned; everything else starts at the left.
    pub const fn right_aligned(self) -> bool {
        matches!(self, Self::Size)
    }

    /// Lower values are hidden first when the pane is too narrow.
    const fn keep_priority(self) -> u8 {
        match self {
            Self::Created => 0,
            Self::Kind => 1,
            Self::Size => 2,
            Self::Modified => 3,
            Self::Folder => 4,
            Self::Name => u8::MAX,
        }
    }
}

/// Persisted column choice and widths, shared by every details view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ListColumns {
    /// Optional columns shown, in display order.
    pub shown: Vec<Column>,
    /// Widths the user dragged; others use [`Column::default_width`].
    pub widths: Vec<(Column, f32)>,
}

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

/// What a file is, for the Kind column: "Folder", "Application", "PNG
/// image", "TXT file".
pub fn kind_label(name: &str, entry_type: EntryType) -> String {
    match entry_type {
        EntryType::Directory => return "Folder".to_owned(),
        EntryType::Package => {
            let app = std::path::Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("app"));
            return if app { "Application" } else { "Package" }.to_owned();
        }
        EntryType::File => {}
    }
    let Some(extension) = std::path::Path::new(name)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|ext| !ext.is_empty())
    else {
        return "File".to_owned();
    };
    let what = match crate::app::assets::kinds::media_kind(std::path::Path::new(name)) {
        Some(crate::app::assets::kinds::MediaKind::Video) => "video",
        Some(_) => "image",
        None => "file",
    };
    format!("{} {what}", extension.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_panes_shrink_then_hide_the_least_important_columns() {
        let columns = ListColumns {
            shown: vec![
                Column::Modified,
                Column::Created,
                Column::Kind,
                Column::Size,
            ],
            widths: Vec::new(),
        };
        let names = |layout: &[(Column, f32)]| layout.iter().map(|(c, _)| *c).collect::<Vec<_>>();
        let wide = columns.layout(2000.0, false);
        assert_eq!(
            names(&wide),
            [
                Column::Name,
                Column::Modified,
                Column::Created,
                Column::Kind,
                Column::Size
            ]
        );
        assert!((wide[1].1 - Column::Modified.default_width()).abs() < f32::EPSILON);
        // Shrunk, not hidden: 140 + 96 + 96 + 80 + 64 = 476.
        let shrunk = columns.layout(500.0, false);
        assert_eq!(names(&shrunk).len(), 5);
        let used: f32 = shrunk.iter().map(|(_, w)| w).sum();
        assert!(used + NAME_MIN_WIDTH <= 500.5, "{shrunk:?}");
        assert_eq!(
            names(&columns.layout(320.0, false)),
            [Column::Name, Column::Modified, Column::Size]
        );
        assert_eq!(names(&columns.layout(200.0, true)), [Column::Name]);
        assert_eq!(
            names(&columns.layout(270.0, true)),
            [Column::Name, Column::Folder]
        );
    }

    #[test]
    fn widths_are_clamped_and_resettable() {
        let mut columns = ListColumns::default();
        columns.set_width(Column::Size, 5.0);
        assert!((columns.width(Column::Size) - COLUMN_MIN_WIDTH).abs() < f32::EPSILON);
        columns.reset_width(Column::Size);
        assert!((columns.width(Column::Size) - Column::Size.default_width()).abs() < f32::EPSILON);
    }

    #[test]
    fn toggled_columns_keep_menu_order() {
        let mut columns = ListColumns::default();
        columns.toggle(Column::Kind);
        columns.toggle(Column::Created);
        assert_eq!(
            columns.shown,
            [
                Column::Modified,
                Column::Created,
                Column::Kind,
                Column::Size
            ]
        );
        columns.toggle(Column::Modified);
        assert_eq!(columns.shown, [Column::Created, Column::Kind, Column::Size]);
    }

    #[test]
    fn kinds_name_the_extension() {
        assert_eq!(kind_label("a.png", EntryType::File), "PNG image");
        assert_eq!(kind_label("a.mp4", EntryType::File), "MP4 video");
        assert_eq!(kind_label("notes.txt", EntryType::File), "TXT file");
        assert_eq!(kind_label("Makefile", EntryType::File), "File");
        assert_eq!(kind_label("src", EntryType::Directory), "Folder");
        assert_eq!(kind_label("Safari.app", EntryType::Package), "Application");
        assert_eq!(kind_label("Notes.rtfd", EntryType::Package), "Package");
    }
}
