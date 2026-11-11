//! What the larger dialogs hold: the command palette, the list of keyboard
//! shortcuts, and the settings.
use std::rc::Rc;

use dirfleet_core::app::{
    Appearance, Event, IconSize, PaletteItem, PaletteTarget, PaletteView, QueueMode, Settings,
    ViewModel,
};
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, ComboBox, Grid, GridLength, HorizontalAlignment,
    Orientation, ScrollBarVisibility, ScrollViewer, StackPanel, TextBlock, TextBox, TextTrimming,
    Thickness, VerticalAlignment, View,
};

use crate::core_link::CoreLink;
use crate::theme;

/// The palette: what was typed, and what it could mean. Choosing an item
/// tells `on_chosen`, which closes the palette and carries the item out.
pub fn palette(
    core: &CoreLink,
    palette: &PaletteView,
    on_chosen: &Callback<PaletteTarget>,
) -> View {
    let typed = {
        let core = core.clone();
        Callback::new(move |text: Rc<str>| {
            core.send(Event::SetPaletteText {
                text: text.to_string(),
            });
        })
    };
    let items: Vec<View> = palette
        .items
        .iter()
        .map(|item| palette_item(item, on_chosen))
        .collect();
    StackPanel::new()
        .orientation(Orientation::Vertical)
        .spacing(8.0)
        .children((
            TextBox::new()
                .text(palette.text.clone())
                .placeholder_text("A command, a folder, a saved search")
                .on_text_changed(typed),
            ScrollViewer::new()
                .height(340.0)
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                .content(
                    StackPanel::new()
                        .orientation(Orientation::Vertical)
                        .children(items),
                ),
        ))
        .into()
}

fn palette_item(item: &PaletteItem, on_chosen: &Callback<PaletteTarget>) -> View {
    let on_chosen = on_chosen.clone();
    let target = item.target.clone();
    Button::new()
        .style(ButtonStyle::Subtle)
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .horizontal_content_alignment(HorizontalAlignment::Stretch)
        .automation_name(item.title.clone())
        .content(two_columns(&item.title, &item.detail))
        .on_click(move || on_chosen.call(target.clone()))
        .into()
}

/// A name on the left and a quieter note on the right.
fn two_columns(name: &str, note: &str) -> Grid {
    Grid::new()
        .columns([GridLength::STAR, GridLength::Auto])
        .column_spacing(16.0)
        .children((
            TextBlock::new()
                .text(name)
                .text_trimming(TextTrimming::CharacterEllipsis),
            TextBlock::new()
                .text(note)
                .font_size(12.0)
                .opacity(0.6)
                .vertical_alignment(VerticalAlignment::Center)
                .grid_column(1),
        ))
}

/// Every command that has a shortcut, with it.
pub fn shortcuts(shared: &ViewModel) -> View {
    let lines: Vec<View> = shared
        .commands
        .iter()
        .filter(|command| !command.shortcut.is_empty())
        .map(|command| {
            Border::new()
                .padding(Thickness::new(4.0, 5.0, 4.0, 5.0))
                .content(two_columns(&command.label, &command.shortcut))
                .into()
        })
        .chain(shared.plugin_shortcuts.iter().map(|shortcut| {
            Border::new()
                .padding(Thickness::new(4.0, 5.0, 4.0, 5.0))
                .content(two_columns(&shortcut.title, &shortcut.text))
                .into()
        }))
        .collect();
    ScrollViewer::new()
        .height(400.0)
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .content(
            StackPanel::new()
                .orientation(Orientation::Vertical)
                .children(lines),
        )
        .into()
}

/// The settings: each change is told to the core at once.
#[expect(clippy::too_many_lines, reason = "one line per setting")]
pub fn settings(core: &CoreLink, shared: &ViewModel) -> View {
    let now = &shared.settings;
    let change = |apply: fn(&mut Settings, bool)| {
        let core = core.clone();
        let settings = now.clone();
        Callback::new(move |on: bool| {
            let mut settings = settings.clone();
            apply(&mut settings, on);
            core.send(Event::ChangeSettings { settings });
        })
    };
    let toggle = |label: &str, on: bool, apply: fn(&mut Settings, bool)| -> View {
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .children((
                TextBlock::new()
                    .text(label)
                    .vertical_alignment(VerticalAlignment::Center),
                Border::new().grid_column(1).content(
                    theme::switch(on)
                        .automation_name(label)
                        .on_toggled(change(apply)),
                ),
            ))
            .into()
    };
    let choice = |label: &str, options: &[&str], chosen: usize, apply: fn(&mut Settings, usize)| {
        let core = core.clone();
        let settings = now.clone();
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .children((
                TextBlock::new()
                    .text(label)
                    .vertical_alignment(VerticalAlignment::Center),
                Border::new().grid_column(1).content(
                    ComboBox::new()
                        .automation_name(label)
                        .items_source(options.iter().map(ToString::to_string))
                        .selected_index(chosen)
                        .on_selection_changed(move |index: Option<usize>| {
                            if let Some(index) = index.filter(|index| *index != chosen) {
                                let mut settings = settings.clone();
                                apply(&mut settings, index);
                                core.send(Event::ChangeSettings { settings });
                            }
                        }),
                ),
            ))
    };
    let heading = |text: &str| -> View {
        TextBlock::new()
            .text(text)
            .font_size(12.0)
            .opacity(0.6)
            .margin(Thickness::new(0.0, 10.0, 0.0, 0.0))
            .into()
    };
    let terminal = {
        let core = core.clone();
        let settings = now.clone();
        TextBox::new()
            .automation_name("Terminal")
            .text(now.terminal.clone())
            .placeholder_text("The program Open in Terminal starts")
            .on_text_changed(move |text: Rc<str>| {
                let mut settings = settings.clone();
                settings.terminal = text.to_string();
                core.send(Event::ChangeSettings { settings });
            })
    };
    let restore = {
        let core = core.clone();
        Button::new()
            .content("Restore Defaults")
            .on_click(move || core.send(Event::RestoreDefaultSettings))
    };
    let forget = {
        let core = core.clone();
        Button::new()
            .content("Clear Open History…")
            .on_click(move || core.send(Event::ClearOpenHistory))
    };
    let thumbnails = {
        let core = core.clone();
        Button::new()
            .content("Reset Chosen Thumbnails")
            .on_click(move || core.send(Event::ResetAllThumbnails))
    };
    let lines: Vec<View> = vec![
        heading("Appearance"),
        choice(
            "Theme",
            &["Use system setting", "Light", "Dark"],
            match now.appearance {
                Appearance::System => 0,
                Appearance::Light => 1,
                Appearance::Dark => 2,
            },
            |settings, index| {
                settings.appearance =
                    [Appearance::System, Appearance::Light, Appearance::Dark][index.min(2)];
            },
        )
        .into(),
        choice(
            "Icon size",
            &["Small", "Medium", "Large", "Extra large"],
            match now.icon_size {
                IconSize::Small => 0,
                IconSize::Medium => 1,
                IconSize::Large => 2,
                IconSize::ExtraLarge => 3,
            },
            |settings, index| {
                settings.icon_size = [
                    IconSize::Small,
                    IconSize::Medium,
                    IconSize::Large,
                    IconSize::ExtraLarge,
                ][index.min(3)];
            },
        )
        .into(),
        toggle(
            "Play the selected video or GIF",
            now.animate_selected_previews,
            |settings, on| settings.animate_selected_previews = on,
        ),
        heading("Sidebar"),
        toggle("Show recent folders", now.show_recent, |settings, on| {
            settings.show_recent = on;
        }),
        toggle(
            &format!("Show the {}", shared.trash.label),
            now.show_trash,
            |settings, on| settings.show_trash = on,
        ),
        toggle(
            "Show how full each drive is",
            now.show_drive_capacity,
            |settings, on| settings.show_drive_capacity = on,
        ),
        toggle(
            "Say when a drive is connected or disconnected",
            now.notify_drive_changes,
            |settings, on| settings.notify_drive_changes = on,
        ),
        heading("Files"),
        toggle(
            &format!("Ask before moving items to the {}", shared.trash.label),
            now.confirm_trash,
            |settings, on| settings.confirm_trash = on,
        ),
        toggle(
            "Ask whether to move or copy dropped items",
            now.confirm_drop,
            |settings, on| settings.confirm_drop = on,
        ),
        toggle(
            "Remember the files opened from here",
            now.track_open_history,
            |settings, on| settings.track_open_history = on,
        ),
        toggle(
            "Open archives as folders",
            now.browse_archives,
            |settings, on| settings.browse_archives = on,
        ),
        toggle(
            "Check each copy against its original",
            now.verify_copies,
            |settings, on| settings.verify_copies = on,
        ),
        choice(
            "Copies, moves and deletions run",
            &QueueMode::ALL.map(QueueMode::label),
            QueueMode::ALL
                .iter()
                .position(|mode| *mode == now.queue_mode)
                .unwrap_or(0),
            |settings, index| {
                settings.queue_mode = QueueMode::ALL[index.min(QueueMode::ALL.len() - 1)];
            },
        )
        .into(),
        heading("Terminal"),
        terminal.into(),
        heading("About"),
        TextBlock::new()
            .text(format!(
                "DirFleet {}  ·  {}",
                shared.access.version, shared.access.website
            ))
            .opacity(0.8)
            .into(),
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .margin(Thickness::new(0.0, 10.0, 0.0, 0.0))
            .children((restore, forget, thumbnails))
            .into(),
    ];
    let mut lines = lines;
    lines.push(heading("Plugins"));
    lines.extend(crate::plugins::settings(core, &shared.plugins));
    ScrollViewer::new()
        .height(440.0)
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .content(
            StackPanel::new()
                .orientation(Orientation::Vertical)
                .spacing(8.0)
                .margin(Thickness::new(0.0, 0.0, 16.0, 0.0))
                .children(lines),
        )
        .into()
}
