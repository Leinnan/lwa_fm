//! Full Disk Access, as egui's banner: while a built app can't read every
//! folder, offer System Settings; once opened, check every second for the
//! grant; once granted, offer to restart, since only a new process gets it.
//! Also whether the terminal app in the settings exists, and what the About
//! section says.
use crux_core::Command;
use crux_core::capability::Operation;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::{Delay, DirFleet, Effect, Event, Model, SystemOp};

/// How often the grant is checked while System Settings is open.
const RECHECK_MILLIS: u64 = 1000;

/// Whether the app has Full Disk Access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskAccessCheck;

/// What the check found. Opaque to facet: it never leaves the core.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Facet)]
#[facet(opaque)]
pub struct DiskAccess {
    /// The grant matters: a built app on macOS (not tests, not other systems).
    pub applies: bool,
    pub granted: bool,
}

impl Operation for DiskAccessCheck {
    type Output = DiskAccess;
}

/// Whether the terminal app `app` (a name on macOS, a program elsewhere)
/// can be found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalCheck {
    pub app: String,
}

impl Operation for TerminalCheck {
    type Output = bool;
}

/// Quit the app; the shell does it.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct QuitApp;

impl Operation for QuitApp {
    type Output = ();
}

/// The Full Disk Access banner.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(C)]
pub enum DiskAccessPrompt {
    #[default]
    Hidden,
    /// Access is missing: offer to open System Settings.
    Ask,
    /// System Settings was opened: the grant is being watched for.
    Waiting,
    /// Granted, but only a new process gets it: offer to restart.
    Restart,
}

/// What Settings ▸ General shows about Full Disk Access, the terminal and
/// the app.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct AccessView {
    pub prompt: DiskAccessPrompt,
    /// Full Disk Access matters here (a built app on macOS).
    pub disk_access_applies: bool,
    pub disk_access_granted: bool,
    /// The terminal app in the settings was found.
    pub terminal_found: bool,
    /// "Version 0.0.1 (abc1234)", once the app has started.
    pub version: String,
    pub website: String,
}

#[derive(Default)]
pub(super) struct Access {
    prompt: DiskAccessPrompt,
    found: DiskAccess,
    terminal_found: bool,
    /// Known once the app started: scripts, which never start it, keep the
    /// same view model from one commit to the next.
    started: bool,
}

impl Default for DiskAccess {
    fn default() -> Self {
        Self {
            applies: false,
            granted: true,
        }
    }
}

impl Access {
    pub(super) fn view(&self) -> AccessView {
        AccessView {
            prompt: self.prompt,
            disk_access_applies: self.found.applies,
            disk_access_granted: self.found.granted,
            terminal_found: self.terminal_found,
            version: if self.started {
                format!(
                    "Version {} ({})",
                    env!("CARGO_PKG_VERSION"),
                    env!("DIRFLEET_COMMIT")
                )
            } else {
                String::new()
            },
            website: env!("CARGO_PKG_HOMEPAGE").to_owned(),
        }
    }
}

impl DirFleet {
    /// Checks Full Disk Access and the terminal once the settings are known.
    pub(super) fn check_access(model: &mut Model) -> Command<Effect, Event> {
        model.access.started = true;
        Command::request_from_shell(DiskAccessCheck)
            .then_send(|found| Event::DiskAccessChecked { found })
            .and(Self::check_terminal(model))
    }

    pub(super) fn check_terminal(model: &Model) -> Command<Effect, Event> {
        Command::request_from_shell(TerminalCheck {
            app: model.settings.terminal.clone(),
        })
        .then_send(|found| Event::TerminalChecked { found })
    }

    /// The events of the Full Disk Access banner and the terminal check.
    pub(super) fn access_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        let access = &mut model.access;
        match event {
            Event::DiskAccessChecked { found } => {
                access.found = found;
                access.prompt = match access.prompt {
                    _ if !found.applies => DiskAccessPrompt::Hidden,
                    DiskAccessPrompt::Waiting if found.granted => DiskAccessPrompt::Restart,
                    DiskAccessPrompt::Waiting => {
                        return Command::request_from_shell(Delay {
                            millis: RECHECK_MILLIS,
                        })
                        .then_send(|()| Event::RecheckDiskAccess);
                    }
                    DiskAccessPrompt::Hidden
                        if !found.granted && model.settings.ask_full_disk_access =>
                    {
                        DiskAccessPrompt::Ask
                    }
                    prompt => prompt,
                };
                render()
            }
            Event::RecheckDiskAccess => {
                if access.prompt != DiskAccessPrompt::Waiting {
                    return Command::done();
                }
                Command::request_from_shell(DiskAccessCheck)
                    .then_send(|found| Event::DiskAccessChecked { found })
            }
            Event::OpenDiskAccessSettings => {
                access.prompt = DiskAccessPrompt::Waiting;
                render()
                    .and(Self::system(SystemOp::OpenPrivacySettings))
                    .and(
                        Command::request_from_shell(DiskAccessCheck)
                            .then_send(|found| Event::DiskAccessChecked { found }),
                    )
            }
            Event::DismissDiskAccess { forever } => {
                access.prompt = DiskAccessPrompt::Hidden;
                if forever {
                    model.settings.ask_full_disk_access = false;
                }
                render()
            }
            Event::RestartApp => Command::request_from_shell(SystemOp::Relaunch)
                .then_send(|done| Event::Relaunching { done }),
            Event::Relaunching { done } => match done.0 {
                Ok(()) => Command::notify_shell(QuitApp).into(),
                Err(error) => Self::notify(model, error),
            },
            Event::TerminalChecked { found } => {
                access.terminal_found = found;
                render()
            }
            _ => Command::done(),
        }
    }
}
