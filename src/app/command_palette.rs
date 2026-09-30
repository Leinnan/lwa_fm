use std::{borrow::Cow, path::Path};

use egui::{Modal, TextBuffer};

use crate::{app::dock::CurrentPath, locations::Locations};

use super::commands::ActionToPerform;

#[derive(Default, Debug, Clone)]
pub struct CommandPalette {
    pub commands: Vec<ValidAction>,
    query: String,
    selected: usize,
}

#[derive(Debug, Clone)]
pub struct ValidAction {
    pub action: ActionToPerform,
    pub name: Cow<'static, str>,
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
                });
            }
        } else {
            commands.push(ValidAction {
                action: ActionToPerform::TabAction(
                    crate::app::commands::TabTarget::ActiveTab,
                    crate::app::commands::TabAction::ChangePaths(path.to_path_buf().into()),
                ),
                name: "Open".into(),
            });
            commands.push(ValidAction {
                action: ActionToPerform::NewTab(path.to_path_buf()),
                name: "Open in new tab".into(),
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
            });
        } else {
            commands.push(ValidAction {
                action: ActionToPerform::AddToFavorites(path.to_string_lossy().to_string().into()),
                name: "Add to favorites".into(),
            });
        }
    }
    commands
}

impl From<ActionToPerform> for ValidAction {
    fn from(val: ActionToPerform) -> Self {
        let name = (&val).into();
        Self { action: val, name }
    }
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
    }

    pub fn ui(&mut self, ctx: &egui::Context) {
        let mut action = None;
        let modal = Modal::new("Commands".into()).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 64.0).clamp(300.0, 460.0));
            ui.heading("Commands");
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Find a command…")
                    .desired_width(ui.available_width()),
            );
            if edit.changed() {
                self.selected = 0;
            }
            if !ctx.memory(|m| m.focused().is_some()) {
                edit.request_focus();
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
                if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                    self.selected = (self.selected + 1).min(commands.len() - 1);
                }
                if ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                    self.selected = self.selected.saturating_sub(1);
                }
                self.selected = self.selected.min(commands.len() - 1);
                egui::ScrollArea::vertical()
                    .max_height(ctx.content_rect().height() - 180.0)
                    .show(ui, |ui| {
                        for (index, command) in commands.iter().enumerate() {
                            let response = ui.add_sized(
                                [ui.available_width(), 28.0],
                                egui::Button::new(command.name.as_str())
                                    .selected(index == self.selected),
                            );
                            if response.clicked() {
                                action = Some(command.action.clone());
                            }
                        }
                    });
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    action = Some(commands[self.selected].action.clone());
                }
            }
            ui.separator();
            ui.weak("↑ ↓ to choose · Enter to run · Esc to close");
        });
        if let Some(action) = action {
            action.schedule();
            ActionToPerform::CloseActiveModalWindow.schedule();
        } else if modal.should_close() {
            ActionToPerform::CloseActiveModalWindow.schedule();
        }
    }
}
