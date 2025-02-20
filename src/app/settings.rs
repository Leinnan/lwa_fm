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
