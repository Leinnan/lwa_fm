//! Keyboard shortcuts, read from the core's own table.
//!
//! The core lists every command with the shortcut its menus and palette show
//! ("Ctrl+Shift+N"). The shell binds keys by parsing those labels, so what is
//! shown and what is bound cannot drift apart.
use dirfleet_core::app::{CommandId, CommandView, PluginShortcutView};

/// A key with the modifiers held, as Windows names keys (virtual-key codes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub key: u32,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Chord {
    /// Parses a label like "Ctrl+Shift+P", "Alt+Left", "F5" or "Del".
    pub fn parse(label: &str) -> Option<Self> {
        let mut chord = Self {
            key: 0,
            ctrl: false,
            shift: false,
            alt: false,
        };
        let mut parts = label.split('+').peekable();
        while let Some(part) = parts.next() {
            // "Ctrl++" ends with an empty part for the plus key itself.
            let last = parts.peek().is_none();
            match part {
                "Ctrl" if !last => chord.ctrl = true,
                "Shift" if !last => chord.shift = true,
                "Alt" if !last => chord.alt = true,
                name if last => chord.key = key_code(name)?,
                _ => return None,
            }
        }
        (chord.key != 0).then_some(chord)
    }

    /// Whether the chord types text or edits it in a text box: such chords
    /// belong to the box while one has the keyboard.
    pub const fn edits_text(self) -> bool {
        if self.alt {
            return false;
        }
        if self.ctrl {
            // Select all, copy, cut, paste, undo, redo.
            return !self.shift && matches!(self.key, 0x41 | 0x43 | 0x56 | 0x58 | 0x59 | 0x5A);
        }
        // Letters, digits, punctuation, Space, Enter, Backspace, Delete, arrows, Home, End.
        matches!(self.key, 0x08 | 0x0D | 0x20..=0x28 | 0x2E | 0x30..=0x5A | 0xBA..=0xDF)
    }
}

/// The virtual-key code of a key as the core's labels name it.
fn key_code(name: &str) -> Option<u32> {
    let code = match name {
        "Left" => 0x25,
        "Up" => 0x26,
        "Right" => 0x27,
        "Down" => 0x28,
        "Del" | "Delete" => 0x2E,
        "Space" => 0x20,
        "Enter" | "Return" => 0x0D,
        "Backspace" => 0x08,
        "Esc" | "Escape" => 0x1B,
        "Tab" => 0x09,
        "Home" => 0x24,
        "End" => 0x23,
        "PageUp" | "PgUp" => 0x21,
        "PageDown" | "PgDn" => 0x22,
        "," => 0xBC,
        "." => 0xBE,
        "/" => 0xBF,
        ";" => 0xBA,
        "=" => 0xBB,
        "-" => 0xBD,
        "[" => 0xDB,
        "]" => 0xDD,
        "\\" => 0xDC,
        "'" => 0xDE,
        "`" => 0xC0,
        _ => {
            if let Some(number) = name.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
                return (1..=24).contains(&number).then(|| 0x70 + number - 1);
            }
            let mut characters = name.chars();
            let (Some(character), None) = (characters.next(), characters.next()) else {
                return None;
            };
            let character = character.to_ascii_uppercase();
            return (character.is_ascii_uppercase() || character.is_ascii_digit())
                .then_some(character as u32);
        }
    };
    Some(code)
}

/// The commands bound to keys.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shortcuts {
    bound: Vec<(Chord, CommandId)>,
    /// Plugins' actions (`<plugin id>.<action id>`) bound to keys.
    plugins: Vec<(Chord, String)>,
}

impl Shortcuts {
    /// Binds every command whose label names a key, and the plugins' actions
    /// that have one. A command's key is not taken by a plugin.
    pub fn new(commands: &[CommandView], plugins: &[PluginShortcutView]) -> Self {
        Self {
            plugins: plugins
                .iter()
                .filter_map(|shortcut| {
                    let chord = Chord {
                        key: key_code(&shortcut.key)?,
                        // The core's "command" key is Ctrl here.
                        ctrl: shortcut.command,
                        shift: shortcut.shift,
                        alt: shortcut.alt,
                    };
                    Some((chord, shortcut.action.clone()))
                })
                .collect(),
            bound: commands
                .iter()
                .filter_map(|command| Some((Chord::parse(&command.shortcut)?, command.id)))
                .collect(),
        }
    }

    /// The plugin action `chord` runs, if any.
    pub fn plugin(&self, chord: Chord) -> Option<&str> {
        self.plugins
            .iter()
            .find(|(bound, _)| *bound == chord)
            .map(|(_, action)| action.as_str())
    }

    /// The command `chord` runs, if any.
    pub fn command(&self, chord: Chord) -> Option<CommandId> {
        self.bound
            .iter()
            .find(|(bound, _)| *bound == chord)
            .map(|(_, command)| *command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn chord(key: u32, ctrl: bool, shift: bool, alt: bool) -> Chord {
        Chord {
            key,
            ctrl,
            shift,
            alt,
        }
    }

    #[test]
    fn labels_parse_into_chords() {
        assert_eq!(
            Chord::parse("Ctrl+Shift+P"),
            Some(chord(0x50, true, true, false))
        );
        assert_eq!(
            Chord::parse("Alt+Left"),
            Some(chord(0x25, false, false, true))
        );
        assert_eq!(Chord::parse("F5"), Some(chord(0x74, false, false, false)));
        assert_eq!(Chord::parse("Del"), Some(chord(0x2E, false, false, false)));
        assert_eq!(
            Chord::parse("Ctrl+,"),
            Some(chord(0xBC, true, false, false))
        );
        assert_eq!(
            Chord::parse("Space"),
            Some(chord(0x20, false, false, false))
        );
    }

    #[test]
    fn labels_without_a_key_are_not_chords() {
        assert_eq!(Chord::parse(""), None);
        assert_eq!(Chord::parse("Ctrl+"), None);
        assert_eq!(Chord::parse("Ctrl+Nonsense"), None);
        assert_eq!(Chord::parse("⌘N"), None);
    }

    #[test]
    fn every_shortcut_the_core_lists_can_be_bound() {
        for command in CommandId::ALL {
            let label = command.other_shortcut();
            assert!(
                label.is_empty() || Chord::parse(label).is_some(),
                "{command:?} has a shortcut the shell cannot bind: {label}"
            );
        }
    }

    #[test]
    fn typing_and_editing_chords_are_told_apart() {
        assert!(chord(0x41, true, false, false).edits_text(), "Ctrl+A");
        assert!(chord(0x20, false, false, false).edits_text(), "Space");
        assert!(chord(0x2E, false, false, false).edits_text(), "Del");
        assert!(!chord(0x4E, true, false, false).edits_text(), "Ctrl+N");
        assert!(!chord(0x43, true, true, false).edits_text(), "Ctrl+Shift+C");
        assert!(!chord(0x74, false, false, false).edits_text(), "F5");
        assert!(!chord(0x25, false, false, true).edits_text(), "Alt+Left");
    }

    #[test]
    fn a_chord_finds_its_command() {
        let shortcuts = Shortcuts::new(
            &[
                CommandView {
                    id: CommandId::NewWindow,
                    label: "New Window".to_owned(),
                    shortcut: "Ctrl+N".to_owned(),
                },
                CommandView {
                    id: CommandId::OpenTrash,
                    label: "Open Trash".to_owned(),
                    shortcut: String::new(),
                },
            ],
            &[PluginShortcutView {
                action: "sizes.measure".to_owned(),
                title: "Measure".to_owned(),
                plugin: "Sizes".to_owned(),
                text: "Ctrl+Alt+M".to_owned(),
                key: "M".to_owned(),
                command: true,
                alt: true,
                shift: false,
            }],
        );
        assert_eq!(
            shortcuts.plugin(chord(0x4D, true, false, true)),
            Some("sizes.measure")
        );
        assert_eq!(shortcuts.plugin(chord(0x4D, true, false, false)), None);
        assert_eq!(
            shortcuts.command(chord(0x4E, true, false, false)),
            Some(CommandId::NewWindow)
        );
        assert_eq!(shortcuts.command(chord(0x4E, false, false, false)), None);
    }
}
