//! Keyboard shortcuts: as plugin manifests and the keymap write them, and
//! as the app shows them.
//!
//! `Ctrl+Alt+M`, `Shift+F5`: `Ctrl` means Cmd on macOS, and `Control` the
//! Mac's own Control key. The same grammar and key names as
//! `egui_mev_commands::Shortcut::parse` (which egui's app uses), without egui,
//! for the keys a shortcut uses: letters, digits, punctuation, editing and
//! navigation keys and F1 to F35; not modifier, lock, media or system keys.

/// A parsed shortcut. `key` is egui's name for the key ("M", "F5", "Down").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the modifier keys, one flag each"
)]
pub struct Shortcut {
    /// Cmd on macOS, Ctrl elsewhere.
    pub command: bool,
    /// The Mac's Control key; no key elsewhere.
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: &'static str,
}

/// Every key a shortcut can name: egui's name for it, then what else
/// spells it.
const KEYS: &[(&str, &[&str])] = &[
    ("Down", &["⏷", "ArrowDown"]),
    ("Left", &["⏴", "ArrowLeft"]),
    ("Right", &["⏵", "ArrowRight"]),
    ("Up", &["⏶", "ArrowUp"]),
    ("Escape", &["Esc"]),
    ("Tab", &[]),
    ("Backspace", &[]),
    ("Enter", &["Return", "NumpadEnter"]),
    ("Insert", &[]),
    ("Delete", &["Del"]),
    ("Home", &[]),
    ("End", &[]),
    ("PageUp", &[]),
    ("PageDown", &[]),
    ("Copy", &[]),
    ("Cut", &[]),
    ("Paste", &[]),
    ("Space", &[" "]),
    ("Colon", &[":"]),
    ("Comma", &[",", "NumpadComma"]),
    ("Minus", &["-", "−", "NumpadSubtract"]),
    ("Period", &[".", "NumpadDecimal"]),
    ("Plus", &["+", "NumpadAdd"]),
    ("Equals", &["=", "Equal", "NumpadEqual"]),
    ("Semicolon", &[";"]),
    ("Backslash", &["\\"]),
    ("Slash", &["/", "NumpadDivide"]),
    ("Pipe", &["|"]),
    ("Questionmark", &["?"]),
    ("Exclamationmark", &["!"]),
    ("OpenBracket", &["[", "BracketLeft"]),
    ("CloseBracket", &["]", "BracketRight"]),
    ("OpenCurlyBracket", &["{"]),
    ("CloseCurlyBracket", &["}"]),
    ("Backtick", &["`", "Backquote", "Grave"]),
    ("Quote", &["'"]),
    ("IntlBackslash", &[]),
    ("PrintScreen", &[]),
    ("Pause", &[]),
    ("Menu", &["ContextMenu", "Apps"]),
    ("Help", &[]),
    ("Clear", &[]),
];

const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
const LETTERS: [&str; 26] = [
    "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S",
    "T", "U", "V", "W", "X", "Y", "Z",
];
const FUNCTION_KEYS: [&str; 35] = [
    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "F13", "F14", "F15",
    "F16", "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24", "F25", "F26", "F27", "F28",
    "F29", "F30", "F31", "F32", "F33", "F34", "F35",
];

impl Shortcut {
    /// How the app's own shortcuts are written on this system, so the two
    /// can be compared and shown alike: "⌥⌘M" on macOS, "Ctrl+Alt+M"
    /// elsewhere.
    #[must_use]
    pub fn display(&self) -> String {
        if cfg!(target_os = "macos") {
            self.mac_text()
        } else {
            self.other_text()
        }
    }

    /// Modifier symbols in the Mac's order, then the key's symbol.
    #[must_use]
    pub fn mac_text(&self) -> String {
        let mut text = String::new();
        if self.control {
            text.push('⌃');
        }
        if self.alt {
            text.push('⌥');
        }
        if self.shift {
            text.push('⇧');
        }
        if self.command {
            text.push('⌘');
        }
        text.push_str(match self.key {
            "Up" => "↑",
            "Down" => "↓",
            "Left" => "←",
            "Right" => "→",
            "Backspace" => "⌫",
            "Delete" => "⌦",
            "Enter" => "↩",
            "Escape" => "⎋",
            "Tab" => "⇥",
            other => symbol(other),
        });
        text
    }

    /// "Ctrl+Alt+Shift+" then the key, as the app's other tables write it.
    #[must_use]
    pub fn other_text(&self) -> String {
        let mut text = String::new();
        for (on, name) in [
            (self.control, "Control+"),
            (self.command, "Ctrl+"),
            (self.alt, "Alt+"),
            (self.shift, "Shift+"),
        ] {
            if on {
                text.push_str(name);
            }
        }
        text.push_str(match self.key {
            "Delete" => "Del",
            other => symbol(other),
        });
        text
    }

    /// The shortcut as [`parse_command`] reads it back, whatever the
    /// system: how the keymap keeps it.
    #[must_use]
    pub fn neutral_text(&self) -> String {
        let mut text = String::new();
        for (on, name) in [
            (self.control, "Control+"),
            (self.command, "Ctrl+"),
            (self.alt, "Alt+"),
            (self.shift, "Shift+"),
        ] {
            if on {
                text.push_str(name);
            }
        }
        text.push_str(self.key);
        text
    }

    /// Whether the key is pressed with nothing that keeps it from typing
    /// or moving: no Ctrl, Cmd, Control or Alt.
    #[must_use]
    pub const fn is_plain(&self) -> bool {
        !self.command && !self.control && !self.alt
    }
}

/// The keys a command may have with no modifier, or with Shift alone:
/// they type nothing into a name.
const PLAIN_KEYS: [&str; 5] = ["Delete", "Backspace", "Insert", "Space", "Enter"];

/// A punctuation key as the character it types; other keys by name.
fn symbol(key: &'static str) -> &'static str {
    match key {
        "Comma" => ",",
        "Period" => ".",
        "Minus" => "-",
        "Plus" => "+",
        "Equals" => "=",
        "Semicolon" => ";",
        "Colon" => ":",
        "Slash" => "/",
        "Backslash" => "\\",
        "Pipe" => "|",
        "Questionmark" => "?",
        "Exclamationmark" => "!",
        "OpenBracket" => "[",
        "CloseBracket" => "]",
        "OpenCurlyBracket" => "{",
        "CloseCurlyBracket" => "}",
        "Backtick" => "`",
        "Quote" => "'",
        other => other,
    }
}

/// `Ctrl++` ends with the plus key, not an empty part.
fn split_parts(text: &str) -> Vec<&str> {
    if text == "+" {
        return vec!["+"];
    }
    text.strip_suffix("++").map_or_else(
        || text.split('+').collect(),
        |rest| {
            let mut parts: Vec<&str> = rest.split('+').collect();
            parts.push("+");
            parts
        },
    )
}

/// The key `name` spells, as egui reads it: one of its spellings exactly, or
/// its own name in any case.
fn key_named(name: &str) -> Option<&'static str> {
    let spelled = KEYS
        .iter()
        .find(|(_, others)| others.contains(&name))
        .map(|(key, _)| *key)
        .or_else(|| {
            let digit = name
                .strip_prefix("Digit")
                .or_else(|| name.strip_prefix("Numpad"))?;
            DIGITS.iter().copied().find(|key| *key == digit)
        })
        .or_else(|| {
            let letter = name.strip_prefix("Key")?;
            LETTERS.iter().copied().find(|key| *key == letter)
        });
    spelled.or_else(|| {
        KEYS.iter()
            .map(|(key, _)| *key)
            .chain(DIGITS)
            .chain(LETTERS)
            .chain(FUNCTION_KEYS)
            .find(|key| key.eq_ignore_ascii_case(name))
    })
}

/// Reads the modifiers and the key of `Ctrl+Alt+M`, whatever they are.
fn read(text: &str) -> Result<Shortcut, String> {
    let (mut command, mut control, mut alt, mut shift) = (false, false, false, false);
    let mut key = None;
    for part in split_parts(text).into_iter().map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "cmd" | "command" | "mod" => command = true,
            "control" => control = true,
            "alt" | "option" => alt = true,
            "shift" => shift = true,
            "" => return Err(format!("\"{text}\" is not a shortcut")),
            _ => {
                if key.is_some() {
                    return Err(format!("\"{text}\" names more than one key"));
                }
                key = Some(
                    key_named(part)
                        .ok_or_else(|| format!("unknown key \"{part}\" in \"{text}\""))?,
                );
            }
        }
    }
    let key = key.ok_or_else(|| format!("\"{text}\" has no key"))?;
    Ok(Shortcut {
        command,
        control,
        alt,
        shift,
        key,
    })
}

/// Reads `Ctrl+Alt+M`, `Shift+F5` and the like.
///
/// # Errors
/// When the text isn't a shortcut, or is a plain key other than a function
/// key, which would stop that key from typing; in the toolkit's words.
pub fn parse(text: &str) -> Result<Shortcut, String> {
    let shortcut = read(text)?;
    if shortcut.is_plain() && !shortcut.shift && !FUNCTION_KEYS.contains(&shortcut.key) {
        return Err(format!(
            "\"{text}\" needs Ctrl or Alt, so typing a name still works"
        ));
    }
    Ok(shortcut)
}

/// Reads a shortcut for one of the app's commands.
///
/// As [`parse`], and also the keys that type nothing on their own (Delete,
/// Space, Enter and the like), alone or with Shift. A letter with Shift
/// alone is refused: it types a capital.
///
/// # Errors
/// When the text isn't a shortcut, or its key would no longer type.
pub fn parse_command(text: &str) -> Result<Shortcut, String> {
    let shortcut = read(text)?;
    let types = shortcut.is_plain()
        && !FUNCTION_KEYS.contains(&shortcut.key)
        && !PLAIN_KEYS.contains(&shortcut.key)
        && !NAVIGATION_KEYS.contains(&shortcut.key);
    if types {
        return Err(format!(
            "\"{text}\" needs Ctrl or Alt, so typing a name still works"
        ));
    }
    Ok(shortcut)
}

/// The keys that move through a list or a dialog.
pub const NAVIGATION_KEYS: [&str; 10] = [
    "Up", "Down", "Left", "Right", "Home", "End", "PageUp", "PageDown", "Tab", "Escape",
];

/// Reads a shortcut as the app shows it: "Ctrl+Shift+N" and "Del" as
/// [`Shortcut::other_text`] writes them, "⇧⌘N" and "⌘⌫" as
/// [`Shortcut::mac_text`] does. `None` for "" and for what is no shortcut.
#[must_use]
pub fn parse_shown(text: &str) -> Option<Shortcut> {
    const SYMBOLS: [(char, &str); 9] = [
        ('↑', "Up"),
        ('↓', "Down"),
        ('←', "Left"),
        ('→', "Right"),
        ('⌫', "Backspace"),
        ('⌦', "Delete"),
        ('↩', "Enter"),
        ('⎋', "Escape"),
        ('⇥', "Tab"),
    ];
    let mut shortcut = Shortcut {
        command: false,
        control: false,
        alt: false,
        shift: false,
        key: "",
    };
    // The Mac's modifier signs come first, without a plus between them.
    let key = text.trim_start_matches(|sign| match sign {
        '⌃' => {
            shortcut.control = true;
            true
        }
        '⌥' => {
            shortcut.alt = true;
            true
        }
        '⇧' => {
            shortcut.shift = true;
            true
        }
        '⌘' => {
            shortcut.command = true;
            true
        }
        _ => false,
    });
    let mut signs = key.chars();
    let sign = match (signs.next(), signs.next()) {
        (Some(sign), None) => SYMBOLS
            .iter()
            .find(|(symbol, _)| *symbol == sign)
            .map(|(_, key)| *key),
        _ => None,
    };
    shortcut.key = match sign {
        Some(key) => key,
        // No sign of the Mac's: written with pluses, as elsewhere.
        None if key.len() == text.len() => return read(text).ok(),
        None => key_named(key)?,
    };
    Some(shortcut)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_keys_and_spellings_read_as_egui_names() {
        assert_eq!(
            parse("Ctrl+Alt+m"),
            Ok(Shortcut {
                command: true,
                control: false,
                alt: true,
                shift: false,
                key: "M"
            })
        );
        assert_eq!(parse("Shift+F5").map(|s| s.key), Ok("F5"));
        assert_eq!(
            parse("F12").map(|s| s.key),
            Ok("F12"),
            "function keys alone"
        );
        assert_eq!(parse("Cmd++").map(|s| s.key), Ok("Plus"));
        assert_eq!(parse("Option+ArrowDown").map(|s| s.key), Ok("Down"));
        assert_eq!(parse("Ctrl+Digit3").map(|s| s.key), Ok("3"));
        assert_eq!(parse("Ctrl+KeyQ").map(|s| s.key), Ok("Q"));
    }

    #[test]
    fn shortcuts_read_as_the_app_writes_its_own() {
        let text = |shortcut: &str| parse(shortcut).map(|s| (s.mac_text(), s.other_text()));
        assert_eq!(text("Ctrl+Alt+m"), Ok(("⌥⌘M".into(), "Ctrl+Alt+M".into())));
        assert_eq!(
            text("Ctrl+Shift+N"),
            Ok(("⇧⌘N".into(), "Ctrl+Shift+N".into()))
        );
        assert_eq!(text("Cmd+["), Ok(("⌘[".into(), "Ctrl+[".into())));
        assert_eq!(text("Alt+Delete"), Ok(("⌥⌦".into(), "Alt+Del".into())));
        assert_eq!(text("Alt+Up"), Ok(("⌥↑".into(), "Alt+Up".into())));
        assert_eq!(text("F5"), Ok(("F5".into(), "F5".into())));
    }

    #[test]
    fn a_command_may_have_a_key_that_types_nothing_but_not_one_that_types() {
        assert_eq!(parse_command("Delete").map(|s| s.key), Ok("Delete"));
        assert_eq!(parse_command("Shift+Del").map(|s| s.key), Ok("Delete"));
        assert_eq!(parse_command("Space").map(|s| s.key), Ok("Space"));
        assert_eq!(parse_command("F2").map(|s| s.key), Ok("F2"));
        assert_eq!(parse_command("Ctrl+U").map(|s| s.key), Ok("U"));
        // The keys that move are read; whether they may be taken is the
        // keymap's to say.
        assert_eq!(parse_command("Down").map(|s| s.key), Ok("Down"));
        for typed in ["M", "Shift+M", "7", "Shift+,"] {
            assert_eq!(
                parse_command(typed),
                Err(format!(
                    "\"{typed}\" needs Ctrl or Alt, so typing a name still works"
                )),
            );
        }
        // The Mac's Control key is a modifier of its own.
        let both = parse_command("Control+Cmd+S").expect("a shortcut");
        assert!(both.control && both.command);
        assert_eq!(both.mac_text(), "⌃⌘S");
        assert_eq!(both.neutral_text(), "Control+Ctrl+S");
        assert_eq!(parse_command(&both.neutral_text()), Ok(both));
    }

    #[test]
    fn shortcuts_are_read_back_as_the_app_shows_them() {
        for shown in [
            "Ctrl+Shift+N",
            "Alt+Left",
            "Del",
            "Shift+Del",
            "F5",
            "Space",
            "Ctrl+,",
            "Ctrl+Alt+S",
        ] {
            let shortcut = parse_shown(shown).expect(shown);
            assert_eq!(shortcut.other_text(), shown);
        }
        for shown in [
            "⌘[",
            "⌘↑",
            "⇧⌘G",
            "⌃⌘S",
            "⌥⌘I",
            "⇧⌘.",
            "⌘⌫",
            "⌥⌘⌫",
            "↩",
            "Space",
            "⌘/",
            "⌘2",
        ] {
            let shortcut = parse_shown(shown).expect(shown);
            assert_eq!(shortcut.mac_text(), shown);
        }
        assert_eq!(parse_shown(""), None);
        assert_eq!(parse_shown("⌘"), None);
        assert_eq!(parse_shown("Ctrl+Nope"), None);
    }

    #[test]
    fn what_isnt_a_shortcut_says_why() {
        assert_eq!(
            parse("M"),
            Err("\"M\" needs Ctrl or Alt, so typing a name still works".into())
        );
        assert_eq!(
            parse("Ctrl+A+B"),
            Err("\"Ctrl+A+B\" names more than one key".into())
        );
        assert_eq!(
            parse("Ctrl+Nope"),
            Err("unknown key \"Nope\" in \"Ctrl+Nope\"".into())
        );
        assert_eq!(parse("Ctrl+Alt"), Err("\"Ctrl+Alt\" has no key".into()));
        assert_eq!(
            parse("Ctrl++A"),
            Err("\"Ctrl++A\" is not a shortcut".into())
        );
    }
}
