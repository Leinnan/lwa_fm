use std::{path::Path, process::Command};

use egui::Modal;
use serde::{Deserialize, Serialize};

use super::commands::ActionToPerform;

#[derive(Serialize, Deserialize)]
pub struct ApplicationSettings {
    pub terminal_path: String,
}

impl Default for ApplicationSettings {
    fn default() -> Self {
        Self {
            #[cfg(not(target_os = "macos"))]
            terminal_path: "C:\\Program Files\\Alacritty\\alacritty.exe".into(),
            #[cfg(target_os = "macos")]
            terminal_path: "Terminal".into(),
        }
    }
}

impl ApplicationSettings {
    #[allow(clippy::unused_self)]
    pub fn open_in_terminal<P>(&self, directory: P) -> std::io::Result<std::process::Child>
    where
        P: AsRef<Path>,
    {
        #[cfg(target_os = "macos")]
        {
            Command::new("open")
                .current_dir(directory)
                .arg("-a")
                .arg(&self.terminal_path)
                .arg(".")
                .spawn()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Command::new(&self.terminal_path)
                .current_dir(directory)
                .spawn()
        }
    }

    /// Display the settings modal.
    /// returns true if the modal was closed.
    pub(crate) fn display(&mut self, ctx: &egui::Context) -> Option<ActionToPerform> {
        let mut close = false;
        let modal = Modal::new("Settings".into()).show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.heading("Settings");
                ui.separator();
                ui.add_space(10.0);
                ui.label("Terminal App");
                ui.text_edit_singleline(&mut self.terminal_path);
                ui.add_space(10.0);
                ui.separator();
                close = ui.button("Close").clicked();
            });
        });

        if modal.should_close() || close {
            Some(ActionToPerform::CloseActiveModalWindow)
        } else {
            None
        }
    }
}
