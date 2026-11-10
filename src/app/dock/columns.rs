//! The details view's columns: which are shown, how wide they are and which
//! give way first in a narrow pane. Shared with every shell in `dirfleet_core`.
pub use dirfleet_core::columns::*;
pub use dirfleet_core::listing::kind_label;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::files::EntryType;

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
