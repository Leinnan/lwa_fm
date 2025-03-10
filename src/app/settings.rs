use std::{path::Path, process::Command};

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
    pub fn open_in_terminal<P>(&self, directory: P) -> std::io::Result<std::process::Child>
    where
        P: AsRef<Path>,
    {
        #[cfg(windows)]
        {
            Command::new(&self.terminal_path)
                .current_dir(directory)
                .spawn()
        }

        #[cfg(target_os = "macos")]
        {
            Command::new("open")
                .current_dir(directory)
                .arg("-a")
                .arg("Terminal")
                .arg(".")
                .spawn()
        }
    }
}
