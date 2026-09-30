use std::{path::Path, process::Command};

use egui::Modal;
use serde::{Deserialize, Serialize};

use crate::{
    app::{
        Sort,
        assets::IconSize,
        directory_view_settings::{DirectoryShowHidden, DirectoryViewSettings},
    },
    helper::DataHolder,
};

use super::{
    commands::ActionToPerform,
    ui::{self, Appearance},
};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum SettingsCategory {
    #[default]
    General,
    Appearance,
    Files,
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct ApplicationSettings {
    pub terminal_path: String,
    pub icon_size: IconSize,
    pub animate_selected_previews: bool,
    pub appearance: Appearance,
    pub sidebar_visible: bool,
    pub inspector_visible: bool,
    pub sidebar_width: f32,
    pub inspector_width: f32,
    #[serde(skip)]
    category: SettingsCategory,
}

impl Default for ApplicationSettings {
    fn default() -> Self {
        Self {
            #[cfg(not(target_os = "macos"))]
            terminal_path: "C:\\Program Files\\Alacritty\\alacritty.exe".into(),
            #[cfg(target_os = "macos")]
            terminal_path: "Terminal".into(),
            icon_size: IconSize::default(),
            animate_selected_previews: false,
            appearance: Appearance::System,
            sidebar_visible: true,
            inspector_visible: false,
            sidebar_width: 200.0,
            inspector_width: 280.0,
            category: SettingsCategory::General,
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
    #[allow(clippy::too_many_lines)]
    pub(crate) fn display(&mut self, ctx: &egui::Context) {
        let modal = Modal::new("Settings".into()).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 64.0).clamp(300.0, 520.0));
            ui.heading("Settings");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.category, SettingsCategory::General, "General");
                ui.selectable_value(
                    &mut self.category,
                    SettingsCategory::Appearance,
                    "Appearance",
                );
                ui.selectable_value(&mut self.category, SettingsCategory::Files, "Files");
            });
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(ctx.content_rect().height() - 160.0)
                .show(ui, |ui| match self.category {
                    SettingsCategory::General => {
                        ui::section(ui, "Applications");
                        ui::card(ui).show(ui, |ui| {
                            ui::form_row(ui, "Terminal app", |ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.terminal_path)
                                        .desired_width(ui.available_width()),
                                );
                            });
                        });
                    }
                    SettingsCategory::Appearance => {
                        ui::section(ui, "Interface");
                        ui::card(ui).show(ui, |ui| {
                            ui::form_row(ui, "Appearance", |ui| {
                                egui::ComboBox::from_id_salt("appearance")
                                    .selected_text(format!("{:?}", self.appearance))
                                    .show_ui(ui, |ui| {
                                        for value in [
                                            Appearance::System,
                                            Appearance::Light,
                                            Appearance::Dark,
                                        ] {
                                            ui.selectable_value(
                                                &mut self.appearance,
                                                value,
                                                format!("{value:?}"),
                                            );
                                        }
                                    });
                            });
                            ui.checkbox(&mut self.sidebar_visible, "Show sidebar");
                            ui.checkbox(&mut self.inspector_visible, "Show file inspector");
                            ui.weak("Side panels collapse automatically in smaller windows.");
                        });
                    }
                    SettingsCategory::Files => {
                        ui::section(ui, "Directory defaults");
                        let mut view = ui
                            .data_get_persisted::<DirectoryViewSettings>()
                            .unwrap_or_default();
                        let mut hidden = ui
                            .data_get_persisted::<DirectoryShowHidden>()
                            .unwrap_or_default();
                        let before = (view.sorting, view.invert_sort, hidden.0);
                        ui::card(ui).show(ui, |ui| {
                            ui.checkbox(&mut hidden.0, "Show hidden files");
                            ui::form_row(ui, "Sort by", |ui| {
                                egui::ComboBox::from_id_salt("default_sort")
                                    .selected_text(format!("{:?}", view.sorting))
                                    .show_ui(ui, |ui| {
                                        for value in [
                                            Sort::Name,
                                            Sort::Created,
                                            Sort::Modified,
                                            Sort::Size,
                                            Sort::Random,
                                        ] {
                                            ui.selectable_value(
                                                &mut view.sorting,
                                                value,
                                                format!("{value:?}"),
                                            );
                                        }
                                    });
                            });
                            ui.add_enabled_ui(view.sorting != Sort::Random, |ui| {
                                ui.checkbox(&mut view.invert_sort, "Reverse order");
                            });
                        });
                        if before != (view.sorting, view.invert_sort, hidden.0) {
                            ui.data_set_persisted(view);
                            ui.data_set_persisted(hidden);
                            ActionToPerform::ViewSettingsChanged(crate::app::DataSource::Settings)
                                .schedule();
                        }
                        ui::section(ui, "Previews");
                        ui::card(ui).show(ui, |ui| {
                            ui.checkbox(
                                &mut self.animate_selected_previews,
                                "Animate selected videos and GIFs",
                            );
                            ui::form_row(ui, "Icon size", |ui| {
                                egui::ComboBox::from_id_salt("icon_size")
                                    .selected_text(format!("{:?}", self.icon_size))
                                    .show_ui(ui, |ui| {
                                        for value in [
                                            IconSize::Small,
                                            IconSize::Medium,
                                            IconSize::Large,
                                            IconSize::ExtraLarge,
                                        ] {
                                            ui.selectable_value(
                                                &mut self.icon_size,
                                                value,
                                                format!("{value:?}"),
                                            );
                                        }
                                    });
                            });
                        });
                    }
                });
            ui.add_space(12.0);
            if ui.button("Close").clicked() {
                ActionToPerform::CloseActiveModalWindow.schedule();
            }
        });
        self.appearance.apply(ctx);
        if modal.should_close() {
            ActionToPerform::CloseActiveModalWindow.schedule();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_settings_receive_new_defaults() {
        let settings: ApplicationSettings = serde_json::from_str(
            r#"{"terminal_path":"Terminal","animate_selected_previews":true}"#,
        )
        .expect("legacy settings");
        assert_eq!(settings.appearance, Appearance::System);
        assert!(settings.sidebar_visible);
        assert!(!settings.inspector_visible);
        assert!((settings.sidebar_width - 200.0).abs() < f32::EPSILON);
    }
    #[test]
    fn panel_preferences_round_trip() {
        let settings = ApplicationSettings {
            appearance: Appearance::Light,
            inspector_visible: true,
            sidebar_width: 225.0,
            ..Default::default()
        };
        let encoded = serde_json::to_string(&settings).expect("encode");
        let restored: ApplicationSettings = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(restored.appearance, Appearance::Light);
        assert!(restored.inspector_visible);
        assert!((restored.sidebar_width - 225.0).abs() < f32::EPSILON);
    }
}
