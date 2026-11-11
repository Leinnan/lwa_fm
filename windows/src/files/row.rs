//! One row of the list. Each is a component of its own, so a row whose input
//! stayed the same is not drawn again, and hovering redraws that row only.
use std::rc::Rc;

use dirfleet_core::app::{RenameView, RowView};
use dirfleet_core::columns::Column;
use windows_reactor::{
    Border, Brush, Callback, Color, Component, ComponentContext, Grid, GridLength,
    HorizontalAlignment, PointerEventInfo, TextBlock, TextTrimming, ThemeBrush, Thickness,
    VerticalAlignment, View, ViewContext,
};

use super::columns::{COLUMN_GAP, Cell, lengths};
use super::entry_icon;
use super::layout::ROW_HEIGHT;
use super::press::{PressClaim, Presses};
use super::rename::{RenameBox, RenameInput};
use crate::core_link::CoreLink;

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
    /// The row is being renamed: a text box stands in for its name.
    pub rename: Option<Rc<RenameView>>,
}

pub struct Row {
    core: CoreLink,
    tab: u32,
    index: u32,
    hovered: bool,
    presses: Presses,
    on_pressed: Callback<PointerEventInfo>,
}

#[derive(Clone, Copy, Debug)]
pub enum RowMessage {
    Pressed(PointerEventInfo),
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
        Self {
            core: input.core.clone(),
            tab: input.tab,
            index: input.index,
            hovered: false,
            presses: Presses::default(),
            // The claim is made as the press is seen, before the area behind sees it.
            on_pressed: Callback::new(move |info: PointerEventInfo| {
                claim.claim(&info);
                _ = sender.send(RowMessage::Pressed(info));
            }),
        }
    }

    fn input_changed(&mut self, input: &RowInput, _: &ComponentContext<Self>) {
        if (self.tab, self.index) != (input.tab, input.index) {
            self.presses.forget();
        }
        self.tab = input.tab;
        self.index = input.index;
    }

    fn update(&mut self, message: RowMessage, _: &ComponentContext<Self>) {
        match message {
            RowMessage::Hovered(hovered) => self.hovered = hovered,
            RowMessage::Pressed(info) => {
                self.presses.press(&self.core, self.tab, self.index, &info);
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
                let text = match cell.column {
                    Column::Name => TextBlock::new().text(row.name.clone()),
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
                    .horizontal_alignment(if cell.right_aligned {
                        HorizontalAlignment::Right
                    } else {
                        HorizontalAlignment::Left
                    })
                    .grid_column(position)
                    .into()
            })
            .collect();
        let mark: Brush = if row.selected {
            ThemeBrush::Accent.into()
        } else {
            Color::transparent().into()
        };
        frame
            .corner_radius(4.0)
            .background(entry_fill(row.selected, self.hovered))
            .opacity(if row.cut { 0.5 } else { 1.0 })
            .automation_name(row.name.clone())
            .on_pointer_pressed(self.on_pressed.clone())
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
                        Border::new()
                            .grid_column(1)
                            .content(entry_icon(&row.name, row.is_dir, 16.0)),
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
