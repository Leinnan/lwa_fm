//! Keyboard shortcuts, read from the core's own table.
//!
//! The core lists every command with the shortcut it has now, the app's
//! own or the user's: as the text its menus and palette show
//! ("Ctrl+Shift+N"), and as a key with its modifiers. The shell binds the
//! key, and records new shortcuts by naming keys as the core does.
use dirfleet_core::app::{CommandId, CommandView, KeyView, PluginShortcutView};

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

    /// The chord of a key and modifiers as the core names them. The core's
    /// "command" key is Ctrl here; the Mac's Control key is no key here.
    pub fn of(key: &KeyView) -> Option<Self> {
        Some(Self {
            key: key_code(&key.key)?,
            ctrl: key.command,
            shift: key.shift,
            alt: key.alt,
        })
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

/// The keys with a name of their own, as the core names them, each with
/// its virtual-key code.
const NAMED_KEYS: [(&str, u32); 31] = [
    ("Left", 0x25),
    ("Up", 0x26),
    ("Right", 0x27),
    ("Down", 0x28),
    ("Delete", 0x2E),
    ("Insert", 0x2D),
    ("Space", 0x20),
    ("Enter", 0x0D),
    ("Backspace", 0x08),
    ("Escape", 0x1B),
    ("Tab", 0x09),
    ("Home", 0x24),
    ("End", 0x23),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("Comma", 0xBC),
    ("Period", 0xBE),
    ("Slash", 0xBF),
    ("Semicolon", 0xBA),
    ("Equals", 0xBB),
    ("Minus", 0xBD),
    ("OpenBracket", 0xDB),
    ("CloseBracket", 0xDD),
    ("Backslash", 0xDC),
    ("Quote", 0xDE),
    ("Backtick", 0xC0),
    ("Pause", 0x13),
    ("PrintScreen", 0x2C),
    ("Menu", 0x5D),
    ("Help", 0x2F),
    ("Clear", 0x0C),
];

/// The core's name for the key with this virtual-key code: what a
/// recorded shortcut is written with. `None` for keys no shortcut uses.
pub fn key_name(code: u32) -> Option<String> {
    if let Some((name, _)) = NAMED_KEYS.iter().find(|(_, named)| *named == code) {
        return Some((*name).to_owned());
    }
    match code {
        // Letters and the digits above them are their own characters.
        0x30..=0x39 | 0x41..=0x5A => char::from_u32(code).map(String::from),
        // The number pad's digits.
        0x60..=0x69 => char::from_u32(code - 0x30).map(String::from),
        0x70..=0x87 => Some(format!("F{}", code - 0x70 + 1)),
        _ => None,
    }
}

/// What a key pressed while a shortcut is recorded means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// A modifier alone: the key itself is still to come.
    Waiting,
    /// Escape alone: no shortcut after all.
    Cancel,
    /// The shortcut, as the core reads shortcuts ("Ctrl+Shift+K").
    Shortcut(String),
    /// A key no shortcut uses.
    Unknown,
}

/// What `chord`, pressed while a shortcut is recorded, means.
pub fn recorded(chord: Chord) -> Recorded {
    match chord.key {
        // Shift, Ctrl, Alt, the Windows keys, and their left and right ones.
        0x10..=0x12 | 0x5B | 0x5C | 0xA0..=0xA5 => Recorded::Waiting,
        0x1B if !chord.ctrl && !chord.alt && !chord.shift => Recorded::Cancel,
        key => key_name(key).map_or(Recorded::Unknown, |name| {
            let held = |on: bool, text: &'static str| if on { text } else { "" };
            Recorded::Shortcut(format!(
                "{}{}{}{name}",
                held(chord.ctrl, "Ctrl+"),
                held(chord.alt, "Alt+"),
                held(chord.shift, "Shift+"),
            ))
        }),
    }
}

/// The virtual-key code of a key as the core names it, or as its labels
/// write it.
fn key_code(name: &str) -> Option<u32> {
    if let Some((_, code)) = NAMED_KEYS.iter().find(|(named, _)| *named == name) {
        return Some(*code);
    }
    let code = match name {
        "Del" => 0x2E,
        "Return" => 0x0D,
        "Esc" => 0x1B,
        "PgUp" => 0x21,
        "PgDn" => 0x22,
        "," => 0xBC,
        "." => 0xBE,
        "/" => 0xBF,
        ";" => 0xBA,
        // The plus sign is on the equals key.
        "=" | "+" | "Plus" => 0xBB,
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
    /// Binds every command to the key it has, and the plugins' actions
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
                .filter_map(|command| {
                    // The key as the core gives it; its label, read, where
                    // only that is there.
                    let chord = command
                        .key
                        .as_ref()
                        .and_then(Chord::of)
                        .or_else(|| Chord::parse(&command.shortcut))?;
                    Some((chord, command.id))
                })
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
                    ..CommandView::default()
                },
                CommandView {
                    id: CommandId::OpenTrash,
                    label: "Open Trash".to_owned(),
                    ..CommandView::default()
                },
                // The key the core gives counts, whatever the label says.
                CommandView {
                    id: CommandId::SwapPanes,
                    label: "Swap Panes".to_owned(),
                    shortcut: "Ctrl+U".to_owned(),
                    key: Some(KeyView {
                        key: "Comma".to_owned(),
                        command: true,
                        alt: true,
                        ..KeyView::default()
                    }),
                    ..CommandView::default()
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
                control: false,
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
        assert_eq!(
            shortcuts.command(chord(0xBC, true, false, true)),
            Some(CommandId::SwapPanes)
        );
        assert_eq!(shortcuts.command(chord(0x55, true, false, false)), None);
    }

    #[test]
    fn every_key_the_recorder_names_is_bound_by_that_name() {
        // Named keys, letters, digits and function keys: the name a
        // recorded key gets is one the binder reads back as that key.
        let codes = NAMED_KEYS
            .iter()
            .map(|(_, code)| *code)
            .chain(0x30..=0x39)
            .chain(0x41..=0x5A)
            .chain(0x70..=0x87);
        for code in codes {
            let name = key_name(code).unwrap_or_else(|| panic!("{code:#x} has no name"));
            assert_eq!(key_code(&name), Some(code), "{name}");
        }
        assert_eq!(key_name(0x4B).as_deref(), Some("K"));
        assert_eq!(key_name(0x74).as_deref(), Some("F5"));
        assert_eq!(key_name(0x65).as_deref(), Some("5"), "the number pad's 5");
        assert_eq!(key_name(0x07), None);
    }

    #[test]
    fn a_recorded_key_is_written_as_the_core_reads_shortcuts() {
        assert_eq!(
            recorded(chord(0x4B, true, true, false)),
            Recorded::Shortcut("Ctrl+Shift+K".to_owned())
        );
        assert_eq!(
            recorded(chord(0x2E, false, false, true)),
            Recorded::Shortcut("Alt+Delete".to_owned())
        );
        assert_eq!(
            recorded(chord(0x74, false, false, false)),
            Recorded::Shortcut("F5".to_owned())
        );
        assert_eq!(
            recorded(chord(0xBC, true, false, true)),
            Recorded::Shortcut("Ctrl+Alt+Comma".to_owned())
        );
        // What it writes, the core reads.
        for text in ["Ctrl+Shift+K", "Alt+Delete", "F5", "Ctrl+Alt+Comma"] {
            assert!(
                dirfleet_core::shortcut::parse_command(text).is_ok(),
                "{text}"
            );
        }
        // A modifier on its own is not the shortcut yet; Escape gives up.
        assert_eq!(recorded(chord(0x11, true, false, false)), Recorded::Waiting);
        assert_eq!(recorded(chord(0xA0, false, true, false)), Recorded::Waiting);
        assert_eq!(recorded(chord(0x1B, false, false, false)), Recorded::Cancel);
        assert_eq!(
            recorded(chord(0x1B, true, false, false)),
            Recorded::Shortcut("Ctrl+Escape".to_owned())
        );
        assert_eq!(
            recorded(chord(0x07, false, false, false)),
            Recorded::Unknown
        );
    }
}
