//! One row of the list. Each is a component of its own, so a row whose input
//! stayed the same is not drawn again, and hovering redraws that row only.
use std::rc::Rc;

use dirfleet_core::app::{Event, RenameView, RowView};
use dirfleet_core::columns::Column;
use windows_reactor::{
    Border, Brush, Callback, Color, Component, ComponentContext, Grid, GridLength,
    HorizontalAlignment, PointerEventInfo, TextBlock, TextTrimming, ThemeBrush, Thickness,
    VerticalAlignment, View, ViewContext,
};

use super::columns::{COLUMN_GAP, Cell, lengths};
use super::highlighted;
use super::layout::ROW_HEIGHT;
use super::press::{DragWatch, EntryHooks, PressClaim, Presses};
use super::rename::{RenameBox, RenameInput};

/// The cell of the line a content search matched: the line after its
/// number, with what matched in bold. A row without a match gets an empty
/// cell instead, never another column's text.
fn matched_line(row: &RowView, alignment: HorizontalAlignment, position: i32) -> View {
    let number = format!("{}:  ", row.matched_line);
    let shift = u32::try_from(number.len()).unwrap_or(0);
    let marks: Vec<dirfleet_core::app::TextRange> = row
        .matched_highlights
        .iter()
        .map(|range| dirfleet_core::app::TextRange {
            start: range.start + shift,
            end: range.end + shift,
        })
        .collect();
    highlighted(&format!("{number}{}", row.matched), &marks)
        .opacity(0.8)
        .vertical_alignment(VerticalAlignment::Center)
        .horizontal_alignment(alignment)
        .grid_column(position)
        .into()
}
use crate::core_link::CoreLink;

/// The line a group's heading takes in the list: its name and how many it
/// holds, over a rule. A press selects the group.
pub fn group_heading(
    core: &CoreLink,
    tab: u32,
    group: u32,
    (label, count): (&str, u32),
    width: f64,
    claim: &PressClaim,
) -> View {
    let (core, claim) = (core.clone(), claim.clone());
    Border::new()
        .height(ROW_HEIGHT)
        .width(width)
        .padding(Thickness::new(10.0, 0.0, 10.0, 0.0))
        // Transparent, not absent, so the whole line takes the pointer.
        .background(Color::transparent())
        .border_brush(crate::theme::stroke())
        .border_thickness(Thickness::new(0.0, 0.0, 0.0, 1.0))
        .automation_name(format!("{label}, {count}"))
        .on_pointer_pressed(move |info: PointerEventInfo| {
            // Not a press on empty space, which would clear the selection.
            claim.claim(&info);
            if info.is_left_button_pressed {
                core.send(Event::SelectGroup { tab, group });
            }
        })
        .content(
            super::heading_text(label, count)
                .font_size(12.0)
                .opacity(0.8)
                .vertical_alignment(VerticalAlignment::Bottom)
                .margin(Thickness::new(0.0, 0.0, 0.0, 4.0)),
        )
        .into()
}

#[derive(Clone, PartialEq)]
pub struct RowInput {
    pub core: CoreLink,
    pub tab: u32,
    /// The row's position in the folder.
    pub index: u32,
    /// The row, once the core sent it.
    pub row: Option<Rc<RowView>>,
    pub cells: Rc<Vec<Cell>>,
    pub width: f64,
    pub claim: PressClaim,
    pub hooks: EntryHooks,
    /// The row is being renamed: a text box stands in for its name.
    pub rename: Option<Rc<RenameView>>,
}

pub struct Row {
    core: CoreLink,
    tab: u32,
    index: u32,
    hovered: bool,
    selected: bool,
    presses: Presses,
    /// The click of a press on the row while it was selected, sent when the
    /// button goes up unless the press became a drag.
    held: Option<Event>,
    on_pressed: Callback<PointerEventInfo>,
    on_moved: Callback<PointerEventInfo>,
    on_released: Callback<PointerEventInfo>,
    on_drag: Callback<u32>,
}

#[derive(Clone, Copy, Debug)]
pub enum RowMessage {
    Pressed(PointerEventInfo),
    Released,
    /// The press has moved far enough to drag the row.
    Dragged,
    Hovered(bool),
}

/// The fill behind an entry: marked when selected, lighter under the pointer.
pub fn entry_fill(selected: bool, hovered: bool) -> Brush {
    match (selected, hovered) {
        (true, true) => Color::argb(0x38, 0x80, 0x80, 0x80).into(),
        (true, false) => Color::argb(0x28, 0x80, 0x80, 0x80).into(),
        (false, true) => Color::argb(0x14, 0x80, 0x80, 0x80).into(),
        // Transparent, not absent, so the whole entry takes the pointer.
        (false, false) => Color::transparent().into(),
    }
}

impl Component for Row {
    type Input = RowInput;
    type Message = RowMessage;

    fn create(input: &RowInput, context: &ComponentContext<Self>) -> Self {
        let claim = input.claim.clone();
        let sender = context.sender();
        let watch = DragWatch::default();
        let (pressed, moved, released) = (watch.clone(), watch.clone(), watch);
        let (on_move, on_release) = (context.sender(), context.sender());
        Self {
            core: input.core.clone(),
            tab: input.tab,
            index: input.index,
            hovered: false,
            selected: input.row.as_ref().is_some_and(|row| row.selected),
            presses: Presses::default(),
            held: None,
            // The claim is made as the press is seen, before the area behind sees it.
            on_pressed: Callback::new(move |info: PointerEventInfo| {
                claim.claim(&info);
                pressed.pressed(0, &info);
                _ = sender.send(RowMessage::Pressed(info));
            }),
            on_moved: Callback::new(move |info: PointerEventInfo| {
                if moved.moved(&info).is_some() {
                    _ = on_move.send(RowMessage::Dragged);
                }
            }),
            on_released: Callback::new(move |_: PointerEventInfo| {
                released.released();
                _ = on_release.send(RowMessage::Released);
            }),
            on_drag: input.hooks.on_drag.clone(),
        }
    }

    fn input_changed(&mut self, input: &RowInput, _: &ComponentContext<Self>) {
        if (self.tab, self.index) != (input.tab, input.index) {
            self.presses.forget();
            self.held = None;
        }
        self.tab = input.tab;
        self.index = input.index;
        self.selected = input.row.as_ref().is_some_and(|row| row.selected);
        self.on_drag = input.hooks.on_drag.clone();
    }

    fn update(&mut self, message: RowMessage, _: &ComponentContext<Self>) {
        match message {
            RowMessage::Hovered(hovered) => self.hovered = hovered,
            RowMessage::Pressed(info) => {
                self.held =
                    self.presses
                        .press(&self.core, self.tab, self.index, &info, self.selected);
            }
            RowMessage::Released => {
                if let Some(click) = self.held.take() {
                    self.core.send(click);
                }
            }
            RowMessage::Dragged => {
                self.held = None;
                self.on_drag.call(self.index);
            }
        }
    }

    fn view(&self, input: &RowInput, context: &mut ViewContext<Self>) -> View {
        let frame = Border::new().height(ROW_HEIGHT).width(input.width);
        let Some(row) = &input.row else {
            // Not sent yet: the space is held so the list keeps its length.
            return frame.into();
        };
        let mut texts = row.cells.iter();
        let cells: Vec<View> = input
            .cells
            .iter()
            .zip(0..)
            .map(|(cell, position)| {
                if cell.column == Column::Name
                    && let Some(rename) = &input.rename
                {
                    return Border::new()
                        .grid_column(position)
                        .content(View::component::<RenameBox>(RenameInput {
                            core: input.core.clone(),
                            tab: input.tab,
                            rename: Rc::clone(rename),
                            is_dir: row.is_dir,
                        }))
                        .into();
                }
                let alignment = if cell.right_aligned {
                    HorizontalAlignment::Right
                } else {
                    HorizontalAlignment::Left
                };
                if cell.column == Column::Name && !row.highlights.is_empty() {
                    return highlighted(&row.name, &row.highlights)
                        .vertical_alignment(VerticalAlignment::Center)
                        .horizontal_alignment(alignment)
                        .grid_column(position)
                        .into();
                }
                if cell.column == Column::Match && row.matched_line > 0 {
                    return matched_line(row, alignment, position);
                }
                let text = match cell.column {
                    Column::Name => TextBlock::new().text(row.name.clone()),
                    Column::Match => TextBlock::new(),
                    Column::Folder => TextBlock::new()
                        .text(row.folder.clone())
                        .font_size(12.0)
                        .opacity(0.7),
                    _ => TextBlock::new()
                        .text(texts.next().cloned().unwrap_or_default())
                        .font_size(12.0)
                        .opacity(0.7),
                };
                text.text_trimming(TextTrimming::CharacterEllipsis)
                    .vertical_alignment(VerticalAlignment::Center)
                    .horizontal_alignment(alignment)
                    .grid_column(position)
                    .into()
            })
            .collect();
        let mark: Brush = if row.selected {
            ThemeBrush::Accent.into()
        } else {
            Color::transparent().into()
        };
        // Files can be dropped on a folder.
        let frame = input.hooks.taking_drops(frame, row.is_dir, input.index);
        frame
            .corner_radius(4.0)
            .background(entry_fill(row.selected, self.hovered))
            .opacity(if row.cut { 0.5 } else { 1.0 })
            .automation_name(row.name.clone())
            .on_pointer_pressed(self.on_pressed.clone())
            .on_pointer_moved(self.on_moved.clone())
            .on_pointer_released(self.on_released.clone())
            .on_pointer_entered(context.callback(|_: PointerEventInfo| RowMessage::Hovered(true)))
            .on_pointer_exited(context.callback(|_: PointerEventInfo| RowMessage::Hovered(false)))
            .content(
                Grid::new()
                    .columns([
                        GridLength::Pixel(3.0),
                        GridLength::Pixel(20.0),
                        GridLength::STAR,
                    ])
                    .column_spacing(8.0)
                    .margin(Thickness::new(4.0, 0.0, 12.0, 0.0))
                    .children((
                        Border::new()
                            .width(3.0)
                            .height(16.0)
                            .corner_radius(1.5)
                            .background(mark)
                            .vertical_alignment(VerticalAlignment::Center),
                        super::tagged_icon(row, 16.0, 6.0).grid_column(1),
                        Grid::new()
                            .grid_column(2)
                            .columns(lengths(&input.cells))
                            .column_spacing(COLUMN_GAP)
                            .children(cells),
                    )),
            )
            .into()
    }
}
