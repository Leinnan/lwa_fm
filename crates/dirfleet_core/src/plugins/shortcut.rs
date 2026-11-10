//! Keyboard shortcuts as plugin manifests write them.
//!
//! `Ctrl+Alt+M`, `Shift+F5`: `Ctrl` means Cmd on macOS. The same grammar and key names as
//! `egui_mev_commands::Shortcut::parse` (which egui's app uses), without egui,
//! for the keys a shortcut uses: letters, digits, punctuation, editing and
//! navigation keys and F1 to F35; not modifier, lock, media or system keys.

/// A parsed shortcut. `key` is egui's name for the key ("M", "F5", "Down").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortcut {
    /// Cmd on macOS, Ctrl elsewhere.
    pub command: bool,
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
    ("Delete", &[]),
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
}

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

/// Reads `Ctrl+Alt+M`, `Shift+F5` and the like.
///
/// # Errors
/// When the text isn't a shortcut, or is a plain key other than a function
/// key, which would stop that key from typing; in the toolkit's words.
pub fn parse(text: &str) -> Result<Shortcut, String> {
    let (mut command, mut alt, mut shift) = (false, false, false);
    let mut key = None;
    for part in split_parts(text).into_iter().map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "cmd" | "command" | "mod" => command = true,
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
    if !command && !alt && !shift && !FUNCTION_KEYS.contains(&key) {
        return Err(format!(
            "\"{text}\" needs Ctrl or Alt, so typing a name still works"
        ));
    }
    Ok(Shortcut {
        command,
        alt,
        shift,
        key,
    })
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
