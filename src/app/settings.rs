use std::{path::Path, process::Command};

use egui::Modal;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct ApplicationSettings {
    pub terminal_path: String,
}

impl Default for ApplicationSettings {
    fn default() -> Self {
        Self {
            terminal_path: "C:\\Program Files\\Alacritty\\alacritty.exe".into(),
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
                .arg("Terminal")
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
    pub(crate) fn display(&mut self, ctx: &egui::Context) -> bool {
        let mut close = false;
        let modal = Modal::new("Settings".into()).show(ctx, |ui| {
            ui.label("Terminal Path");
            ui.text_edit_singleline(&mut self.terminal_path);
            ui.separator();
            close = ui.button("Close").clicked();
        });

        modal.should_close() || close
    }
}
