//! The keymap: which keys run which command.
//!
//! Every command has the shortcut the app gives it, or none; the user may
//! give it another, or take it away. A shortcut something else has is
//! taken only after a question, and then leaves its holder: a command or a
//! plugin's action. Keys that move through lists, and the system's own
//! combinations, are refused with the reason.
use std::collections::BTreeMap;

use crux_core::Command;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::{CommandId, DirFleet, Effect, Event, Model};
use crate::shortcut::{NAVIGATION_KEYS, Shortcut, parse_command};

/// A shortcut's key and modifiers, for shells to bind without reading the
/// text shown.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the modifier keys, one flag each"
)]
pub struct KeyView {
    /// egui's name for the key ("M", "F5", "Up", "Comma").
    pub key: String,
    /// Cmd on macOS, Ctrl elsewhere.
    pub command: bool,
    /// The Mac's Control key.
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
}

impl From<Shortcut> for KeyView {
    fn from(shortcut: Shortcut) -> Self {
        Self {
            key: shortcut.key.to_owned(),
            command: shortcut.command,
            control: shortcut.control,
            alt: shortcut.alt,
            shift: shortcut.shift,
        }
    }
}

/// A shortcut asked for that something else has: it is given only once
/// `ConfirmShortcut` says so, and then leaves its holder.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ShortcutConflictView {
    /// The command the shortcut is asked for.
    pub command: CommandId,
    /// The shortcut, as the app shows shortcuts.
    pub shortcut: String,
    /// What has it now: a command's name, or a plugin action's.
    pub holder: String,
}

/// The state of the keyboard settings.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct KeymapView {
    /// The command whose shortcut is being recorded: the next key pressed
    /// goes to `SetShortcut`.
    pub recording: Option<CommandId>,
    /// Why the shortcut last asked for was refused; "" when it was not.
    pub error: String,
    pub conflict: Option<ShortcutConflictView>,
    /// How many commands have a shortcut of the user's.
    pub customized: u32,
}

/// What has a shortcut that is asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Holder {
    Command(CommandId),
    /// A plugin's action, `<plugin id>.<action id>`, and its title.
    Plugin(String, String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Pending {
    command: CommandId,
    shortcut: Shortcut,
    holder: Holder,
}

/// The shortcuts the user chose, and what the settings are in the middle of.
#[derive(Default)]
pub(super) struct Keymap {
    /// By the command's name, as [`Shortcut::neutral_text`] writes
    /// shortcuts; "" for none.
    overrides: BTreeMap<String, String>,
    recording: Option<CommandId>,
    error: String,
    pending: Option<Pending>,
}

fn name_of(command: CommandId) -> String {
    format!("{command:?}")
}

/// Why `shortcut` can't be a command's, on a Mac (`mac`) or elsewhere;
/// `None` when it can.
fn refused(shortcut: &Shortcut, mac: bool) -> Option<String> {
    let shown = if mac {
        shortcut.mac_text()
    } else {
        shortcut.other_text()
    };
    let key = shortcut.key;
    if shortcut.is_plain() && NAVIGATION_KEYS.contains(&key) {
        return Some(format!(
            "{shown} moves through lists and dialogs, and stays that way"
        ));
    }
    let only = |command: bool, control: bool, alt: bool| {
        shortcut.command == command && shortcut.control == control && shortcut.alt == alt
    };
    let system = if mac {
        // Quit, Hide, the app switcher, Spotlight, Force Quit.
        (only(true, false, false) && ["Q", "H", "Tab", "Space"].contains(&key))
            || (only(true, false, true) && key == "Escape")
    } else {
        shortcut.control
            // Close, the window switcher, the window's menu.
            || (only(false, false, true) && ["F4", "Tab", "Space", "Escape"].contains(&key))
            // The Start menu, the security screen.
            || (only(true, false, false) && key == "Escape")
            || (only(true, false, true) && key == "Delete")
    };
    if system {
        return Some(format!("{shown} is the system's own"));
    }
    // Enter opens what is selected, wherever it is not the Mac's rename;
    // Backspace goes up a folder there.
    if !mac && shortcut.is_plain() {
        match key {
            "Enter" => return Some(format!("{shown} opens what is selected")),
            "Backspace" => return Some(format!("{shown} goes to the folder above")),
            _ => {}
        }
    }
    None
}

impl Keymap {
    /// The shortcut `command` has now: the user's, or the app's own.
    pub(super) fn shortcut(&self, command: CommandId) -> Option<Shortcut> {
        match self.overrides.get(&name_of(command)) {
            Some(text) if text.is_empty() => None,
            Some(text) => parse_command(text).ok(),
            None => command.default_shortcut(),
        }
    }

    /// That shortcut as the app shows shortcuts; "" for none.
    pub(super) fn shown(&self, command: CommandId) -> String {
        self.shortcut(command)
            .map(|shortcut| shortcut.display())
            .unwrap_or_default()
    }

    pub(super) fn customized(&self, command: CommandId) -> bool {
        self.overrides.contains_key(&name_of(command))
    }

    /// Every shortcut a command has, with the command.
    pub(super) fn taken(&self) -> Vec<(Shortcut, CommandId)> {
        CommandId::ALL
            .into_iter()
            .filter_map(|command| Some((self.shortcut(command)?, command)))
            .collect()
    }

    /// The saved shortcuts, to keep; and the kept ones, taken back: only
    /// those of commands there are, that still read as shortcuts.
    pub(super) fn saved(&self) -> BTreeMap<String, String> {
        self.overrides.clone()
    }

    pub(super) fn load(&mut self, saved: BTreeMap<String, String>) {
        let known: Vec<String> = CommandId::ALL.into_iter().map(name_of).collect();
        self.overrides = saved
            .into_iter()
            .filter(|(name, text)| {
                known.contains(name) && (text.is_empty() || parse_command(text).is_ok())
            })
            .collect();
    }

    /// Gives `command` the shortcut, or none: kept only when it is not
    /// what the app gives it anyway.
    fn give(&mut self, command: CommandId, shortcut: Option<Shortcut>) {
        if shortcut == command.default_shortcut() {
            self.overrides.remove(&name_of(command));
        } else {
            self.overrides.insert(
                name_of(command),
                shortcut
                    .map(|shortcut| shortcut.neutral_text())
                    .unwrap_or_default(),
            );
        }
    }

    pub(super) fn view(&self) -> KeymapView {
        KeymapView {
            recording: self.recording,
            error: self.error.clone(),
            conflict: self.pending.as_ref().map(|pending| ShortcutConflictView {
                command: pending.command,
                shortcut: pending.shortcut.display(),
                holder: match &pending.holder {
                    Holder::Command(command) => command.label().to_owned(),
                    Holder::Plugin(_, title) => title.clone(),
                },
            }),
            customized: u32::try_from(self.overrides.len()).unwrap_or(u32::MAX),
        }
    }
}

impl DirFleet {
    /// The events of the keyboard settings.
    pub(super) fn keymap_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        let keymap = &mut model.keymap;
        match event {
            Event::EditShortcut { command } => {
                keymap.recording = Some(command);
                keymap.pending = None;
                keymap.error.clear();
                render()
            }
            Event::SetShortcut { command, text } => {
                keymap.pending = None;
                if text.trim().is_empty() {
                    return Self::bind(model, command, None);
                }
                match parse_command(&text) {
                    Ok(shortcut) => Self::bind(model, command, Some(shortcut)),
                    Err(error) => {
                        // Still recording: another key may do.
                        keymap.error = error;
                        render()
                    }
                }
            }
            Event::ResetShortcut { command } => {
                keymap.pending = None;
                Self::bind(model, command, command.default_shortcut())
            }
            Event::ResetAllShortcuts => {
                *keymap = Keymap::default();
                Self::keymap_changed(model)
            }
            Event::ConfirmShortcut => {
                let Some(pending) = keymap.pending.take() else {
                    return Command::done();
                };
                // Its holder lets go of it.
                let freed = match pending.holder {
                    Holder::Command(holder) => {
                        keymap.give(holder, None);
                        Command::done()
                    }
                    Holder::Plugin(action, _) => match action.rsplit_once('.') {
                        Some((plugin, action)) => Self::plugin_event(
                            model,
                            Event::SetPluginShortcut {
                                plugin: plugin.to_owned(),
                                action: action.to_owned(),
                                text: String::new(),
                            },
                        ),
                        None => Command::done(),
                    },
                };
                model.keymap.give(pending.command, Some(pending.shortcut));
                freed.and(Self::keymap_changed(model))
            }
            Event::CancelShortcut => {
                keymap.recording = None;
                keymap.pending = None;
                keymap.error.clear();
                render()
            }
            _ => Command::done(),
        }
    }

    /// Gives `command` the shortcut, or none; refuses what can't be taken,
    /// and asks first when something else has it.
    fn bind(
        model: &mut Model,
        command: CommandId,
        shortcut: Option<Shortcut>,
    ) -> Command<Effect, Event> {
        if let Some(shortcut) = shortcut {
            if let Some(reason) = refused(&shortcut, cfg!(target_os = "macos")) {
                model.keymap.error = reason;
                return render();
            }
            let holder = model
                .keymap
                .taken()
                .into_iter()
                .find(|(taken, holder)| *taken == shortcut && *holder != command)
                .map(|(_, holder)| Holder::Command(holder))
                .or_else(|| {
                    model
                        .plugins
                        .bindings()
                        .0
                        .into_iter()
                        .find(|binding| binding.holds(&shortcut))
                        .map(|binding| Holder::Plugin(binding.action, binding.title))
                });
            if let Some(holder) = holder {
                model.keymap.recording = None;
                model.keymap.error.clear();
                model.keymap.pending = Some(Pending {
                    command,
                    shortcut,
                    holder,
                });
                return render();
            }
        }
        model.keymap.give(command, shortcut);
        Self::keymap_changed(model)
    }

    /// After shortcuts changed: nothing is being recorded or asked, and
    /// plugins' shortcuts are checked against the commands' new ones.
    pub(super) fn keymap_changed(model: &mut Model) -> Command<Effect, Event> {
        model.keymap.recording = None;
        model.keymap.error.clear();
        model.plugins.set_command_shortcuts(model.keymap.taken());
        render()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shortcut::parse_shown;

    #[test]
    fn every_command_s_shortcut_reads_back_as_it_is_shown() {
        // The shortcuts the app has always shown are what the keymap starts
        // from: each is read, and written again the same.
        for command in CommandId::ALL {
            for (shown, mac) in [
                (command.mac_shortcut(), true),
                (command.other_shortcut(), false),
            ] {
                if shown.is_empty() {
                    assert_eq!(parse_shown(shown), None);
                    continue;
                }
                let shortcut = parse_shown(shown)
                    .unwrap_or_else(|| panic!("{command:?}: {shown} is no shortcut"));
                let written = if mac {
                    shortcut.mac_text()
                } else {
                    shortcut.other_text()
                };
                assert_eq!(written, shown, "{command:?}");
                // What the app itself gives is never refused, and is kept
                // in a form that reads back.
                assert_eq!(refused(&shortcut, mac), None, "{command:?}: {shown}");
                assert_eq!(parse_command(&shortcut.neutral_text()), Ok(shortcut));
            }
        }
        // A few, by name: they are what users know.
        let shown = |command: CommandId| (command.mac_shortcut(), command.other_shortcut());
        assert_eq!(shown(CommandId::Refresh), ("⌘R", "F5"));
        assert_eq!(shown(CommandId::MoveToTrash), ("⌘⌫", "Del"));
        assert_eq!(shown(CommandId::Rename), ("↩", "F2"));
        assert_eq!(shown(CommandId::ToggleSidebar), ("⌃⌘S", "Ctrl+Alt+S"));
        assert_eq!(shown(CommandId::Palette), ("⇧⌘P", "Ctrl+Shift+P"));
    }

    #[test]
    fn keys_that_move_and_the_system_s_own_are_refused() {
        let reason = |text: &str, mac| {
            let shortcut = parse_command(text).expect("a shortcut");
            refused(&shortcut, mac)
        };
        assert_eq!(
            reason("Down", false).as_deref(),
            Some("Down moves through lists and dialogs, and stays that way")
        );
        assert_eq!(
            reason("Shift+Tab", false).as_deref(),
            Some("Shift+Tab moves through lists and dialogs, and stays that way")
        );
        assert_eq!(reason("Alt+Down", false), None, "with Alt it moves nothing");
        assert_eq!(
            reason("Alt+F4", false).as_deref(),
            Some("Alt+F4 is the system's own")
        );
        assert_eq!(
            reason("Ctrl+Alt+Delete", false).as_deref(),
            Some("Ctrl+Alt+Del is the system's own")
        );
        assert_eq!(
            reason("Enter", false).as_deref(),
            Some("Enter opens what is selected")
        );
        assert_eq!(
            reason("Backspace", false).as_deref(),
            Some("Backspace goes to the folder above")
        );
        assert_eq!(reason("Ctrl+Backspace", false), None);
        assert_eq!(reason("Ctrl+Q", false), None);
        assert_eq!(
            reason("Cmd+Q", true).as_deref(),
            Some("⌘Q is the system's own")
        );
        assert_eq!(
            reason("Cmd+Space", true).as_deref(),
            Some("⌘Space is the system's own")
        );
        assert_eq!(reason("Enter", true), None, "the Mac renames with it");
        assert_eq!(reason("Alt+F4", true), None);
    }
}
