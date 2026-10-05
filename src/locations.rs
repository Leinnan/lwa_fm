use std::{borrow::Cow, path::PathBuf, str::FromStr};

use egui::{Ui, Vec2};

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

impl Locations {
    pub fn draw_source_list(
        &self,
        title: &str,
        ui: &mut Ui,
        removable: bool,
        active: Option<&std::path::Path>,
    ) {
        use crate::app::ui::{self, Palette};
        if self.locations.is_empty() {
            return;
        }
        ui::section(ui, title);
        for location in &self.locations {
            let selected =
                active.is_some_and(|path| path == std::path::Path::new(location.path.as_ref()));
            let (rect, response) = ui.allocate_exact_size(
                Vec2::new(ui.available_width(), ui::nav_row_height(ui)),
                egui::Sense::click(),
            );
            response.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::SelectableLabel,
                    true,
                    selected,
                    location.name.as_ref(),
                )
            });
            if selected || response.hovered() || response.has_focus() {
                ui.painter().rect_filled(
                    rect,
                    ui::control_radius(ui),
                    ui::navigation_fill(ui, selected),
                );
            }
            if selected && ui::is_fluent(ui.ctx()) {
                ui.painter().rect_filled(
                    egui::Rect::from_center_size(
                        egui::pos2(rect.left() + 3.0, rect.center().y),
                        Vec2::new(3.0, 16.0),
                    ),
                    2,
                    Palette::of(ui).accent,
                );
            }
            if response.has_focus() {
                ui.painter().rect_stroke(
                    rect,
                    ui::control_radius(ui),
                    egui::Stroke::new(1.0_f32, Palette::of(ui).accent),
                    egui::StrokeKind::Inside,
                );
            }
            let icon = if removable {
                lucide_icons::Icon::Star
            } else {
                lucide_icons::Icon::Folder
            };
            ui.painter().text(
                rect.left_center() + Vec2::new(16.0, 0.0),
                egui::Align2::CENTER_CENTER,
                char::from(icon),
                egui::FontId::new(14.0, egui::FontFamily::Name("lucide".into())),
                Palette::of(ui).secondary,
            );
            let job = egui::text::LayoutJob::simple_singleline(
                location.name.to_string(),
                egui::TextStyle::Body.resolve(ui.style()),
                Palette::of(ui).text,
            );
            let mut job = job;
            job.wrap.max_width = (rect.width() - 42.0).max(0.0);
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
            ui.painter().galley(
                rect.left_center() + Vec2::new(32.0, -galley.size().y / 2.0),
                galley,
                Palette::of(ui).text,
            );
            let response = response.on_hover_text(location.path.as_ref());
            if response.clicked()
                && let Some(action) =
                    ActionToPerform::path_from_str(&location.path, ui.command_pressed())
            {
                action.schedule();
            }
            response.context_menu(|ui| {
                if ui.button("Open in new tab").clicked() {
                    ActionToPerform::NewTab(PathBuf::from(location.path.as_ref())).schedule();
                    ui.close();
                }
                if removable && ui.button("Remove from favorites").clicked() {
                    ActionToPerform::RemoveFromFavorites(location.path.clone()).schedule();
                    ui.close();
                }
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

        Self { locations }
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
