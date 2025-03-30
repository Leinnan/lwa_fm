use std::path::Path;

use egui::Modal;

use super::commands::ActionToPerform;

#[derive(Default)]
pub struct CommandPalette {
    pub commands: Vec<ValidAction>,
    pub visible: bool,
}

pub struct ValidAction {
    pub action: ActionToPerform,
    pub name: String,
}

impl CommandPalette {
    pub fn build_for_path(&mut self, path: &Path) {
        self.commands.clear();

        self.commands.push(ValidAction {
            action: ActionToPerform::OpenInTerminal(path.to_path_buf()),
            name: "Open in Terminal".to_string(),
        });
        if let Some(parent) = path.parent() {
            self.commands.push(ValidAction {
                action: ActionToPerform::ChangePath(parent.to_path_buf()),
                name: "Go Up".to_string(),
            });
        }
    }

    pub fn ui(&self, ctx: &egui::Context) -> Option<ActionToPerform> {
        if !self.visible {
            return None;
        }
        let mut action = None;
        Modal::new("Settings".into()).show(ctx, |ui| {
            ui.vertical_centered_justified(|ui| {
                ui.label("Run Command");
                ui.separator();
                for a in &self.commands {
                    if ui.button(&a.name).clicked() {
                        action = Some(a.action.clone());
                    }
                }
            });
        });
        action
    }
}
