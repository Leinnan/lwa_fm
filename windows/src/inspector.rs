//! The inspector: what is known about the selection, beside the folder.
use dirfleet_core::app::{
    CommandId, Event, InspectorView, ItemInfo, PreviewState, SelectionSummary, SizeInfo,
    TabViewModel,
};
use windows_reactor::{
    Border, Button, Callback, Grid, GridLength, HorizontalAlignment, Image, Orientation,
    ProgressRing, ScrollBarVisibility, ScrollViewer, StackPanel, Stretch, TextBlock, TextTrimming,
    TextWrapping, Thickness, VerticalAlignment, View,
};

use crate::core_link::CoreLink;
use crate::files::entry_icon;

/// The inspector's width.
pub const WIDTH: f64 = 280.0;
/// The edge of the preview at its top.
const PREVIEW: f64 = 200.0;

/// The inspector for `tab`'s selection; commands run through `on_command`.
pub fn inspector(core: &CoreLink, tab: &TabViewModel, on_command: &Callback<CommandId>) -> View {
    let content: View = match &tab.inspector {
        Some(InspectorView {
            item: Some(item), ..
        }) => one(core, tab.id, item, on_command),
        Some(InspectorView {
            summary: Some(summary),
            ..
        }) => several(summary, on_command),
        _ => TextBlock::new()
            .text("Nothing selected")
            .opacity(0.6)
            .horizontal_alignment(HorizontalAlignment::Center)
            .margin(Thickness::new(0.0, 40.0, 0.0, 0.0))
            .into(),
    };
    let mut parts = vec![content];
    parts.extend(crate::plugins::sections(&tab.plugin_sections));
    let content = StackPanel::new()
        .orientation(Orientation::Vertical)
        .spacing(14.0)
        .children(parts);
    ScrollViewer::new()
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
        .content(
            Border::new()
                .padding(Thickness::uniform(14.0))
                .content(content),
        )
        .into()
}

/// A label and its value on one line.
fn fact(label: &str, value: impl Into<View>) -> View {
    Grid::new()
        .columns([GridLength::Pixel(84.0), GridLength::STAR])
        .children((
            TextBlock::new().text(label).font_size(12.0).opacity(0.6),
            Border::new().grid_column(1).content(value),
        ))
        .into()
}

fn small(text: &str) -> TextBlock {
    TextBlock::new()
        .text(text)
        .font_size(12.0)
        .text_wrapping(TextWrapping::Wrap)
}

/// A button that runs one of the core's commands on the selection.
fn action(label: &str, command: CommandId, on_command: &Callback<CommandId>) -> View {
    let on_command = on_command.clone();
    Button::new()
        .content(label)
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .on_click(move || on_command.call(command))
        .into()
}

/// One selected item: its preview, name, and what is known of it.
fn one(core: &CoreLink, tab: u32, item: &ItemInfo, on_command: &Callback<CommandId>) -> View {
    let size: View = match &item.size {
        SizeInfo::Known(size) => small(size).into(),
        SizeInfo::Unavailable => small("Unavailable").into(),
        SizeInfo::Calculating => ProgressRing::new()
            .is_indeterminate(true)
            .is_active(true)
            .width(14.0)
            .height(14.0)
            .horizontal_alignment(HorizontalAlignment::Left)
            .into(),
        SizeInfo::NotCalculated => {
            let core = core.clone();
            let path = item.path.clone();
            Button::new()
                .content("Calculate")
                .on_click(move || core.send(Event::CalculateSize { path: path.clone() }))
                .into()
        }
    };
    let open = {
        let core = core.clone();
        Button::new()
            .content("Open")
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .on_click(move || core.send(Event::OpenSelection { tab }))
    };
    let mut lines: Vec<View> = vec![
        preview(core, item),
        TextBlock::new()
            .text(item.name.clone())
            .font_size(15.0)
            .text_wrapping(TextWrapping::Wrap)
            .is_text_selection_enabled(true)
            .into(),
        small(&item.path)
            .opacity(0.6)
            .is_text_selection_enabled(true)
            .into(),
        fact("Type", small(&item.kind)),
        fact("Size", size),
        fact("Created", small(&item.created)),
        fact("Modified", small(&item.modified)),
    ];
    if !item.opened.is_empty() {
        lines.push(fact("Opened", small(&item.opened)));
    }
    if item.opens > 0 {
        lines.push(fact("Opens", small(&item.opens.to_string())));
    }
    lines.extend([
        open.into(),
        action("Copy as Path", CommandId::CopyAsPath, on_command),
        action("Show in Explorer", CommandId::ShowInFinder, on_command),
    ]);
    StackPanel::new()
        .orientation(Orientation::Vertical)
        .spacing(8.0)
        .children(lines)
        .into()
}

/// The item's picture: its thumbnail when the core has one, its icon otherwise.
fn preview(core: &CoreLink, item: &ItemInfo) -> View {
    let picture: View = match &item.preview {
        PreviewState::Ready(file) => {
            let file = if item.animation.is_empty() {
                file
            } else {
                &item.animation
            };
            Image::new().source_file(file).map_or_else(
                |_| entry_icon(&item.name, item.is_dir, 96.0),
                |image| image.stretch(Stretch::Uniform).into(),
            )
        }
        PreviewState::Loading => ProgressRing::new()
            .is_indeterminate(true)
            .is_active(true)
            .width(28.0)
            .height(28.0)
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
        PreviewState::Unavailable => {
            let core = core.clone();
            let path = item.path.clone();
            StackPanel::new()
                .orientation(Orientation::Vertical)
                .spacing(8.0)
                .vertical_alignment(VerticalAlignment::Center)
                .horizontal_alignment(HorizontalAlignment::Center)
                .children((
                    small("No preview").opacity(0.6),
                    Button::new()
                        .content("Try Again")
                        .on_click(move || core.send(Event::RetryPreview { path: path.clone() })),
                ))
                .into()
        }
        PreviewState::None => entry_icon(&item.name, item.is_dir, 96.0),
    };
    Border::new()
        .height(PREVIEW)
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .content(picture)
        .into()
}

/// Several selected items: how many, of what kinds, and what they come to.
fn several(summary: &SelectionSummary, on_command: &Callback<CommandId>) -> View {
    let mut lines: Vec<View> = vec![
        TextBlock::new()
            .text(format!("{} items", summary.count))
            .font_size(15.0)
            .into(),
        fact("Folders", small(&summary.folders.to_string())),
        fact("Files", small(&summary.files.to_string())),
        fact("Size", small(&summary.total)),
    ];
    lines.extend(summary.kinds.iter().map(|kind| {
        fact(
            &kind.count.to_string(),
            small(&kind.kind).text_trimming(TextTrimming::CharacterEllipsis),
        )
    }));
    lines.extend([
        action("Copy", CommandId::Copy, on_command),
        action("Duplicate", CommandId::Duplicate, on_command),
        action("Move to Recycle Bin", CommandId::MoveToTrash, on_command),
    ]);
    StackPanel::new()
        .orientation(Orientation::Vertical)
        .spacing(8.0)
        .children(lines)
        .into()
}
