use std::{borrow::Cow, path::Path};

use egui_mev_commands::{PaletteItem, PaletteResponse};

use crate::{app::dock::CurrentPath, locations::Locations};

use super::commands::ActionToPerform;

/// The palette's items while they are put together, and the palette once open.
#[derive(Default, Debug, Clone)]
pub struct CommandPalette {
    pub commands: Vec<ValidAction>,
    palette: egui_mev_commands::CommandPalette<ActionToPerform>,
    opened: bool,
}

pub type ValidAction = PaletteItem<ActionToPerform>;

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
                        crate::app::commands::TabAction::ChangePaths(
                            crate::app::dock::existing_dir(parent.to_path_buf()),
                        ),
                    ),
                    name: "Go Up".into(),
                    shortcut: None,
                    enabled: true,
                });
            }
        } else {
            commands.push(ValidAction {
                action: ActionToPerform::TabAction(
                    crate::app::commands::TabTarget::ActiveTab,
                    crate::app::commands::TabAction::ChangePaths(crate::app::dock::existing_dir(
                        path.to_path_buf(),
                    )),
                ),
                name: "Open".into(),
                shortcut: None,
                enabled: true,
            });
            commands.push(ValidAction {
                action: ActionToPerform::NewTab(path.to_path_buf()),
                name: "Open in new tab".into(),
                shortcut: None,
                enabled: true,
            });
            commands.push(ValidAction {
                action: ActionToPerform::NewWindow(path.to_path_buf()),
                name: "Open in new window".into(),
                shortcut: None,
                enabled: true,
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
            enabled: true,
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
                enabled: true,
            });
        } else {
            commands.push(ValidAction {
                action: ActionToPerform::AddToFavorites(path.to_string_lossy().to_string().into()),
                name: "Add to favorites".into(),
                shortcut: None,
                enabled: true,
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
            enabled: true,
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
            enabled: true,
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
        self.opened = false;
    }

    pub fn ui(&mut self, ctx: &egui::Context) {
        if !self.opened {
            self.palette.reopen(std::mem::take(&mut self.commands));
            self.opened = true;
        }
        match self.palette.show(ctx) {
            PaletteResponse::Open => {}
            PaletteResponse::Chosen(action) => {
                // Closing first: the action may open a window of its own.
                ActionToPerform::CloseActiveModalWindow.schedule();
                action.schedule();
            }
            PaletteResponse::Closed => ActionToPerform::CloseActiveModalWindow.schedule(),
        }
    }
}
