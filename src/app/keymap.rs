//! App-wide keyboard shortcuts, defined in one table.
//!
//! Every shortcut that is not tied to the file view's rows lives here, so the
//! help overlay, menus and tooltips can show the same key combinations that
//! actually run. Bindings follow Windows Explorer where it has one.

use std::sync::LazyLock;

use egui::{Context, Key, Modifiers, PointerButton};
use egui_mev_commands::{Binding, HelpSection, Keymap};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    Back,
    Forward,
    Up,
    Refresh,
    FocusSearch,
    FocusAddress,
    Palette,
    Settings,
    ToggleSidebar,
    ToggleInspector,
    ToggleHidden,
    ViewList,
    ViewGrid,
    NewTab,
    NewWindow,
    CloseTab,
    NextTab,
    PreviousTab,
    ShortcutHelp,
    Delete,
    DeletePermanently,
    Rename,
    NewFolder,
    Duplicate,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    CopyAsPath,
    SaveSearch,
    /// Toggles "match case" in the open search.
    MatchCase,
    /// Ctrl+1..8 select that tab; Ctrl+9 the last one.
    SelectTab(u8),
    ReopenClosedTab,
    #[cfg(feature = "profiling")]
    Profiler,
}

impl Command {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Back => "Back",
            Self::Forward => "Forward",
            Self::Up => "Up to parent folder",
            Self::Refresh => "Refresh",
            Self::FocusSearch => "Search",
            Self::FocusAddress => "Edit path",
            Self::Palette => "Commands",
            Self::Settings => "Settings",
            Self::ToggleSidebar => "Toggle sidebar",
            Self::ToggleInspector => "Toggle inspector",
            Self::ToggleHidden => "Show hidden items",
            Self::ViewList => "List view",
            Self::ViewGrid => "Grid view",
            Self::NewTab => "New tab",
            Self::NewWindow => "New window",
            Self::CloseTab => "Close tab",
            Self::NextTab => "Next tab",
            Self::PreviousTab => "Previous tab",
            Self::ShortcutHelp => "Keyboard shortcuts",
            Self::Delete => "Delete",
            Self::DeletePermanently => "Delete permanently",
            Self::Rename => "Rename",
            Self::NewFolder => "New folder",
            Self::Duplicate => "Duplicate",
            Self::Undo => "Undo",
            Self::Redo => "Redo",
            Self::Cut => "Cut",
            Self::Copy => "Copy",
            Self::Paste => "Paste",
            Self::CopyAsPath => "Copy as path",
            Self::SaveSearch => "Save search",
            Self::MatchCase => "Match case",
            Self::SelectTab(9) => "Go to the last tab",
            Self::SelectTab(_) => "Go to tab 1–8",
            Self::ReopenClosedTab => "Reopen closed tab",
            #[cfg(feature = "profiling")]
            Self::Profiler => "Profiler",
        }
    }
}

const fn bind(modifiers: Modifiers, key: Key, command: Command, in_text: bool) -> Binding<Command> {
    let binding = Binding::new(modifiers, key, command);
    if in_text { binding.in_text() } else { binding }
}

const NONE: Modifiers = Modifiers::NONE;
const CMD: Modifiers = Modifiers::COMMAND;
const ALT: Modifiers = Modifiers::ALT;
const CMD_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);
const CMD_ALT: Modifiers = Modifiers::COMMAND.plus(Modifiers::ALT);

/// All bindings. The first binding of a command is the one shown in menus.
pub const BINDINGS: &[Binding<Command>] = &[
    bind(ALT, Key::ArrowLeft, Command::Back, false),
    bind(NONE, Key::Backspace, Command::Back, false),
    bind(ALT, Key::ArrowRight, Command::Forward, false),
    bind(ALT, Key::ArrowUp, Command::Up, false),
    bind(NONE, Key::F5, Command::Refresh, true),
    bind(CMD, Key::R, Command::Refresh, false),
    bind(CMD, Key::F, Command::FocusSearch, true),
    bind(NONE, Key::F3, Command::FocusSearch, true),
    bind(CMD, Key::E, Command::FocusSearch, true),
    bind(CMD, Key::L, Command::FocusAddress, true),
    bind(ALT, Key::D, Command::FocusAddress, true),
    bind(NONE, Key::F4, Command::FocusAddress, true),
    bind(CMD_SHIFT, Key::P, Command::Palette, true),
    bind(CMD, Key::K, Command::Palette, true),
    bind(CMD, Key::Comma, Command::Settings, true),
    bind(CMD_ALT, Key::S, Command::ToggleSidebar, true),
    bind(CMD_ALT, Key::I, Command::ToggleInspector, true),
    bind(CMD, Key::H, Command::ToggleHidden, false),
    bind(CMD_SHIFT, Key::Num1, Command::ViewList, false),
    bind(CMD_SHIFT, Key::Num2, Command::ViewGrid, false),
    bind(CMD_SHIFT, Key::T, Command::ReopenClosedTab, true),
    bind(CMD, Key::T, Command::NewTab, true),
    bind(CMD, Key::N, Command::NewWindow, true),
    bind(CMD, Key::W, Command::CloseTab, true),
    bind(CMD_SHIFT, Key::Tab, Command::PreviousTab, true),
    bind(CMD, Key::Tab, Command::NextTab, true),
    bind(NONE, Key::Delete, Command::Delete, false),
    bind(
        Modifiers::SHIFT,
        Key::Delete,
        Command::DeletePermanently,
        false,
    ),
    bind(NONE, Key::F2, Command::Rename, false),
    bind(CMD_SHIFT, Key::N, Command::NewFolder, false),
    bind(CMD, Key::D, Command::Duplicate, false),
    bind(CMD, Key::Z, Command::Undo, false),
    bind(CMD, Key::Y, Command::Redo, false),
    bind(CMD_SHIFT, Key::Z, Command::Redo, false),
    bind(CMD, Key::X, Command::Cut, false),
    bind(CMD_SHIFT, Key::C, Command::CopyAsPath, false),
    bind(CMD, Key::C, Command::Copy, false),
    bind(CMD, Key::V, Command::Paste, false),
    bind(CMD, Key::S, Command::SaveSearch, true),
    bind(ALT, Key::C, Command::MatchCase, true),
    bind(CMD, Key::Num1, Command::SelectTab(1), true),
    bind(CMD, Key::Num2, Command::SelectTab(2), true),
    bind(CMD, Key::Num3, Command::SelectTab(3), true),
    bind(CMD, Key::Num4, Command::SelectTab(4), true),
    bind(CMD, Key::Num5, Command::SelectTab(5), true),
    bind(CMD, Key::Num6, Command::SelectTab(6), true),
    bind(CMD, Key::Num7, Command::SelectTab(7), true),
    bind(CMD, Key::Num8, Command::SelectTab(8), true),
    bind(CMD, Key::Num9, Command::SelectTab(9), true),
    bind(NONE, Key::F1, Command::ShortcutHelp, true),
    bind(CMD, Key::Slash, Command::ShortcutHelp, false),
    #[cfg(feature = "profiling")]
    bind(CMD_ALT, Key::F12, Command::Profiler, true),
];

/// Keys handled by the file view itself, listed in the shortcut overlay.
pub const VIEW_KEYS: &[(&str, &str)] = &[
    ("↑ ↓ ← →, Home, End, PgUp, PgDn", "Move between items"),
    ("Shift + movement", "Extend the selection"),
    ("Ctrl + movement", "Move without selecting"),
    ("Ctrl+Space", "Toggle the focused item"),
    ("Space", "Quick Look: preview the focused item"),
    ("Ctrl+A", "Select all"),
    ("Ctrl+Shift+I", "Invert selection"),
    ("Esc", "Clear the selection"),
    ("Type a name", "Jump to a matching item"),
    ("Enter", "Open the selected items"),
    ("Ctrl+Enter, middle-click", "Open folders in a new tab"),
    (
        "Click / Ctrl+click / Shift+click",
        "Select / toggle / select range",
    ),
    ("Mouse back / forward buttons", "Back / Forward"),
];

static KEYMAP: LazyLock<Keymap<Command>> = LazyLock::new(|| Keymap::new(BINDINGS.iter().cloned()));

/// Returns the commands whose shortcuts were pressed this frame and consumes
/// their key events. Modifiers must match exactly, so Ctrl+Shift+Tab never
/// also triggers Ctrl+Tab.
pub fn pressed(ctx: &Context) -> Vec<Command> {
    let mut commands = KEYMAP.pressed(ctx);
    ctx.input(|input| {
        if input.pointer.button_pressed(PointerButton::Extra1) {
            commands.push(Command::Back);
        }
        if input.pointer.button_pressed(PointerButton::Extra2) {
            commands.push(Command::Forward);
        }
    });
    commands
}

/// The first shortcut bound to `command`, formatted for this platform.
pub fn shortcut_text(ctx: &Context, command: Command) -> Option<String> {
    KEYMAP.shortcut_text(ctx, &command)
}

/// A tooltip like "Back  Alt+Left".
pub fn tooltip(ctx: &Context, command: Command) -> String {
    KEYMAP.tooltip(ctx, command.label(), &command)
}

/// A menu entry that runs `command` and shows its shortcut, so menus and the
/// keyboard always do the same thing.
pub fn menu_button(ui: &mut egui::Ui, command: Command) {
    let mut button = egui::Button::new(command.label());
    if let Some(shortcut) = shortcut_text(ui.ctx(), command) {
        button = button.shortcut_text(shortcut);
    }
    if ui.add(button).clicked() {
        crate::app::commands::ActionToPerform::RunCommand(command).schedule();
        ui.close();
    }
}

/// Like [`menu_button`] with a custom label (e.g. "Undo rename"), disabled
/// when `enabled` is false.
pub fn menu_button_labeled(ui: &mut egui::Ui, command: Command, label: &str, enabled: bool) {
    let mut button = egui::Button::new(label);
    if let Some(shortcut) = shortcut_text(ui.ctx(), command) {
        button = button.shortcut_text(shortcut);
    }
    if ui.add_enabled(enabled, button).clicked() {
        crate::app::commands::ActionToPerform::RunCommand(command).schedule();
        ui.close();
    }
}

/// Like [`menu_button`], drawn as a checkbox reflecting `checked`.
pub fn menu_toggle(ui: &mut egui::Ui, command: Command, checked: bool) {
    let mut button = egui::Button::selectable(checked, command.label());
    if let Some(shortcut) = shortcut_text(ui.ctx(), command) {
        button = button.shortcut_text(shortcut);
    }
    if ui.add(button).clicked() {
        crate::app::commands::ActionToPerform::RunCommand(command).schedule();
        ui.close();
    }
}

/// Every shortcut as a settings row with key caps: the Settings → Keyboard
/// page and the shortcut overlay show the same list.
pub fn shortcut_rows(ui: &mut egui::Ui) {
    let sections = [
        HelpSection::from_keymap(ui.ctx(), "Application", &KEYMAP, |command| {
            Some(command.label().into())
        }),
        HelpSection::from_keys("Files", VIEW_KEYS),
    ];
    egui_mev_commands::shortcut_rows(ui, &sections);
    crate::plugins::shortcut_rows(ui);
}

/// Draws the keyboard shortcut overlay. Returns `true` when it should close.
pub fn help_ui(ctx: &Context) -> bool {
    let mut close = false;
    let modal = egui::Modal::new(egui::Id::unique("keyboard_shortcuts")).show(ctx, |ui| {
        ui.set_width((ctx.content_rect().width() - 64.0).clamp(300.0, 560.0));
        ui.heading("Keyboard shortcuts");
        egui::ScrollArea::vertical()
            .max_height(ctx.content_rect().height() - 160.0)
            .show(ui, shortcut_rows);
        ui.separator();
        if ui.button("Close").clicked() {
            close = true;
        }
    });
    close || modal.should_close()
}

#[cfg(test)]
mod tests {
    use super::{BINDINGS, Command, pressed};
    use egui::{Event, Key, Modifiers, RawInput};

    fn press(ctx: &egui::Context, modifiers: Modifiers, key: Key) -> Vec<Command> {
        let mut commands = Vec::new();
        let input = RawInput {
            events: vec![Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| commands = pressed(ui.ctx()));
        output.textures_delta.clear();
        commands
    }

    #[test]
    fn modifiers_must_match_exactly() {
        let ctx = egui::Context::default();
        assert_eq!(
            press(&ctx, Modifiers::COMMAND | Modifiers::SHIFT, Key::Tab),
            vec![Command::PreviousTab]
        );
        assert_eq!(
            press(&ctx, Modifiers::COMMAND, Key::Tab),
            vec![Command::NextTab]
        );
        assert_eq!(
            press(&ctx, Modifiers::ALT, Key::ArrowLeft),
            vec![Command::Back]
        );
        assert_eq!(press(&ctx, Modifiers::NONE, Key::ArrowLeft), Vec::new());
    }

    fn clipboard(ctx: &egui::Context, modifiers: Modifiers, event: Event) -> Vec<Command> {
        let mut commands = Vec::new();
        let input = RawInput {
            events: vec![Event::ModifiersChanged(modifiers), event],
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| commands = pressed(ui.ctx()));
        output.textures_delta.clear();
        commands
    }

    /// egui-winit sends these as clipboard events instead of key presses.
    #[test]
    fn clipboard_events_run_their_shortcuts() {
        let ctx = egui::Context::default();
        assert_eq!(
            clipboard(&ctx, Modifiers::COMMAND, Event::Cut),
            vec![Command::Cut]
        );
        assert_eq!(
            clipboard(&ctx, Modifiers::COMMAND, Event::Copy),
            vec![Command::Copy]
        );
        assert_eq!(
            clipboard(&ctx, Modifiers::COMMAND | Modifiers::SHIFT, Event::Copy),
            vec![Command::CopyAsPath]
        );
        assert_eq!(
            clipboard(&ctx, Modifiers::COMMAND, Event::Paste("x".into())),
            vec![Command::Paste]
        );
        if cfg!(windows) {
            assert_eq!(
                clipboard(&ctx, Modifiers::SHIFT, Event::Cut),
                vec![Command::DeletePermanently]
            );
        }
    }

    #[test]
    fn every_shortcut_is_bound_once() {
        for (index, binding) in BINDINGS.iter().enumerate() {
            assert!(
                BINDINGS[..index]
                    .iter()
                    .all(|other| other.shortcut != binding.shortcut),
                "{:?} is bound twice",
                binding.shortcut
            );
        }
    }
}
