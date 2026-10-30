//! Keyboard shortcuts for plugin actions: the manifest's default or the
//! user's own, never one the app already uses.

use egui::{Context, Key, KeyboardShortcut, Modifiers};

/// Parses `Ctrl+Alt+M`, `Shift+F5` and the like. `Ctrl` means Cmd on macOS.
pub fn parse_shortcut(text: &str) -> Result<KeyboardShortcut, String> {
    let mut modifiers = Modifiers::NONE;
    let mut key = None;
    for part in text.split('+').map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "cmd" | "command" | "mod" => modifiers |= Modifiers::COMMAND,
            "alt" | "option" => modifiers |= Modifiers::ALT,
            "shift" => modifiers |= Modifiers::SHIFT,
            "" => return Err(format!("\"{text}\" is not a shortcut")),
            _ => {
                if key.is_some() {
                    return Err(format!("\"{text}\" names more than one key"));
                }
                key = Some(
                    Key::from_name(part)
                        .or_else(|| Key::from_name(&part.to_ascii_uppercase()))
                        .ok_or_else(|| format!("unknown key \"{part}\" in \"{text}\""))?,
                );
            }
        }
    }
    let key = key.ok_or_else(|| format!("\"{text}\" has no key"))?;
    let function_key = matches!(
        key,
        Key::F1
            | Key::F2
            | Key::F3
            | Key::F4
            | Key::F5
            | Key::F6
            | Key::F7
            | Key::F8
            | Key::F9
            | Key::F10
            | Key::F11
            | Key::F12
    );
    if modifiers.is_none() && !function_key {
        return Err(format!(
            "\"{text}\" needs Ctrl or Alt, so typing a name still works"
        ));
    }
    Ok(KeyboardShortcut::new(modifiers, key))
}

/// The built-in command that already uses `shortcut`, if any.
pub fn conflict_with_app(shortcut: KeyboardShortcut) -> Option<&'static str> {
    crate::app::keymap::BINDINGS
        .iter()
        .find(|binding| binding.shortcut == shortcut)
        .map(|binding| binding.command.label())
}

/// A plugin action bound to a shortcut.
#[derive(Debug, Clone)]
pub struct PluginBinding {
    pub shortcut: KeyboardShortcut,
    /// Full action id, `<plugin id>.<action id>`.
    pub action: String,
}

/// The plugin actions whose shortcuts were pressed this frame. Their key
/// events are consumed. Not while a text field has focus.
pub fn pressed(ctx: &Context, bindings: &[PluginBinding]) -> Vec<String> {
    if bindings.is_empty() || ctx.text_edit_focused() {
        return Vec::new();
    }
    let mut actions = Vec::new();
    ctx.input_mut(|input| {
        for binding in bindings {
            let KeyboardShortcut {
                modifiers,
                logical_key,
            } = binding.shortcut;
            let before = input.events.len();
            input.events.retain(|event| {
                !matches!(
                    event,
                    egui::Event::Key { key, modifiers: pressed, pressed: true, .. }
                        if *key == logical_key && pressed.matches_exact(modifiers)
                )
            });
            if input.events.len() != before && !actions.contains(&binding.action) {
                actions.push(binding.action.clone());
            }
        }
    });
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_shortcuts() {
        let shortcut = parse_shortcut("Ctrl+Alt+M").expect("valid");
        assert_eq!(shortcut.logical_key, Key::M);
        assert_eq!(shortcut.modifiers, Modifiers::COMMAND | Modifiers::ALT);
        assert_eq!(parse_shortcut("F6").expect("valid").logical_key, Key::F6);
        assert!(parse_shortcut("M").is_err());
        assert!(parse_shortcut("Ctrl+Nope").is_err());
        assert!(parse_shortcut("Ctrl+A+B").is_err());
        assert!(parse_shortcut("Ctrl+").is_err());
    }

    #[test]
    fn finds_conflicts_with_built_in_shortcuts() {
        let refresh = parse_shortcut("Ctrl+R").expect("valid");
        assert_eq!(conflict_with_app(refresh), Some("Refresh"));
        let free = parse_shortcut("Ctrl+Alt+M").expect("valid");
        assert_eq!(conflict_with_app(free), None);
    }
}
