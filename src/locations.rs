use std::{borrow::Cow, path::PathBuf, str::FromStr};

use egui::{Align, Layout, RichText, TextBuffer, Ui};

use crate::{app::commands::ActionToPerform, helper::KeyWithCommandPressed};

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

const fn empty_icon(_ui: &mut egui::Ui, _openness: f32, _response: &egui::Response) {
    // Empty icon function
}

impl Locations {
    pub fn draw_ui(&self, id: &str, ui: &mut Ui, removable: bool) -> Option<ActionToPerform> {
        if self.locations.is_empty() {
            return None;
        }
        let mut action = None;
        egui::CollapsingHeader::new(RichText::new(id).weak().size(21.0))
            .icon(empty_icon)
            .default_open(true)
            .show(ui, |ui| {
                ui.with_layout(
                    Layout::top_down(Align::Min).with_cross_justify(true),
                    |ui| {
                        for location in &self.locations {
                            let button = ui.add(
                                egui::Button::new(location.name.as_str())
                                    .frame(false)
                                    .fill(egui::Color32::from_white_alpha(0)),
                            );
                            if button.clicked() {
                                action = if ui.command_pressed() {
                                    ActionToPerform::NewTab(
                                        PathBuf::from_str(&location.path).unwrap(),
                                    )
                                } else {
                                    ActionToPerform::ChangePaths(
                                        PathBuf::from_str(&location.path).unwrap().into(),
                                    )
                                }
                                .into();
                                return;
                            }
                            button.context_menu(|ui| {
                                if ui.button("Open in new tab").clicked() {
                                    action = Some(ActionToPerform::NewTab(
                                        PathBuf::from_str(&location.path).unwrap(),
                                    ));
                                    ui.close();
                                    return;
                                }

                                if removable && ui.button("Remove from favorites").clicked() {
                                    action = Some(ActionToPerform::RemoveFromFavorites(
                                        location.path.clone(),
                                    ));
                                    ui.close();
                                }
                            });
                        }
                    },
                );
            });
        action
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
    #[cfg(not(target_os = "macos"))]
    pub fn get_drives() -> Self {
        let mut drives = sysinfo::Disks::new_with_refreshed_list();
        drives.sort_by(|a, b| a.mount_point().cmp(b.mount_point()));
        let locations = drives
            .iter()
            .map(|drive| {
                Location::from_path(
                    drive.mount_point(),
                    format!(
                        "{} ({})",
                        drive.name().to_str().unwrap_or(""),
                        drive.mount_point().display()
                    ),
                )
            })
            .collect();

        Self {
            locations,
            editable: false,
        }
    }

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
