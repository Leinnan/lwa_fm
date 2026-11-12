//! What the larger dialogs hold: the command palette, the list of keyboard
//! shortcuts, and the settings.
use std::rc::Rc;

use dirfleet_core::app::{
    Appearance, CommandId, CommandView, Event, IconSize, KeymapView, PaletteItem, PaletteTarget,
    PaletteView, QueueMode, Settings, ViewModel,
};
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, Color, ComboBox, Grid, GridLength, HorizontalAlignment,
    InputModifiers, KeyEventInfo, Orientation, RoutedCallback, ScrollBarVisibility, ScrollViewer,
    StackPanel, TextBlock, TextBox, TextTrimming, TextWrapping, Thickness, VerticalAlignment, View,
};

use crate::core_link::CoreLink;
use crate::shortcuts::{Chord, Recorded, recorded};
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

/// What a command's line in the keyboard settings is in the middle of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    /// Nothing: it shows its shortcut, which can be changed, and reset when
    /// it is the user's.
    Idle { customized: bool },
    /// The next key pressed becomes its shortcut.
    Recording,
    /// The shortcut asked for is something else's: taken only on a yes.
    Asking,
}

impl Line {
    pub fn of(keymap: &KeymapView, command: &CommandView) -> Self {
        if keymap
            .conflict
            .as_ref()
            .is_some_and(|conflict| conflict.command == command.id)
        {
            Self::Asking
        } else if keymap.recording == Some(command.id) {
            Self::Recording
        } else {
            Self::Idle {
                customized: command.customized,
            }
        }
    }

    /// What the line says where its shortcut stands.
    pub fn note(&self, keymap: &KeymapView, command: &CommandView) -> String {
        match (self, &keymap.conflict) {
            (Self::Asking, Some(conflict)) => format!(
                "{} is used by \u{201c}{}\u{201d}",
                conflict.shortcut, conflict.holder
            ),
            (Self::Recording, _) => "Press the keys\u{2026}".to_owned(),
            _ if command.shortcut.is_empty() => "None".to_owned(),
            _ => command.shortcut.clone(),
        }
    }

    /// The line's two buttons: each one's label, what it tells the core,
    /// and whether it can be pressed. Always two, so the keyboard stays
    /// where it is while the line changes.
    pub const fn buttons(&self, command: &CommandView) -> [(&'static str, Event, bool); 2] {
        let command = command.id;
        match self {
            Self::Idle { customized } => [
                ("Change", Event::EditShortcut { command }, true),
                ("Reset", Event::ResetShortcut { command }, *customized),
            ],
            Self::Recording => [
                (
                    "No Shortcut",
                    Event::SetShortcut {
                        command,
                        text: String::new(),
                    },
                    true,
                ),
                ("Cancel", Event::CancelShortcut, true),
            ],
            Self::Asking => [
                ("Take It", Event::ConfirmShortcut, true),
                ("Cancel", Event::CancelShortcut, true),
            ],
        }
    }
}

/// One command's line: its name, where its shortcut stands, and the two
/// buttons that go with that.
fn line(core: &CoreLink, keymap: &KeymapView, command: &CommandView) -> View {
    let send = |event: Event| {
        let core = core.clone();
        move || core.send(event.clone())
    };
    {
        let state = Line::of(keymap, command);
        let note = TextBlock::new()
            .text(state.note(keymap, command))
            .font_size(12.0)
            .vertical_alignment(VerticalAlignment::Center);
        let note = if state == (Line::Idle { customized: false }) {
            note.opacity(0.6)
        } else {
            note
        };
        let [first, second] = state.buttons(command);
        let button = |(label, event, enabled): (&'static str, Event, bool)| {
            Button::new()
                .style(ButtonStyle::Subtle)
                .content(label)
                .is_enabled(enabled)
                .automation_name(format!("{label}: {}", command.label))
                .on_click(send(event))
        };
        Border::new()
            .padding(Thickness::new(4.0, 2.0, 4.0, 2.0))
            .content(
                Grid::new()
                    .columns([
                        GridLength::STAR,
                        GridLength::Auto,
                        GridLength::Auto,
                        GridLength::Auto,
                    ])
                    .column_spacing(8.0)
                    .children((
                        TextBlock::new()
                            .text(command.label.clone())
                            .text_trimming(TextTrimming::CharacterEllipsis)
                            .vertical_alignment(VerticalAlignment::Center),
                        note.grid_column(1),
                        button(first).grid_column(2),
                        button(second).grid_column(3),
                    )),
            )
            .into()
    }
}

/// The keys of the sheet: while a shortcut is recorded for a command, the
/// keys pressed are that shortcut's.
fn recorder(core: &CoreLink, recording: Option<CommandId>) -> RoutedCallback<KeyEventInfo> {
    let core = core.clone();
    RoutedCallback::new(move |info: KeyEventInfo| {
        let Some(command) = recording else {
            return false;
        };
        let chord = Chord {
            key: info.key.0,
            ctrl: info.modifiers.contains(InputModifiers::CONTROL),
            shift: info.modifiers.contains(InputModifiers::SHIFT),
            alt: info.modifiers.contains(InputModifiers::ALT),
        };
        match recorded(chord) {
            Recorded::Waiting => return false,
            Recorded::Cancel => core.send(Event::CancelShortcut),
            Recorded::Shortcut(text) => core.send(Event::SetShortcut { command, text }),
            Recorded::Unknown => {}
        }
        true
    })
}

/// The keyboard settings: every command under its heading with the
/// shortcut it has, which can be changed by pressing the new keys.
pub fn shortcuts(core: &CoreLink, shared: &ViewModel) -> View {
    let keymap = &shared.keymap;
    let send = |event: Event| {
        let core = core.clone();
        move || core.send(event.clone())
    };
    let heading = |text: &str| -> View {
        Border::new()
            .padding(Thickness::new(4.0, 10.0, 4.0, 2.0))
            .content(TextBlock::new().text(text).font_size(12.0).opacity(0.7))
            .into()
    };
    let line = |command: &CommandView| line(core, keymap, command);
    let mut lines: Vec<View> = Vec::new();
    // Under each heading in turn, in the order the commands come.
    let mut headings: Vec<&str> = Vec::new();
    for command in &shared.commands {
        if !headings.contains(&command.category.as_str()) {
            headings.push(&command.category);
        }
    }
    for category in headings {
        lines.push(heading(category));
        lines.extend(
            shared
                .commands
                .iter()
                .filter(|command| command.category == category)
                .map(line),
        );
    }
    if !shared.plugin_shortcuts.is_empty() {
        lines.push(heading("Plugins (changed in their settings)"));
        lines.extend(shared.plugin_shortcuts.iter().map(|shortcut| {
            Border::new()
                .padding(Thickness::new(4.0, 5.0, 4.0, 5.0))
                .content(two_columns(&shortcut.title, &shortcut.text))
                .into()
        }));
    }
    let on_key = recorder(core, keymap.recording);
    let said = TextBlock::new()
        .text(if keymap.error.is_empty() {
            "Change a shortcut, then press the keys it should have.".to_owned()
        } else {
            keymap.error.clone()
        })
        .font_size(12.0)
        .text_wrapping(TextWrapping::Wrap);
    let said = if keymap.error.is_empty() {
        said.opacity(0.7)
    } else {
        said.foreground(Color::rgb(0xE0, 0x50, 0x50))
    };
    Border::new()
        .on_preview_key_down(on_key)
        .content(
            StackPanel::new()
                .orientation(Orientation::Vertical)
                .spacing(8.0)
                .children((
                    said,
                    ScrollViewer::new()
                        .height(400.0)
                        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                        .content(
                            StackPanel::new()
                                .orientation(Orientation::Vertical)
                                .children(lines),
                        ),
                    Button::new()
                        .content("Reset All to Defaults")
                        .is_enabled(keymap.customized > 0)
                        .on_click(send(Event::ResetAllShortcuts)),
                )),
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
        toggle(
            "Add up the folders in the Size column",
            now.folder_sizes,
            |settings, on| settings.folder_sizes = on,
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

#[cfg(test)]
mod tests {
    use super::*;
    use dirfleet_core::app::{CommandId, ShortcutConflictView};

    fn command(shortcut: &str, customized: bool) -> CommandView {
        CommandView {
            id: CommandId::NewTab,
            label: "New Tab".to_owned(),
            shortcut: shortcut.to_owned(),
            customized,
            ..CommandView::default()
        }
    }

    #[test]
    fn a_shortcut_s_line_shows_it_records_it_or_asks_about_it() {
        let labels = |line: &Line, command: &CommandView| {
            line.buttons(command)
                .map(|(label, _, enabled)| (label, enabled))
        };
        // At rest: its shortcut, a way to change it, and a way back when
        // it is the user's own.
        let quiet = KeymapView::default();
        let own = command("Ctrl+T", false);
        let line = Line::of(&quiet, &own);
        assert_eq!(line, Line::Idle { customized: false });
        assert_eq!(line.note(&quiet, &own), "Ctrl+T");
        assert_eq!(labels(&line, &own), [("Change", true), ("Reset", false)]);
        assert_eq!(
            line.buttons(&own)[0].1,
            Event::EditShortcut {
                command: CommandId::NewTab
            }
        );
        let changed = command("", true);
        let line = Line::of(&quiet, &changed);
        assert_eq!(line.note(&quiet, &changed), "None");
        assert_eq!(labels(&line, &changed), [("Change", true), ("Reset", true)]);

        // Recording: the keys are waited for; it can be left with none.
        let recording = KeymapView {
            recording: Some(CommandId::NewTab),
            ..KeymapView::default()
        };
        let line = Line::of(&recording, &own);
        assert_eq!(line, Line::Recording);
        assert_eq!(line.note(&recording, &own), "Press the keys\u{2026}");
        assert_eq!(
            line.buttons(&own).map(|(label, event, _)| (label, event)),
            [
                (
                    "No Shortcut",
                    Event::SetShortcut {
                        command: CommandId::NewTab,
                        text: String::new()
                    }
                ),
                ("Cancel", Event::CancelShortcut),
            ]
        );
        // Another command's line stays at rest meanwhile.
        let other = CommandView {
            id: CommandId::Refresh,
            ..command("F5", false)
        };
        assert_eq!(
            Line::of(&recording, &other),
            Line::Idle { customized: false }
        );

        // Asked: what has the shortcut is named, and it is taken on a yes.
        let asking = KeymapView {
            conflict: Some(ShortcutConflictView {
                command: CommandId::NewTab,
                shortcut: "F5".to_owned(),
                holder: "Refresh".to_owned(),
            }),
            ..KeymapView::default()
        };
        let line = Line::of(&asking, &own);
        assert_eq!(line, Line::Asking);
        assert_eq!(
            line.note(&asking, &own),
            "F5 is used by \u{201c}Refresh\u{201d}"
        );
        assert_eq!(
            line.buttons(&own).map(|(label, event, _)| (label, event)),
            [
                ("Take It", Event::ConfirmShortcut),
                ("Cancel", Event::CancelShortcut)
            ]
        );
    }
}
