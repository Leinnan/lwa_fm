//! The list's columns: which are shown, how wide, and the header above the rows.
use dirfleet_core::app::{ColumnChoice, ColumnView, Event, FolderView};
use dirfleet_core::columns::Column;
use dirfleet_core::view_settings::Sort;
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, Color, Grid, GridLength, HorizontalAlignment, Key, Menu,
    MenuExt as _, MenuItem, Orientation, PointerEventInfo, StackPanel, TextBlock, TextTrimming,
    Thickness, VerticalAlignment, View,
};

use super::glyph;
use crate::core_link::CoreLink;

pub const HEADER_HEIGHT: f64 = 30.0;
/// The width of the Folder column search results add.
const FOLDER_WIDTH: f64 = 180.0;
/// How wide the line a content search matched is shown.
const MATCH_WIDTH: f64 = 300.0;
/// The space between columns; the grip that resizes a column sits in it.
pub const COLUMN_GAP: f64 = 12.0;
/// Columns are not dragged narrower than this.
const MIN_WIDTH: f64 = 48.0;

/// The columns of every list: the ones shown, and the ones that can be.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColumnSet {
    pub shown: Vec<ColumnView>,
    pub optional: Vec<ColumnChoice>,
}

/// One column as a row lays it out.
#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub column: Column,
    pub title: String,
    /// `None` takes the space the others leave.
    pub width: Option<f64>,
    pub right_aligned: bool,
    pub sort: Option<Sort>,
}

/// The columns a list shows; `show_folder` adds each entry's folder after
/// its name, and `show_match` the line a content search matched.
pub fn cells(columns: &ColumnSet, show_folder: bool, show_match: bool) -> Vec<Cell> {
    let mut cells = Vec::with_capacity(columns.shown.len() + 2);
    for column in &columns.shown {
        cells.push(Cell {
            column: column.column,
            title: column.title.clone(),
            width: (column.width > 0).then(|| f64::from(column.width)),
            right_aligned: column.right_aligned,
            sort: column.sort,
        });
        if column.column == Column::Name && show_folder {
            cells.push(Cell {
                column: Column::Folder,
                title: "Folder".to_owned(),
                width: Some(FOLDER_WIDTH),
                right_aligned: false,
                sort: None,
            });
        }
        if column.column == Column::Name && show_match {
            cells.push(Cell {
                column: Column::Match,
                title: "Match".to_owned(),
                width: Some(MATCH_WIDTH),
                right_aligned: false,
                sort: None,
            });
        }
    }
    cells
}

/// The grid columns of a row.
pub fn lengths(cells: &[Cell]) -> Vec<GridLength> {
    cells
        .iter()
        .map(|cell| cell.width.map_or(GridLength::STAR, GridLength::Pixel))
        .collect()
}

/// A column edge being dragged.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Resize {
    pub column: Column,
    /// Where the pointer went down, across the window.
    pub from_x: f64,
    pub from_width: f64,
}

impl Resize {
    /// The column's width with the pointer at `x`. Columns sit right of Name,
    /// so dragging their left edge leftwards widens them.
    pub fn width_at(&self, x: f64) -> u32 {
        (self.from_width + (self.from_x - x)).max(MIN_WIDTH).round() as u32
    }
}

/// What the header tells the view that owns it.
#[derive(Clone, PartialEq)]
pub struct HeaderCallbacks {
    /// A press on the grip left of a column.
    pub grip_pressed: Callback<(Column, f64, PointerEventInfo)>,
    pub grip_moved: Callback<PointerEventInfo>,
    pub grip_released: Callback<PointerEventInfo>,
    /// A column chosen in the menu of columns.
    pub toggle: Callback<Key>,
}

/// The column titles, laid out like the rows below them. A title sorts by its
/// column, and again reverses the order.
pub fn header(
    core: &CoreLink,
    tab: u32,
    view: FolderView,
    columns: &ColumnSet,
    cells: &[Cell],
    width: f64,
    callbacks: &HeaderCallbacks,
) -> Border {
    let titles: Vec<View> = cells
        .iter()
        .zip(0..)
        .map(|(cell, position)| title(core, tab, view, cell, position, callbacks))
        .collect();
    let chooser = Button::new()
        .style(ButtonStyle::Subtle)
        .automation_name("Columns")
        .content(glyph("\u{E712}", 12.0))
        .menu(Menu::new(
            columns.optional.iter().map(|choice| {
                MenuItem::item(
                    choice.column as u32,
                    format!(
                        "{}{}",
                        if choice.shown { "✓ " } else { "    " },
                        choice.title
                    ),
                )
            }),
            callbacks.toggle.clone(),
        ));
    // The titles are closed off from the rows by a line.
    Border::new()
        .width(width)
        .height(HEADER_HEIGHT)
        .horizontal_alignment(HorizontalAlignment::Left)
        .border_brush(crate::theme::stroke())
        .border_thickness(Thickness::new(0.0, 0.0, 0.0, 1.0))
        .content(
            Grid::new()
                .columns([GridLength::STAR, GridLength::Auto])
                .children((
                    Grid::new()
                        .margin(Thickness::new(40.0, 0.0, 0.0, 0.0))
                        .columns(lengths(cells))
                        .column_spacing(COLUMN_GAP)
                        .children(titles),
                    Border::new().grid_column(1).content(chooser),
                )),
        )
}

/// One column's title: pressed, it sorts by the column; the grip beside a
/// fixed column resizes it.
fn title(
    core: &CoreLink,
    tab: u32,
    view: FolderView,
    cell: &Cell,
    position: i32,
    callbacks: &HeaderCallbacks,
) -> View {
    let sorted = cell.sort.is_some_and(|sort| sort == view.sort);
    let arrow: View = if sorted {
        glyph(if view.invert { "\u{E70D}" } else { "\u{E70E}" }, 9.0)
            .opacity(0.7)
            .into()
    } else {
        Border::new().into()
    };
    let title = TextBlock::new()
        .text(cell.title.clone())
        .font_size(12.0)
        .opacity(if sorted { 1.0 } else { 0.7 })
        .text_trimming(TextTrimming::CharacterEllipsis)
        .vertical_alignment(VerticalAlignment::Center);
    let label = StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(6.0)
        .children((title, arrow));
    let mut button = Button::new()
        .style(ButtonStyle::Subtle)
        .automation_name(format!("Sort by {}", cell.title))
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .horizontal_content_alignment(if cell.right_aligned {
            HorizontalAlignment::Right
        } else {
            HorizontalAlignment::Left
        })
        .is_enabled(cell.sort.is_some())
        .content(label);
    if let Some(sort) = cell.sort {
        let core = core.clone();
        let invert = view.invert;
        button = button.on_click(move || {
            core.send(if sorted {
                Event::SetReverse {
                    tab,
                    reverse: !invert,
                }
            } else {
                Event::SortBy { tab, sort }
            });
        });
    }
    // The grip sits in the gap left of a fixed column.
    let grip: View = cell.width.map_or_else(
        || Border::new().into(),
        |from_width| {
            let column = cell.column;
            let pressed = callbacks.grip_pressed.clone();
            Border::new()
                .width(COLUMN_GAP)
                .margin(Thickness::new(-COLUMN_GAP, 0.0, 0.0, 0.0))
                .horizontal_alignment(HorizontalAlignment::Left)
                .background(Color::transparent())
                .capture_pointer_on_press(true)
                .on_pointer_pressed(move |info: PointerEventInfo| {
                    pressed.call((column, from_width, info));
                })
                .on_pointer_moved(callbacks.grip_moved.clone())
                .on_pointer_released(callbacks.grip_released.clone())
                .content(
                    Border::new()
                        .width(1.0)
                        .height(14.0)
                        .background(Color::argb(0x40, 0x80, 0x80, 0x80)),
                )
                .into()
        },
    );
    Grid::new()
        .grid_column(position)
        .children((button, grip))
        .into()
}

/// The column a key of the columns menu names.
pub fn toggled(columns: &ColumnSet, key: &Key) -> Option<Column> {
    columns
        .optional
        .iter()
        .map(|choice| choice.column)
        .find(|column| Key::from(*column as u32) == *key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(column: Column, width: u32) -> ColumnView {
        ColumnView {
            column,
            title: format!("{column:?}"),
            width,
            right_aligned: false,
            sort: None,
        }
    }

    #[test]
    fn name_takes_the_space_the_others_leave() {
        let set = ColumnSet {
            shown: vec![column(Column::Name, 0), column(Column::Size, 90)],
            optional: Vec::new(),
        };
        let cells = cells(&set, false, false);
        assert_eq!(cells[0].width, None);
        assert_eq!(cells[1].width, Some(90.0));
    }

    #[test]
    fn results_from_several_folders_show_the_folder_after_the_name() {
        let set = ColumnSet {
            shown: vec![column(Column::Name, 0), column(Column::Size, 90)],
            optional: Vec::new(),
        };
        let shown = |folder, matched| -> Vec<Column> {
            cells(&set, folder, matched)
                .iter()
                .map(|cell| cell.column)
                .collect()
        };
        assert_eq!(
            shown(true, false),
            [Column::Name, Column::Folder, Column::Size]
        );
        // A search inside files shows the line it matched as well.
        assert_eq!(
            shown(true, true),
            [Column::Name, Column::Folder, Column::Match, Column::Size]
        );
        assert_eq!(
            shown(false, true),
            [Column::Name, Column::Match, Column::Size]
        );
    }

    #[test]
    fn dragging_an_edge_left_widens_its_column() {
        let resize = Resize {
            column: Column::Size,
            from_x: 500.0,
            from_width: 90.0,
        };
        assert_eq!(resize.width_at(470.0), 120);
        assert_eq!(resize.width_at(530.0), 60);
        assert_eq!(resize.width_at(900.0), 48, "not narrower than the least");
    }
}
