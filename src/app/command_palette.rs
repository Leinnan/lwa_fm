use std::{borrow::Cow, path::Path};

use egui::{Modal, TextBuffer};

use crate::{app::dock::CurrentPath, locations::Locations};

use super::commands::ActionToPerform;

#[derive(Default, Debug, Clone)]
pub struct CommandPalette {
    pub commands: Vec<ValidAction>,
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
                    action: ActionToPerform::ChangePaths(parent.to_path_buf().into()),
                    name: "Go Up".into(),
                });
            }
        } else {
            commands.push(ValidAction {
                action: ActionToPerform::ChangePaths(path.to_path_buf().into()),
                name: "Open".into(),
            });
            commands.push(ValidAction {
                action: ActionToPerform::NewTab(path.to_path_buf()),
                name: "Open in new tab".into(),
            });
        }

        #[cfg(windows)]
        let open_name = "Open in Explorer";
        #[cfg(target_os = "macos")]
        let open_name = "Open in Finder";
        #[cfg(target_os = "linux")]
        let open_name = "Open in File Manager";

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
    }

    pub fn ui(&self, ctx: &egui::Context) -> Option<ActionToPerform> {
        let mut action = None;
        let modal = Modal::new("Commands".into())
            .frame(egui::Frame::canvas(&ctx.style()))
            .show(ctx, |ui| {
                ui.vertical_centered_justified(|ui| {
                    ui.heading("Run Command");
                    ui.separator();
                    for a in &self.commands {
                        if ui.button(a.name.as_str()).clicked() {
                            action = Some(a.action.clone());
                        }
                    }
                });
            });
        if action.is_none() && modal.should_close() {
            Some(ActionToPerform::CloseActiveModalWindow)
        } else {
            action
        }
    }
}
