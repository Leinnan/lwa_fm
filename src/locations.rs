use std::{borrow::Cow, path::PathBuf, str::FromStr};

use egui::Ui;

use crate::{
    app::commands::ActionToPerform,
    helper::{DataHolder, KeyWithCommandPressed},
};

#[derive(serde::Deserialize, serde::Serialize, Default, Debug, Clone)]
#[serde(default)]
pub struct Locations {
    pub locations: Vec<Location>,
}
impl Locations {
    /// Returns a vector of paths for each location.
    pub fn paths(&self) -> Vec<PathBuf> {
        self.locations
            .iter()
            .filter_map(|s| PathBuf::from_str(&s.path).ok())
            .collect()
    }
}

impl Locations {
    /// Draws one row per location; the caller draws the section heading.
    pub fn draw_source_list(&self, ui: &mut Ui, removable: bool, active: Option<&std::path::Path>) {
        use crate::app::ui;
        for location in &self.locations {
            let selected =
                active.is_some_and(|path| path == std::path::Path::new(location.path.as_ref()));
            let icon = if removable {
                lucide_icons::Icon::Star
            } else {
                lucide_icons::Icon::Folder
            };
            let response = ui::nav_row(ui, icon, &location.name, selected, egui::Sense::click());
            let response = response.on_hover_text(location.path.as_ref());
            // Ctrl+click or middle-click opens in a new tab, as in a browser.
            let new_tab = response.middle_clicked() || (response.clicked() && ui.command_pressed());
            if (response.clicked() || new_tab)
                && let Some(action) = ActionToPerform::path_from_str(&location.path, new_tab)
            {
                action.schedule();
            }
            crate::app::file_ops::folder_drop_target(
                ui,
                &response,
                std::path::Path::new(location.path.as_ref()),
            );
            response.context_menu(|ui| {
                let favorites = if removable {
                    self.clone()
                } else {
                    ui.data_get_persisted::<Self>().unwrap_or_default()
                };
                crate::app::menus::folder_actions(
                    ui,
                    std::path::Path::new(location.path.as_ref()),
                    &favorites,
                    true,
                );
            });
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize, Default, Debug, Clone)]
#[serde(default)]
pub struct Location {
    pub name: Cow<'static, str>,
    pub path: Cow<'static, str>,
}

impl Location {
    pub fn from_path(path: impl Into<PathBuf>, name: impl Into<String>) -> Self {
        let path_buf = path.into();
        let path = Cow::Owned(String::from(path_buf.to_string_lossy()));
        Self {
            name: Cow::Owned(name.into()),
            path,
        }
    }
}

impl Locations {
    pub fn get_user_dirs() -> Self {
        let locations: Vec<Location> =
            directories::UserDirs::new().map_or_else(Vec::new, |user_dirs| {
                let mut list = vec![Location::from_path(user_dirs.home_dir(), "User")];
                if let Some(docs) = user_dirs.document_dir() {
                    list.push(Location::from_path(docs, "Documents"));
                }
                if let Some(dir) = user_dirs.desktop_dir() {
                    list.push(Location::from_path(dir, "Desktop"));
                }
                if let Some(dir) = user_dirs.download_dir() {
                    list.push(Location::from_path(dir, "Downloads"));
                }
                if let Some(dir) = user_dirs.picture_dir() {
                    list.push(Location::from_path(dir, "Pictures"));
                }
                if let Some(dir) = user_dirs.audio_dir() {
                    list.push(Location::from_path(dir, "Music"));
                }
                list
            });

        Self { locations }
    }
}
