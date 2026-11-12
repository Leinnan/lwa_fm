//! The inspector: what is known about the selection, beside the folder.
use std::rc::Rc;

use dirfleet_core::app::{
    ChecksumRow, ChecksumVerdict, ChecksumsView, CommandId, Event, InspectorView, ItemInfo,
    PreviewState, SelectionSummary, SizeInfo, TabViewModel,
};
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, Grid, GridLength, HorizontalAlignment, Image,
    Orientation, ProgressBar, ProgressRing, ScrollBarVisibility, ScrollViewer, StackPanel, Stretch,
    TextBlock, TextBox, TextTrimming, TextWrapping, Thickness, VerticalAlignment, View,
};

use crate::core_link::CoreLink;
use crate::files::{entry_icon, glyph};

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

/// What a picture, a video or a sound says of itself, as labelled lines;
/// none for other files and until it has been read.
fn media_facts(item: &ItemInfo) -> Vec<(&'static str, &str)> {
    [
        ("Dimensions", item.dimensions.as_str()),
        ("Duration", item.duration.as_str()),
    ]
    .into_iter()
    .filter(|(_, value)| !value.is_empty())
    .collect()
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
    ];
    lines.extend(
        media_facts(item)
            .into_iter()
            .map(|(label, value)| fact(label, small(value))),
    );
    lines.extend([
        fact("Created", small(&item.created)),
        fact("Modified", small(&item.modified)),
    ]);
    if !item.opened.is_empty() {
        lines.push(fact("Opened", small(&item.opened)));
    }
    if item.opens > 0 {
        lines.push(fact("Opens", small(&item.opens.to_string())));
    }
    if let Some(sums) = &item.checksums {
        lines.extend(checksums(core, &item.path, sums));
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

/// What the comparison with a checksum came to, marked so a match and a
/// mismatch are told apart at a glance; nothing when there is nothing to say.
fn verdict_line(sums: &ChecksumsView) -> Option<String> {
    if sums.message.is_empty() {
        return None;
    }
    Some(match sums.verdict {
        ChecksumVerdict::Match => format!("\u{2713} {}", sums.message),
        ChecksumVerdict::Mismatch => format!("\u{2717} {}", sums.message),
        ChecksumVerdict::None | ChecksumVerdict::Checking | ChecksumVerdict::Unknown => {
            sums.message.clone()
        }
    })
}

/// One checksum's value: the checksum with a button that copies it, a
/// spinner while the file is read, or a button that works it out.
fn checksum_value(core: &CoreLink, path: &str, row: &ChecksumRow) -> View {
    let algorithm = row.algorithm;
    let (core, path) = (core.clone(), path.to_owned());
    if !row.value.is_empty() {
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .column_spacing(4.0)
            .children((
                small(&row.value).is_text_selection_enabled(true),
                Border::new().grid_column(1).content(
                    Button::new()
                        .style(ButtonStyle::Subtle)
                        .automation_name(format!("Copy {}", row.label))
                        .content(glyph("\u{E8C8}", 12.0))
                        .on_click(move || {
                            core.send(Event::CopyChecksum {
                                path: path.clone(),
                                algorithm,
                            });
                        }),
                ),
            ))
            .into()
    } else if row.computing {
        ProgressRing::new()
            .is_indeterminate(true)
            .is_active(true)
            .width(14.0)
            .height(14.0)
            .horizontal_alignment(HorizontalAlignment::Left)
            .into()
    } else {
        Button::new()
            .automation_name(format!("Compute {}", row.label))
            .content("Compute")
            .on_click(move || {
                core.send(Event::ComputeChecksum {
                    path: path.clone(),
                    algorithm,
                });
            })
            .into()
    }
}

/// A file's checksums: each worked out on request and copied, and the file
/// compared with a checksum pasted here or held by a checksum file beside it.
fn checksums(core: &CoreLink, path: &str, sums: &ChecksumsView) -> Vec<View> {
    let mut lines: Vec<View> = vec![small("Checksums").opacity(0.6).into()];
    for row in &sums.rows {
        lines.push(fact(&row.label, checksum_value(core, path, row)));
    }
    if let Some(progress) = sums.progress {
        let (core, path) = (core.clone(), path.to_owned());
        lines.push(
            Grid::new()
                .columns([GridLength::STAR, GridLength::Auto])
                .column_spacing(8.0)
                .children((
                    ProgressBar::new()
                        .minimum(0.0)
                        .maximum(1000.0)
                        .value(f64::from(progress))
                        .vertical_alignment(VerticalAlignment::Center),
                    Border::new().grid_column(1).content(
                        Button::new()
                            .automation_name("Stop reading the file")
                            .content("Stop")
                            .on_click(move || {
                                core.send(Event::CancelChecksum { path: path.clone() });
                            }),
                    ),
                ))
                .into(),
        );
    }
    {
        let (core, path) = (core.clone(), path.to_owned());
        lines.push(
            TextBox::new()
                .automation_name("Checksum to compare with")
                .placeholder_text("Paste a checksum to compare")
                .on_text_changed(move |text: Rc<str>| {
                    core.send(Event::VerifyChecksum {
                        path: path.clone(),
                        expected: text.to_string(),
                    });
                })
                .into(),
        );
    }
    if let Some(line) = verdict_line(sums) {
        lines.push(small(&line).into());
    }
    for sidecar in &sums.sidecars {
        let (core, path, sidecar) = (core.clone(), path.to_owned(), sidecar.clone());
        lines.push(
            Button::new()
                .content(format!("Compare with {sidecar}"))
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .on_click(move || {
                    core.send(Event::VerifySidecar {
                        path: path.clone(),
                        sidecar: sidecar.clone(),
                    });
                })
                .into(),
        );
    }
    lines
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_media_file_s_pixels_and_playing_time_are_listed_once_known() {
        let mut item = ItemInfo {
            name: "clip.mp4".into(),
            path: "C:\\clip.mp4".into(),
            kind: "MP4 video".into(),
            is_dir: false,
            size: SizeInfo::Known("1 MB".into()),
            created: String::new(),
            modified: String::new(),
            opened: String::new(),
            opens: 0,
            preview: PreviewState::None,
            animation: String::new(),
            checksums: None,
            dimensions: String::new(),
            duration: String::new(),
        };
        assert!(media_facts(&item).is_empty(), "nothing read yet");
        item.dimensions = "1920 \u{d7} 1080".into();
        item.duration = "3:07".into();
        assert_eq!(
            media_facts(&item),
            [("Dimensions", "1920 \u{d7} 1080"), ("Duration", "3:07")]
        );
        // A sound has no picture.
        item.dimensions.clear();
        assert_eq!(media_facts(&item), [("Duration", "3:07")]);
    }

    #[test]
    fn a_checksum_s_verdict_is_marked_as_a_match_or_not() {
        let mut sums = ChecksumsView {
            rows: Vec::new(),
            progress: None,
            expected: String::new(),
            verdict: ChecksumVerdict::None,
            message: String::new(),
            sidecars: Vec::new(),
        };
        assert_eq!(verdict_line(&sums), None);
        sums.message = "Matches the file's SHA-256".into();
        sums.verdict = ChecksumVerdict::Match;
        assert_eq!(
            verdict_line(&sums).as_deref(),
            Some("\u{2713} Matches the file's SHA-256")
        );
        sums.message = "Does not match the file's MD5".into();
        sums.verdict = ChecksumVerdict::Mismatch;
        assert_eq!(
            verdict_line(&sums).as_deref(),
            Some("\u{2717} Does not match the file's MD5")
        );
        sums.message = "Reading the file\u{2026}".into();
        sums.verdict = ChecksumVerdict::Checking;
        assert_eq!(
            verdict_line(&sums).as_deref(),
            Some("Reading the file\u{2026}")
        );
    }
}
