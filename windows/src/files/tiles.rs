//! One line of the icon grid: a component holding the tiles that share it.
use std::rc::Rc;

use dirfleet_core::app::{Event, RowView};
use windows_reactor::{
    Border, Callback, Component, ComponentContext, HorizontalAlignment, Image, Orientation,
    PointerEventInfo, StackPanel, Stretch, TextAlignment, TextBlock, TextTrimming, TextWrapping,
    Thickness, TooltipExt as _, VerticalAlignment, View, ViewContext,
};

use super::entry_icon;
use super::layout::Tile;
use super::press::{PressClaim, Presses};
use super::row::entry_fill;
use crate::core_link::CoreLink;

#[derive(Clone, PartialEq)]
pub struct TileLineInput {
    pub core: CoreLink,
    pub tab: u32,
    /// The folder position of the line's first tile.
    pub first: u32,
    /// The line's entries; `None` for one the core has not sent yet.
    pub entries: Vec<Option<Rc<RowView>>>,
    pub tile: Tile,
    pub claim: PressClaim,
}

pub struct TileLine {
    core: CoreLink,
    tab: u32,
    first: u32,
    /// The slot under the pointer.
    hovered: Option<u32>,
    presses: Presses,
    claim: PressClaim,
}

#[derive(Clone, Copy, Debug)]
pub enum TileMessage {
    Pressed(u32, PointerEventInfo),
    Hovered(u32, bool),
}

impl Component for TileLine {
    type Input = TileLineInput;
    type Message = TileMessage;

    fn create(input: &TileLineInput, _: &ComponentContext<Self>) -> Self {
        Self {
            core: input.core.clone(),
            tab: input.tab,
            first: input.first,
            hovered: None,
            presses: Presses::default(),
            claim: input.claim.clone(),
        }
    }

    fn input_changed(&mut self, input: &TileLineInput, _: &ComponentContext<Self>) {
        if (self.tab, self.first) != (input.tab, input.first) {
            self.presses.forget();
            self.hovered = None;
        }
        self.tab = input.tab;
        self.first = input.first;
    }

    fn update(&mut self, message: TileMessage, _: &ComponentContext<Self>) {
        match message {
            TileMessage::Pressed(slot, info) => {
                self.presses
                    .press(&self.core, self.tab, self.first + slot, &info);
            }
            TileMessage::Hovered(slot, inside) => {
                if inside {
                    self.hovered = Some(slot);
                } else if self.hovered == Some(slot) {
                    self.hovered = None;
                }
                // The core plays a video's preview once the pointer rests on it.
                self.core.send(Event::HoverRow {
                    tab: self.tab,
                    row: self.first + slot,
                    inside,
                });
            }
        }
    }

    fn view(&self, input: &TileLineInput, context: &mut ViewContext<Self>) -> View {
        let tile = input.tile;
        let tiles: Vec<View> = input
            .entries
            .iter()
            .zip(0..)
            .map(|(entry, slot)| {
                let frame = Border::new().width(tile.width).height(tile.height);
                // The tile stands in the middle of its share of the line; the
                // space around it belongs to the folder, not to the entry.
                let placed = |tile_view: View| -> View {
                    Border::new()
                        .width(tile.cell)
                        .height(tile.height)
                        .content(
                            Border::new()
                                .horizontal_alignment(HorizontalAlignment::Center)
                                .content(tile_view),
                        )
                        .into()
                };
                let Some(row) = entry else {
                    return placed(frame.into());
                };
                let sender = context.sender();
                let claim = self.claim.clone();
                let pressed = Callback::new(move |info: PointerEventInfo| {
                    claim.claim(&info);
                    _ = sender.send(TileMessage::Pressed(slot, info));
                });
                let mut lines = vec![
                    Border::new()
                        .width(tile.icon)
                        .height(tile.icon)
                        .horizontal_alignment(HorizontalAlignment::Center)
                        .content(picture(row, tile.icon))
                        .into(),
                    TextBlock::new()
                        .text(row.name.clone())
                        .font_size(12.0)
                        .text_alignment(TextAlignment::Center)
                        .text_wrapping(TextWrapping::Wrap)
                        .text_trimming(TextTrimming::CharacterEllipsis)
                        .max_lines(2)
                        .into(),
                ];
                if tile.detail {
                    let detail = if row.folder.is_empty() {
                        &row.detail
                    } else {
                        &row.folder
                    };
                    lines.push(
                        TextBlock::new()
                            .text(detail.clone())
                            .font_size(11.0)
                            .opacity(0.6)
                            .text_alignment(TextAlignment::Center)
                            .text_trimming(TextTrimming::CharacterEllipsis)
                            .into(),
                    );
                }
                let tile_view = frame
                    .padding(Thickness::new(4.0, 6.0, 4.0, 2.0))
                    .corner_radius(6.0)
                    .background(entry_fill(row.selected, self.hovered == Some(slot)))
                    .opacity(if row.cut { 0.5 } else { 1.0 })
                    .automation_name(row.name.clone())
                    .on_pointer_pressed(pressed)
                    .on_pointer_entered(
                        context
                            .callback(move |_: PointerEventInfo| TileMessage::Hovered(slot, true)),
                    )
                    .on_pointer_exited(
                        context
                            .callback(move |_: PointerEventInfo| TileMessage::Hovered(slot, false)),
                    )
                    .content(
                        StackPanel::new()
                            .orientation(Orientation::Vertical)
                            .spacing(4.0)
                            .vertical_alignment(VerticalAlignment::Top)
                            .children(lines),
                    );
                placed(if row.tooltip.is_empty() {
                    tile_view.into()
                } else {
                    tile_view.tooltip(&row.tooltip)
                })
            })
            .collect();
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .height(tile.height)
            .children(tiles)
            .into()
    }
}

/// What a tile shows of its entry: the playing preview, the thumbnail, or the icon.
fn picture(row: &RowView, size: f64) -> View {
    let file = if row.animation.is_empty() {
        &row.thumbnail
    } else {
        &row.animation
    };
    if !file.is_empty()
        && let Ok(image) = Image::new().source_file(file)
    {
        return image
            .stretch(Stretch::Uniform)
            .width(size)
            .height(size)
            .into();
    }
    entry_icon(&row.name, row.is_dir, size)
}
