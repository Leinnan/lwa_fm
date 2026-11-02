use std::{borrow::Cow, path::Path};

use egui::{Modal, TextBuffer};

use crate::{app::dock::CurrentPath, locations::Locations};

use super::commands::ActionToPerform;

#[derive(Default, Debug, Clone)]
pub struct CommandPalette {
    pub commands: Vec<ValidAction>,
    query: String,
    selected: usize,
    focus_requested: bool,
}

#[derive(Debug, Clone)]
pub struct ValidAction {
    pub action: ActionToPerform,
    pub name: Cow<'static, str>,
    /// Keyboard shortcut shown next to the name.
    pub shortcut: Option<String>,
}

pub fn build_for_path(
    current_path: &CurrentPath,
    path: &Path,
    favorites: &Locations,
) -> Vec<ValidAction> {
    let mut commands = Vec::new();
    if path.is_dir() {
        if current_path
            .single_path()
            .is_some_and(|f| f.as_path().eq(path))
        {
            if let Some(parent) = path.parent() {
                commands.push(ValidAction {
                    action: ActionToPerform::TabAction(
                        crate::app::commands::TabTarget::ActiveTab,
                        crate::app::commands::TabAction::ChangePaths(parent.to_path_buf().into()),
                    ),
                    name: "Go Up".into(),
                    shortcut: None,
                });
            }
        } else {
            commands.push(ValidAction {
                action: ActionToPerform::TabAction(
                    crate::app::commands::TabTarget::ActiveTab,
                    crate::app::commands::TabAction::ChangePaths(path.to_path_buf().into()),
                ),
                name: "Open".into(),
                shortcut: None,
            });
            commands.push(ValidAction {
                action: ActionToPerform::NewTab(path.to_path_buf()),
                name: "Open in new tab".into(),
                shortcut: None,
            });
            commands.push(ValidAction {
                action: ActionToPerform::NewWindow(path.to_path_buf()),
                name: "Open in new window".into(),
                shortcut: None,
            });
        }
        let open_name = if cfg!(windows) {
            "Open in Explorer"
        } else if cfg!(target_os = "macos") {
            "Open in Finder"
        } else {
            "Open in File Manager"
        };

        commands.push(ValidAction {
            action: ActionToPerform::SystemOpen(path.to_string_lossy().to_string().into()),
            name: Cow::Borrowed(open_name),
            shortcut: None,
        });
        commands.push(ActionToPerform::OpenInTerminal(path.to_path_buf()).into());
        let exist_in_favorites = favorites
            .locations
            .iter()
            .any(|f| path.to_string_lossy().eq(&f.path));
        if exist_in_favorites {
            commands.push(ValidAction {
                action: ActionToPerform::RemoveFromFavorites(
                    path.to_string_lossy().to_string().into(),
                ),
                name: "Remove from favorites".into(),
                shortcut: None,
            });
        } else {
            commands.push(ValidAction {
                action: ActionToPerform::AddToFavorites(path.to_string_lossy().to_string().into()),
                name: "Add to favorites".into(),
                shortcut: None,
            });
        }
    }
    commands
}

impl From<ActionToPerform> for ValidAction {
    fn from(val: ActionToPerform) -> Self {
        let name = (&val).into();
        Self {
            action: val,
            name,
            shortcut: None,
        }
    }
}

/// Every app command from the keymap, with its shortcut, for the palette.
pub fn app_commands(ctx: &egui::Context) -> Vec<ValidAction> {
    let mut commands: Vec<ValidAction> = Vec::new();
    for binding in crate::app::keymap::BINDINGS {
        if commands
            .iter()
            .any(|c| matches!(c.action, ActionToPerform::RunCommand(cmd) if cmd == binding.command))
        {
            continue;
        }
        commands.push(ValidAction {
            action: ActionToPerform::RunCommand(binding.command),
            name: Cow::Borrowed(binding.command.label()),
            shortcut: crate::app::keymap::shortcut_text(ctx, binding.command),
        });
    }
    commands
}

impl CommandPalette {
    pub fn build_for_path(
        &mut self,
        current_path: &CurrentPath,
        path: &Path,
        favorites: &Locations,
    ) {
        self.commands = build_for_path(current_path, path, favorites);
        self.query.clear();
        self.selected = 0;
        self.focus_requested = false;
    }

    pub fn ui(&mut self, ctx: &egui::Context) {
        let down =
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown));
        let up =
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp));
        let enter =
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        let mut action = None;
        let modal = Modal::new("Commands".into()).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 64.0).clamp(300.0, 460.0));
            ui.heading("Commands");
            let edit = ui.add(
                crate::app::ui::text_field(ui, &mut self.query)
                    .id(egui::Id::new("command_palette_query"))
                    .hint_text("Find a command…")
                    .desired_width(ui.available_width()),
            );
            if edit.changed() {
                self.selected = 0;
            }
            if !self.focus_requested {
                edit.request_focus();
                self.focus_requested = true;
            }
            ui.separator();
            let query = self.query.to_lowercase();
            let commands: Vec<_> = self
                .commands
                .iter()
                .filter(|command| command.name.to_lowercase().contains(&query))
                .collect();
            if commands.is_empty() {
                ui.weak("No matching commands.");
            } else {
                if down {
                    self.selected = (self.selected + 1).min(commands.len() - 1);
                }
                if up {
                    self.selected = self.selected.saturating_sub(1);
                }
                self.selected = self.selected.min(commands.len() - 1);
                egui::ScrollArea::vertical()
                    .max_height(ctx.content_rect().height() - 180.0)
                    .show(ui, |ui| {
                        for (index, command) in commands.iter().enumerate() {
                            let mut button = egui::Button::new(command.name.as_str())
                                .selected(index == self.selected);
                            if let Some(shortcut) = &command.shortcut {
                                button = button.shortcut_text(shortcut.as_str());
                            }
                            let response = ui.add_sized(
                                [ui.available_width(), crate::app::ui::control_height(ui)],
                                button,
                            );
                            if response.clicked() {
                                action = Some(command.action.clone());
                            }
                        }
                    });
                if enter {
                    action = Some(commands[self.selected].action.clone());
                }
            }
            ui.separator();
            ui.weak("Up/Down to choose · Enter to run · Esc to close");
        });
        if let Some(action) = action {
            action.schedule();
            ActionToPerform::CloseActiveModalWindow.schedule();
        } else if modal.should_close() {
            ActionToPerform::CloseActiveModalWindow.schedule();
        }
    }
}
