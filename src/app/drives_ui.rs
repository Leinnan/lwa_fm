//! Drives in the app: the sidebar section, ejecting, and moving tabs off a
//! drive that goes away so nothing keeps it busy.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use egui::{Context, Ui};

use super::{
    App,
    commands::{ActionToPerform, COMMANDS_QUEUE, ModalWindow, TabAction, TabTarget},
    dock::CurrentPath,
    file_ops::escape_pressed,
    ui::{self, Palette},
};
use crate::{
    drives::{self, Drive, Monitor},
    helper::format_bytes_simple,
    locations::Locations,
    toast,
};

/// How long an ejected drive stays hidden while the OS finishes removing it.
const EJECTED_GRACE: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct DrivesState {
    pub list: Vec<Drive>,
    monitor: Option<Monitor>,
    /// Drives whose eject is running in the background.
    ejecting: HashSet<PathBuf>,
    /// Drives ejected moments ago: hidden, and their disappearance is no
    /// surprise worth a message.
    ejected: HashMap<PathBuf, Instant>,
    /// The drive the "operations still running" dialog asks about.
    confirm: Option<Drive>,
}

impl DrivesState {
    /// List the drives now and keep watching for changes.
    pub fn start() -> Self {
        let list = drives::list();
        Self {
            monitor: Some(Monitor::start(list.clone())),
            list,
            ..Default::default()
        }
    }

    /// A fixed list, without monitoring (tests and snapshots).
    #[cfg(test)]
    pub fn fixed(list: Vec<Drive>) -> Self {
        Self {
            list,
            ..Default::default()
        }
    }

    /// The drives to show: all but those just ejected.
    pub fn visible(&self) -> impl Iterator<Item = &Drive> {
        self.list
            .iter()
            .filter(|drive| !self.ejected.contains_key(&drive.mount))
    }

    /// The drive `path` is on.
    pub fn drive_for(&self, path: &Path) -> Option<&Drive> {
        drives::drive_for_path(&self.list, path)
    }

    pub fn is_ejecting(&self, mount: &Path) -> bool {
        self.ejecting.contains(mount)
    }

    fn refresh_now(&self) {
        if let Some(monitor) = &self.monitor {
            monitor.refresh_now();
        }
    }
}

/// Where a tab showing `current` goes when the drive at `mount` disappears:
/// `None` when nothing it shows is on that drive.
fn path_off_drive(current: &CurrentPath, mount: &Path, home: &Path) -> Option<CurrentPath> {
    match current {
        CurrentPath::One(path) if drives::is_under(path, mount) => {
            Some(CurrentPath::One(home.to_path_buf()))
        }
        CurrentPath::Multiple(paths) if paths.iter().any(|p| drives::is_under(p, mount)) => {
            let mut rest: Vec<PathBuf> = paths
                .iter()
                .filter(|p| !drives::is_under(p, mount))
                .cloned()
                .collect();
            Some(match rest.len() {
                0 => CurrentPath::One(home.to_path_buf()),
                1 => CurrentPath::One(rest.remove(0)),
                _ => CurrentPath::Multiple(rest),
            })
        }
        _ => None,
    }
}

/// The folder tabs move to when their drive goes away: the home folder, or
/// any other drive when home is on the drive being removed.
fn fallback_folder(mount: &Path, drives: &[Drive]) -> PathBuf {
    directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .filter(|home| !drives::is_under(home, mount))
        .or_else(|| {
            drives
                .iter()
                .find(|drive| drive.mount != mount)
                .map(|drive| drive.mount.clone())
        })
        .unwrap_or_else(std::env::temp_dir)
}

impl App {
    /// Apply the latest drive list from the monitor: tell the user about
    /// drives that came and went, and let go of the ones that went.
    pub(crate) fn poll_drives(&mut self, ctx: &Context) {
        self.drives
            .ejected
            .retain(|_, when| when.elapsed() < EJECTED_GRACE);
        let Some(latest) = self.drives.monitor.as_ref().and_then(Monitor::latest) else {
            return;
        };
        let (added, removed) = drives::diff(&self.drives.list, &latest);
        let notify = self.settings.notify_drive_changes;
        for drive in &removed {
            if self.drives.ejected.remove(&drive.mount).is_some() {
                continue;
            }
            self.drives.ejecting.remove(&drive.mount);
            for id in self.file_ops.running_on(&drive.mount) {
                self.file_ops.cancel(id);
            }
            let moved = self.release_drive(ctx, &drive.mount, &latest);
            if notify && (drive.ejectable() || moved > 0) {
                let name = drive.display_name();
                if moved > 0 {
                    toast!(
                        Warning,
                        "{name} was disconnected. Tabs on it moved to your home folder."
                    );
                } else {
                    toast!(Warning, "{name} was disconnected");
                }
            }
        }
        self.drives.list = latest;
        self.sync_drive_targets();
        if notify {
            for drive in added.iter().filter(|drive| drive.ejectable()) {
                toast!(Info, "{} connected", drive.display_name());
            }
        }
    }

    /// Tell the tabs which removable drives to offer as transfer targets.
    pub(crate) fn sync_drive_targets(&mut self) {
        self.tabs.removable_drives = self
            .drives
            .list
            .iter()
            .filter(|drive| drive.kind == drives::DriveKind::Removable)
            .map(|drive| drive.mount.clone())
            .collect();
    }

    /// Free space of the drive `folder` is on, and an eject button when
    /// it can be ejected, for the status bar.
    pub(crate) fn drive_status(&self, ui: &mut Ui, folder: Option<&Path>) {
        let Some(drive) = folder
            .filter(|folder| folder.is_absolute())
            .and_then(|folder| self.drives.drive_for(folder))
        else {
            return;
        };
        if drive.total == 0 && !drive.ejectable() {
            return;
        }
        ui.separator();
        if drive.total > 0 {
            ui.weak(format!("{} free", format_bytes_simple(drive.available)))
                .on_hover_text(drive_tooltip(drive));
        }
        if drive.ejectable() {
            if self.drives.is_ejecting(&drive.mount) {
                ui.spinner();
            } else if ui::icon_button(
                ui,
                lucide_icons::Icon::Eject,
                &format!("{} {}", drive.eject_label(), drive.display_name()),
                false,
            )
            .clicked()
            {
                ActionToPerform::EjectDrive {
                    mount: drive.mount.clone(),
                    force: false,
                }
                .schedule();
            }
        }
    }

    /// Eject the drive at `mount`, asking first when file operations are
    /// still running on it unless `force`.
    pub(crate) fn eject_drive(&mut self, ctx: &Context, mount: &Path, force: bool) {
        let Some(drive) = self.drives.list.iter().find(|d| d.mount == mount).cloned() else {
            return;
        };
        if self.drives.is_ejecting(&drive.mount) {
            return;
        }
        let busy = self.file_ops.running_on(&drive.mount);
        if !busy.is_empty() && !force {
            self.drives.confirm = Some(drive);
            self.display_modal = Some(ModalWindow::ConfirmEject);
            return;
        }
        for id in busy {
            self.file_ops.cancel(id);
        }
        // Our own watches, listings and thumbnail readers would veto the
        // eject, so let go of everything on the drive first.
        let list = self.drives.list.clone();
        self.release_drive(ctx, &drive.mount, &list);
        self.drives.ejecting.insert(drive.mount.clone());
        let name = drive.display_name();
        let repaint = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("drive-eject".into())
            .spawn(move || {
                let result = drives::eject(&drive).map_err(|err| format!("{err:#}"));
                COMMANDS_QUEUE.push(ActionToPerform::DriveEjected {
                    mount: drive.mount.clone(),
                    name: drive.display_name(),
                    result,
                });
                repaint.request_repaint();
            });
        if spawned.is_err() {
            self.drives.ejecting.remove(mount);
            toast!(Error, "Could not eject {name}");
        }
    }

    pub(crate) fn drive_ejected(&mut self, mount: PathBuf, name: &str, result: Result<(), String>) {
        self.drives.ejecting.remove(&mount);
        match result {
            Ok(()) => {
                let network = self
                    .drives
                    .list
                    .iter()
                    .any(|d| d.mount == mount && d.kind == drives::DriveKind::Network);
                self.drives.ejected.insert(mount, Instant::now());
                if network {
                    toast!(Success, "Disconnected {name}");
                } else {
                    toast!(Success, "{name} can be safely removed");
                }
            }
            Err(message) => {
                toast!(Error, "Could not eject {name}: {message}");
            }
        }
        self.drives.refresh_now();
    }

    /// Move tabs off the drive at `mount` and drop every watch, queued
    /// listing refresh and asset job on it. Returns how many tabs moved.
    fn release_drive(&mut self, ctx: &Context, mount: &Path, drives: &[Drive]) -> usize {
        let fallback = fallback_folder(mount, drives);
        let mut moved = 0;
        for id in self.tabs.get_tab_ids() {
            let Some(tab) = self.tabs.get_tab_by_id(id) else {
                continue;
            };
            let Some(next) = path_off_drive(&tab.current_path, mount, &fallback) else {
                continue;
            };
            moved += 1;
            self.handle_action(
                ctx,
                ActionToPerform::TabAction(TabTarget::TabWithId(id), TabAction::ChangePaths(next)),
            );
        }
        self.watchers.stop_under(mount);
        self.pending_structural_dirs
            .retain(|dir, _| !drives::is_under(dir, mount));
        self.assets.invalidate_directories([mount.to_path_buf()]);
        moved
    }

    /// The Drives sidebar section: one row per drive with its capacity, and
    /// an eject button on drives that can be ejected.
    pub(crate) fn drives_sidebar(&self, ui: &mut Ui, active: Option<&Path>, favorites: &Locations) {
        for drive in self.drives.visible() {
            let mount = drive.mount.as_path();
            let selected = active == Some(mount);
            let response = ui::nav_row(
                ui,
                drive.kind.icon(),
                &drive.display_name(),
                selected,
                egui::Sense::click(),
            );
            let row = response.rect;
            let ejecting = self.drives.is_ejecting(mount);
            let eject_rect = drive.ejectable().then(|| {
                egui::Rect::from_center_size(
                    egui::pos2(row.right() - 14.0, row.center().y),
                    egui::Vec2::splat(22.0),
                )
            });
            if self.settings.show_drive_capacity
                && let Some(used) = drive.used_fraction()
            {
                let right = eject_rect.map_or_else(|| row.right() - 8.0, |r| r.left() - 4.0);
                capacity_bar(ui, row, right, used);
            }
            let eject_clicked = eject_rect.is_some_and(|rect| {
                eject_button(ui, rect, drive, ejecting, response.hovered() || selected)
            });
            if eject_clicked {
                ActionToPerform::EjectDrive {
                    mount: drive.mount.clone(),
                    force: false,
                }
                .schedule();
            }
            let response = response.on_hover_text(drive_tooltip(drive));
            let new_tab = response.middle_clicked()
                || (response.clicked() && ui.input(|i| i.modifiers.command));
            if !eject_clicked
                && (response.clicked() || new_tab)
                && let Some(action) =
                    ActionToPerform::path_from_str(mount.to_string_lossy(), new_tab)
            {
                action.schedule();
            }
            super::file_ops::folder_drop_target(ui, &response, mount);
            response.context_menu(|ui| {
                super::menus::folder_actions(ui, mount, favorites, true);
                if drive.ejectable() {
                    ui.separator();
                    let button = ui.add_enabled(!ejecting, egui::Button::new(drive.eject_label()));
                    if button.clicked() {
                        ActionToPerform::EjectDrive {
                            mount: drive.mount.clone(),
                            force: false,
                        }
                        .schedule();
                        ui.close();
                    }
                }
            });
        }
    }

    /// Draws `ModalWindow::ConfirmEject`: file operations are still running
    /// on the drive being ejected.
    pub(crate) fn confirm_eject_ui(&mut self, ctx: &Context) {
        let Some(drive) = self.drives.confirm.clone() else {
            self.display_modal = None;
            return;
        };
        let running = self.file_ops.running_on(&drive.mount).len();
        let mut confirmed = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new(ModalWindow::ConfirmEject)).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 64.0).clamp(280.0, 440.0));
            ui.heading(format!("Eject {}?", drive.display_name()));
            ui.label(match running {
                0 => "The file operations on this drive have finished.".to_owned(),
                1 => "A file operation is still running on this drive. Ejecting cancels it."
                    .to_owned(),
                n => format!(
                    "{n} file operations are still running on this drive. Ejecting cancels them."
                ),
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                confirmed = ui.button(drive.eject_label()).clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        if confirmed {
            self.drives.confirm = None;
            ActionToPerform::EjectDrive {
                mount: drive.mount,
                force: true,
            }
            .schedule();
            ActionToPerform::CloseActiveModalWindow.schedule();
        } else if cancel || modal.should_close() || escape_pressed(ctx) {
            self.drives.confirm = None;
            ActionToPerform::CloseActiveModalWindow.schedule();
        }
    }

    /// Palette entries: go to each drive, and eject the ejectable ones.
    pub(crate) fn drive_palette_commands(&self) -> Vec<super::command_palette::ValidAction> {
        let mut commands = Vec::new();
        for drive in self.drives.visible() {
            commands.push(super::command_palette::ValidAction {
                action: ActionToPerform::TabAction(
                    TabTarget::ActiveTab,
                    TabAction::ChangePaths(drive.mount.clone().into()),
                ),
                name: format!("Go to drive: {}", drive.display_name()).into(),
                shortcut: None,
            });
        }
        for drive in self.drives.visible().filter(|drive| drive.ejectable()) {
            commands.push(super::command_palette::ValidAction {
                action: ActionToPerform::EjectDrive {
                    mount: drive.mount.clone(),
                    force: false,
                },
                name: format!("{}: {}", drive.eject_label(), drive.display_name()).into(),
                shortcut: None,
            });
        }
        commands
    }
}

/// A thin bar along the bottom of a drive row showing how full it is.
fn capacity_bar(ui: &Ui, row: egui::Rect, right: f32, used: f32) {
    let track = egui::Rect::from_min_max(
        egui::pos2(row.left() + 32.0, row.bottom() - 4.0),
        egui::pos2(right, row.bottom() - 2.0),
    );
    let palette = Palette::of(ui);
    ui.painter().rect_filled(track, 1, palette.border);
    let mut fill = track;
    fill.set_width(track.width() * used);
    let color = if used > 0.9 {
        egui::Color32::from_rgb(232, 72, 85)
    } else {
        palette.accent
    };
    ui.painter().rect_filled(fill, 1, color);
}

/// The eject button at the end of a drive row, shown while the row is
/// hovered or selected, or a spinner while the eject runs. Returns whether
/// it was clicked.
fn eject_button(ui: &Ui, rect: egui::Rect, drive: &Drive, ejecting: bool, show: bool) -> bool {
    if ejecting {
        egui::Spinner::new()
            .size(12.0)
            .paint_at(ui, rect.shrink(4.0));
        return false;
    }
    let button = ui
        .interact(
            rect,
            ui.id().with(("eject", &drive.mount)),
            egui::Sense::click(),
        )
        .on_hover_text(drive.eject_label());
    if show || button.hovered() {
        if button.hovered() {
            ui.painter()
                .rect_filled(rect, ui::control_radius(ui), ui::navigation_fill(ui, false));
        }
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            char::from(lucide_icons::Icon::Eject),
            egui::FontId::new(13.0, egui::FontFamily::Name("lucide".into())),
            Palette::of(ui).secondary,
        );
    }
    button.clicked()
}

fn drive_tooltip(drive: &Drive) -> String {
    let mut text = drive.mount.display().to_string();
    if drive.total > 0 {
        text = format!(
            "{text}\n{} free of {}",
            format_bytes_simple(drive.available),
            format_bytes_simple(drive.total)
        );
    }
    if !drive.file_system.is_empty() {
        text = format!("{text} · {}", drive.file_system);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(path: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:{}", path.replace('/', "\\")))
        } else {
            PathBuf::from(path)
        }
    }

    #[test]
    fn tabs_on_the_drive_go_home() {
        let (mount, home) = (p("/media/usb"), p("/home/me"));
        let on_drive = CurrentPath::One(mount.join("photos"));
        assert_eq!(
            path_off_drive(&on_drive, &mount, &home),
            Some(CurrentPath::One(home.clone()))
        );
        let elsewhere = CurrentPath::One(p("/home/me/docs"));
        assert_eq!(path_off_drive(&elsewhere, &mount, &home), None);
        assert_eq!(path_off_drive(&CurrentPath::Trash, &mount, &home), None);
    }

    #[test]
    fn multi_folder_tabs_drop_only_the_drive() {
        let (mount, home) = (p("/media/usb"), p("/home/me"));
        let both = CurrentPath::Multiple(vec![mount.join("a"), p("/home/me/b")]);
        assert_eq!(
            path_off_drive(&both, &mount, &home),
            Some(CurrentPath::One(p("/home/me/b")))
        );
        let all_on_drive = CurrentPath::Multiple(vec![mount.join("a"), mount.join("b")]);
        assert_eq!(
            path_off_drive(&all_on_drive, &mount, &home),
            Some(CurrentPath::One(home))
        );
    }
}
