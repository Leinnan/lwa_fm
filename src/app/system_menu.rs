//! Opens Explorer's context menu once the app's own menu is off the screen.
use super::App;
use super::commands::{ActionToPerform, in_window};
use super::dock::WindowId;
use super::keymap::Command;
use crate::shell::context_menu::{self, Outcome};
use crate::toast;
use std::path::PathBuf;

/// A native menu waiting for a frame without the app's own menu.
pub(super) struct PendingSystemMenu {
    paths: Vec<PathBuf>,
    window: WindowId,
    frames_to_wait: u8,
}

impl App {
    pub(super) fn request_system_menu(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>) {
        self.pending_system_menu = Some(PendingSystemMenu {
            paths,
            window: self.tabs.current_window(),
            frames_to_wait: 1,
        });
        ctx.request_repaint();
    }

    /// The native menu blocks the UI thread until it closes, and what's on
    /// screen stays there meanwhile. The frame with the click still paints the
    /// app's menu, so wait until one without it has been painted.
    pub(super) fn show_pending_system_menu(&mut self, ctx: &egui::Context) {
        let Some(pending) = &mut self.pending_system_menu else {
            return;
        };
        ctx.request_repaint();
        if pending.frames_to_wait > 0 {
            pending.frames_to_wait -= 1;
            return;
        }
        let Some(pending) = self.pending_system_menu.take() else {
            return;
        };
        // Whichever viewport has focus: the click that asked for the menu
        // activated it.
        match context_menu::show(crate::shell::active_window(), &pending.paths) {
            Ok(Outcome::Done) => {}
            Ok(Outcome::Rename) => in_window(pending.window, || {
                ActionToPerform::RunCommand(Command::Rename).schedule();
            }),
            Err(error) => {
                toast!(Error, "Could not show the Windows context menu: {error}");
            }
        }
    }
}
