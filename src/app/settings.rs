use std::{path::Path, process::Command};

use egui::Modal;
use serde::{Deserialize, Serialize};

use crate::app::{directory_view_settings::DirectoryViewSettings, Sort};

use super::commands::ActionToPerform;

#[derive(Serialize, Deserialize)]
pub struct ApplicationSettings {
    pub terminal_path: String,
    pub directory_view_settings: DirectoryViewSettings,
}

impl Default for ApplicationSettings {
    fn default() -> Self {
        Self {
            #[cfg(not(target_os = "macos"))]
            terminal_path: "C:\\Program Files\\Alacritty\\alacritty.exe".into(),
            #[cfg(target_os = "macos")]
            terminal_path: "Terminal".into(),
            directory_view_settings: DirectoryViewSettings::default(),
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
        let mut action = None;
        let modal = Modal::new("Settings".into()).show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.heading("Settings");
                ui.separator();
                ui.add_space(10.0);
                ui.label("Terminal App");
                ui.text_edit_singleline(&mut self.terminal_path);
                ui.add_space(10.0);
                ui.separator();
                ui.label("Directory View");
                let mut changed = false;
                egui::Grid::new("directory_view")
                    .spacing([4., 4.])
                    .min_col_width(80.)
                    .num_columns(2)
                    .show(ui, |ui| {
                        let id = ui.label("Show hidden files").id;
                        changed |= ui
                            .checkbox(&mut self.directory_view_settings.show_hidden, "hidden")
                            .labelled_by(id)
                            .changed();
                        ui.end_row();
                        ui.label("Sorting");
                        let old_value = self.directory_view_settings.sorting;
                        egui::ComboBox::from_label("")
                            .selected_text(format!("↕ {:?}", self.directory_view_settings.sorting))
                            .show_ui(ui, |ui| {
                                ui.label("Sort by");
                                ui.separator();
                                ui.selectable_value(
                                    &mut self.directory_view_settings.sorting,
                                    Sort::Name,
                                    "Name",
                                );
                                ui.selectable_value(
                                    &mut self.directory_view_settings.sorting,
                                    Sort::Created,
                                    "Created",
                                );
                                ui.selectable_value(
                                    &mut self.directory_view_settings.sorting,
                                    Sort::Modified,
                                    "Modified",
                                );
                                ui.selectable_value(
                                    &mut self.directory_view_settings.sorting,
                                    Sort::Size,
                                    "Size",
                                );
                                ui.selectable_value(
                                    &mut self.directory_view_settings.sorting,
                                    Sort::Random,
                                    "Random",
                                );
                            });
                        changed |= old_value != self.directory_view_settings.sorting;
                        ui.end_row();
                        let id = ui.label("Invert Sort").id;
                        ui.add_enabled_ui(
                            self.directory_view_settings.sorting != Sort::Random,
                            |ui| {
                                changed |= ui
                                    .checkbox(
                                        &mut self.directory_view_settings.invert_sort,
                                        "invert_sort",
                                    )
                                    .labelled_by(id)
                                    .changed();
                            },
                        );
                        if changed {
                            action = Some(ActionToPerform::ViewSettingsChanged(
                                crate::app::DataSource::Settings,
                            ));
                        }
                    });
                ui.add_space(10.0);
                ui.separator();
                close = ui.button("Close").clicked();
            });
        });

        if modal.should_close() || close {
            action = Some(ActionToPerform::CloseActiveModalWindow);
        }
        action
    }
}
