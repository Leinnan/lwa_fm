//! The Full Disk Access suggestion: a banner under the toolbar while the
//! built app can't read every folder. With the grant, macOS stops asking
//! for Desktop, Documents, Downloads, iCloud Drive and external drives one
//! by one.

use super::App;
use std::time::Duration;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum DiskAccessPrompt {
    #[default]
    Hidden,
    /// Access is missing: offer to open System Settings.
    Ask,
    /// System Settings was opened: watch for the grant.
    Waiting,
    /// Granted, but only a new process gets it.
    Restart,
}

impl DiskAccessPrompt {
    /// The banner's state at launch.
    pub fn at_launch(ask: bool) -> Self {
        if ask && crate::shell::app_bundle().is_some() && !crate::shell::has_full_disk_access() {
            Self::Ask
        } else {
            Self::Hidden
        }
    }
}

impl App {
    pub(crate) fn disk_access_banner(&mut self, root: &mut egui::Ui) {
        if self.disk_access == DiskAccessPrompt::Hidden {
            return;
        }
        let ctx = root.ctx().clone();
        if self.disk_access == DiskAccessPrompt::Waiting {
            if crate::shell::has_full_disk_access() {
                self.disk_access = DiskAccessPrompt::Restart;
            } else {
                ctx.request_repaint_after(Duration::from_secs(1));
            }
        }
        let palette = egui_mev_theme::Theme::of(&ctx).palette(ctx.global_style().visuals.dark_mode);
        egui::Panel::top("disk_access")
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(palette.toolbar_bg)
                    .inner_margin(egui::Margin::symmetric(12, 6)),
            )
            .show(root, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(match self.disk_access {
                        DiskAccessPrompt::Restart => {
                            "Full Disk Access is on. Restart DirFleet to use it."
                        }
                        DiskAccessPrompt::Waiting => {
                            "Turn on DirFleet under Full Disk Access in System Settings."
                        }
                        _ => {
                            "Give DirFleet Full Disk Access to browse every folder \
                             without being asked for permission."
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.disk_access_buttons(ui);
                    });
                });
            });
    }

    fn disk_access_buttons(&mut self, ui: &mut egui::Ui) {
        if self.disk_access == DiskAccessPrompt::Restart {
            if ui.button("Restart DirFleet").clicked() {
                if let Err(error) = crate::shell::relaunch_after_exit() {
                    log::warn!("{error}");
                }
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
            if ui.button("Later").clicked() {
                self.disk_access = DiskAccessPrompt::Hidden;
            }
            return;
        }
        if ui.button("Don't ask again").clicked() {
            self.settings.ask_full_disk_access = false;
            self.disk_access = DiskAccessPrompt::Hidden;
        }
        if ui.button("Not now").clicked() {
            self.disk_access = DiskAccessPrompt::Hidden;
        }
        if ui.button("Open System Settings").clicked() {
            crate::shell::open_full_disk_access_settings();
            self.disk_access = DiskAccessPrompt::Waiting;
        }
    }
}
