//! Keyboard shortcuts for plugin actions: the manifest's default or the
//! user's own, never one the app already uses.

use egui::{Context, KeyboardShortcut};
use egui_mev_commands::Shortcut;

/// Parses `Ctrl+Alt+M`, `Shift+F5` and the like. `Ctrl` means Cmd on macOS.
pub fn parse_shortcut(text: &str) -> Result<KeyboardShortcut, String> {
    Shortcut::parse(text)
        .map(|shortcut| shortcut.0)
        .map_err(|error| error.to_string())
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
    use egui::{Key, Modifiers};

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

    /// The core reads manifests without egui; it must accept the same
    /// shortcuts, as the same keys, except keys no shortcut uses.
    #[test]
    fn the_core_reads_shortcuts_as_the_toolkit_does() {
        use dirfleet_core::plugins::shortcut;
        let unused = |name: &str| {
            [
                "Browser", "Media", "Audio", "Launch", "Shift", "Control", "Alt", "Super",
            ]
            .iter()
            .any(|prefix| name.starts_with(prefix))
                || name.ends_with("Lock")
                || ["Fn", "Eject", "Power", "Sleep"].contains(&name)
        };
        for key in Key::ALL {
            let name = key.name();
            for text in [
                format!("Ctrl+{name}"),
                format!("Alt+Shift+{}", name.to_lowercase()),
                name.to_owned(),
            ] {
                let egui = parse_shortcut(&text);
                let core = shortcut::parse(&text);
                if unused(name) {
                    assert!(core.is_err(), "{text}: no shortcut uses it");
                    continue;
                }
                match (&egui, &core) {
                    (Ok(egui), Ok(core)) => {
                        assert_eq!(core.key, egui.logical_key.name(), "{text}");
                        assert_eq!(core.command, egui.modifiers.command, "{text}");
                        assert_eq!(core.alt, egui.modifiers.alt, "{text}");
                        assert_eq!(core.shift, egui.modifiers.shift, "{text}");
                    }
                    (Err(egui), Err(core)) => assert_eq!(core, egui, "{text}"),
                    _ => panic!("{text}: egui says {egui:?}, the core {core:?}"),
                }
            }
        }
        for text in [
            "Cmd++",
            "Option+ArrowDown",
            "Ctrl+Digit3",
            "Ctrl+KeyQ",
            "Ctrl+Esc",
            "Ctrl+",
        ] {
            assert_eq!(
                parse_shortcut(text).map(|s| s.logical_key.name()).ok(),
                shortcut::parse(text).map(|s| s.key).ok(),
                "{text}"
            );
        }
    }

    #[test]
    fn finds_conflicts_with_built_in_shortcuts() {
        let refresh = parse_shortcut("Ctrl+R").expect("valid");
        assert_eq!(conflict_with_app(refresh), Some("Refresh"));
        let free = parse_shortcut("Ctrl+Alt+M").expect("valid");
        assert_eq!(conflict_with_app(free), None);
    }
}
