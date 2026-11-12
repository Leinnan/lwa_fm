//! One line of the icon grid: a component holding the tiles that share it.
use std::rc::Rc;

use dirfleet_core::app::{Event, RowView};
use windows_reactor::{
    Border, Callback, Component, ComponentContext, Grid, HorizontalAlignment, Image, Orientation,
    PointerEventInfo, StackPanel, Stretch, TextAlignment, TextBlock, TextTrimming, TextWrapping,
    Thickness, TooltipExt as _, VerticalAlignment, View, ViewContext,
};

use super::entry_icon;
use super::layout::Tile;
use super::press::{DragWatch, EntryHooks, PressClaim, Presses};
use super::row::entry_fill;
use crate::core_link::CoreLink;

/// The heading of the group that starts on a line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heading {
    /// The group's place among the tab's groups.
    pub group: u32,
    pub label: String,
    pub count: u32,
}

#[derive(Clone, PartialEq)]
pub struct TileLineInput {
    pub core: CoreLink,
    pub tab: u32,
    /// The heading that takes the line's first cell, when a group starts here.
    pub heading: Option<Heading>,
    /// The folder position of the line's first tile.
    pub first: u32,
    /// The line's entries; `None` for one the core has not sent yet.
    pub entries: Vec<Option<Rc<RowView>>>,
    pub tile: Tile,
    pub claim: PressClaim,
    pub hooks: EntryHooks,
}

pub struct TileLine {
    core: CoreLink,
    tab: u32,
    first: u32,
    /// The slot under the pointer.
    hovered: Option<u32>,
    presses: Presses,
    claim: PressClaim,
    /// Which of the line's slots hold a selected entry.
    selected: Vec<bool>,
    /// The click of a press on a selected tile, sent when the button goes up
    /// unless the press became a drag.
    held: Option<Event>,
    watch: DragWatch,
    on_drag: Callback<u32>,
}

#[derive(Clone, Copy, Debug)]
pub enum TileMessage {
    Pressed(u32, PointerEventInfo),
    Released,
    /// The press on this slot has moved far enough to drag its tile.
    Dragged(u32),
    Hovered(u32, bool),
}

/// Which slots of a line hold a selected entry.
fn selected(entries: &[Option<Rc<RowView>>]) -> Vec<bool> {
    entries
        .iter()
        .map(|entry| entry.as_ref().is_some_and(|row| row.selected))
        .collect()
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
            selected: selected(&input.entries),
            held: None,
            watch: DragWatch::default(),
            on_drag: input.hooks.on_drag.clone(),
        }
    }

    fn input_changed(&mut self, input: &TileLineInput, _: &ComponentContext<Self>) {
        if (self.tab, self.first) != (input.tab, input.first) {
            self.presses.forget();
            self.hovered = None;
            self.held = None;
        }
        self.tab = input.tab;
        self.first = input.first;
        self.selected = selected(&input.entries);
        self.on_drag = input.hooks.on_drag.clone();
    }

    fn update(&mut self, message: TileMessage, _: &ComponentContext<Self>) {
        match message {
            TileMessage::Pressed(slot, info) => {
                let selected = self.selected.get(slot as usize).copied().unwrap_or(false);
                self.held =
                    self.presses
                        .press(&self.core, self.tab, self.first + slot, &info, selected);
            }
            TileMessage::Released => {
                if let Some(click) = self.held.take() {
                    self.core.send(click);
                }
            }
            TileMessage::Dragged(slot) => {
                self.held = None;
                self.on_drag.call(self.first + slot);
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

    #[expect(clippy::too_many_lines, reason = "a tile, top to bottom")]
    fn view(&self, input: &TileLineInput, context: &mut ViewContext<Self>) -> View {
        let tile = input.tile;
        let heading = input
            .heading
            .as_ref()
            .map(|heading| heading_cell(&self.core, self.tab, heading, tile, &self.claim));
        let tiles: Vec<View> =
            input
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
                    let watch = self.watch.clone();
                    let pressed = Callback::new(move |info: PointerEventInfo| {
                        claim.claim(&info);
                        watch.pressed(slot, &info);
                        _ = sender.send(TileMessage::Pressed(slot, info));
                    });
                    let (watch, dragged) = (self.watch.clone(), context.sender());
                    let moved = move |info: PointerEventInfo| {
                        if let Some(slot) = watch.moved(&info) {
                            _ = dragged.send(TileMessage::Dragged(slot));
                        }
                    };
                    let (watch, let_go) = (self.watch.clone(), context.sender());
                    let released = move |_: PointerEventInfo| {
                        watch.released();
                        _ = let_go.send(TileMessage::Released);
                    };
                    // Files can be dropped on a folder.
                    let frame = input
                        .hooks
                        .taking_drops(frame, row.is_dir, input.first + slot);
                    let mut lines = vec![
                        Border::new()
                            .width(tile.icon)
                            .height(tile.icon)
                            .horizontal_alignment(HorizontalAlignment::Center)
                            // The picture, with the entry's tags at its foot.
                            .content(Grid::new().children((
                                picture(row, tile.icon),
                                super::tag_dots(&row.tags, 9.0),
                            )))
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
                    let tile_view =
                        frame
                            .padding(Thickness::new(4.0, 6.0, 4.0, 2.0))
                            .corner_radius(6.0)
                            .background(entry_fill(row.selected, self.hovered == Some(slot)))
                            .opacity(if row.cut { 0.5 } else { 1.0 })
                            .automation_name(row.name.clone())
                            .on_pointer_pressed(pressed)
                            .on_pointer_moved(moved)
                            .on_pointer_released(released)
                            .on_pointer_entered(context.callback(move |_: PointerEventInfo| {
                                TileMessage::Hovered(slot, true)
                            }))
                            .on_pointer_exited(context.callback(move |_: PointerEventInfo| {
                                TileMessage::Hovered(slot, false)
                            }))
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
            .children(heading.into_iter().chain(tiles).collect::<Vec<View>>())
            .into()
    }
}

/// The cell a group's heading takes at the start of the group's first line:
/// its name and how many it holds, beside a rule. A press selects the group.
fn heading_cell(
    core: &CoreLink,
    tab: u32,
    heading: &Heading,
    tile: Tile,
    claim: &PressClaim,
) -> View {
    let (core, claim, group) = (core.clone(), claim.clone(), heading.group);
    Border::new()
        .width(tile.cell)
        .height(tile.height)
        .padding(Thickness::new(10.0, 6.0, 10.0, 6.0))
        // Transparent, not absent, so the whole cell takes the pointer.
        .background(windows_reactor::Color::transparent())
        .border_brush(crate::theme::stroke())
        .border_thickness(Thickness::new(0.0, 0.0, 1.0, 0.0))
        .automation_name(format!("{}, {}", heading.label, heading.count))
        .on_pointer_pressed(move |info: PointerEventInfo| {
            // Not a press on empty space, which would clear the selection.
            claim.claim(&info);
            if info.is_left_button_pressed {
                core.send(Event::SelectGroup { tab, group });
            }
        })
        .content(
            StackPanel::new()
                .orientation(Orientation::Vertical)
                .spacing(2.0)
                .vertical_alignment(VerticalAlignment::Center)
                .children((
                    TextBlock::new()
                        .text(heading.label.clone())
                        .font_size(13.0)
                        .text_wrapping(TextWrapping::Wrap)
                        .text_trimming(TextTrimming::CharacterEllipsis)
                        .max_lines(3),
                    TextBlock::new()
                        .text(heading.count.to_string())
                        .font_size(11.0)
                        .opacity(0.6),
                )),
        )
        .into()
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
