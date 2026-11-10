//! Handing things to the system, as egui's entry actions do: open files in
//! their apps (or one the user picks), show them in the file manager, open a
//! terminal, Get Info, copy paths as text. Folders are browsed into instead,
//! several of them in new tabs.
use crux_core::Command;
use crux_core::capability::Operation;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::{DirFleet, Effect, Event, Model, PickFolderOutput, Tab, ToastKind, tab_id};
use crate::listing::EntryType;

/// Something for the operating system to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SystemOp {
    /// Open each in its default app.
    Open(Vec<String>),
    /// Open these with the app at `app`.
    OpenWith { app: String, paths: Vec<String> },
    /// Show these selected in the system's file manager.
    Reveal(Vec<String>),
    /// Open this folder in the system's file manager; a package (which
    /// opening would launch) is shown selected instead.
    OpenFolder(String),
    /// Open the terminal `app` in `folder`.
    Terminal { folder: String, app: String },
    /// Show the system's information window for these.
    GetInfo(Vec<String>),
    /// Put this text on the clipboard.
    CopyText(String),
    /// Open the system's privacy settings (Full Disk Access on macOS).
    OpenPrivacySettings,
    /// Start the app again once this process has quit, so a new Full Disk
    /// Access grant takes effect.
    Relaunch,
}

/// How a [`SystemOp`] went. Opaque to facet: it never leaves the core.
#[derive(Clone, Debug, PartialEq, Eq, Facet)]
#[facet(opaque)]
pub struct SystemDone(pub Result<(), String>);

impl Operation for SystemOp {
    type Output = SystemDone;
}

/// Change the open history: count opens, forget them, or clear it all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenHistoryOp {
    Record(Vec<String>),
    Forget(Vec<String>),
    /// Items renamed or moved, `(from, to)`: their records and those of
    /// everything below them follow.
    Rename(Vec<(String, String)>),
    Clear,
}

impl Operation for OpenHistoryOp {
    type Output = ();
}

/// Ask the user for an application, with the platform's own picker.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PickApp {
    pub title: String,
}

impl Operation for PickApp {
    type Output = PickFolderOutput;
}

impl Tab {
    /// The selected rows' paths, or the cursor's when nothing is selected,
    /// each with whether it is a folder to browse.
    fn chosen(&self) -> Vec<(String, bool)> {
        let rows: Vec<usize> = if self.selection.is_empty() {
            self.selection.cursor().into_iter().collect()
        } else {
            self.selection.rows().collect()
        };
        rows.into_iter()
            .filter_map(|row| {
                let entry = *self.order.get(row)? as usize;
                let folder = self.listing.meta(entry).entry_type == EntryType::Directory;
                let path = self.row_path(row)?.to_string_lossy().into_owned();
                Some((path, folder))
            })
            .collect()
    }

    /// The chosen paths, or the tab's folder when none is chosen.
    fn chosen_or_folder(&self) -> Vec<String> {
        let chosen: Vec<String> = self.chosen().into_iter().map(|(path, _)| path).collect();
        if chosen.is_empty() {
            self.path.clone().into_iter().collect()
        } else {
            chosen
        }
    }
}

impl DirFleet {
    pub(super) fn system(op: SystemOp) -> Command<Effect, Event> {
        Command::request_from_shell(op).then_send(|done| Event::SystemDone { done })
    }

    /// Opens the row: a folder is browsed into, anything else (packages
    /// too) opens in its app.
    pub(super) fn open_row(model: &Model, tab: u32, row: u32) -> Command<Effect, Event> {
        let Some(state) = tab_id(tab).and_then(|id| model.tabs.get(&id)) else {
            return Command::done();
        };
        let Some(entry) = state.order.get(row as usize).map(|&entry| entry as usize) else {
            return Command::done();
        };
        let Some(path) = state.row_path(row as usize) else {
            return Command::done();
        };
        let path = path.to_string_lossy().into_owned();
        if state.listing.meta(entry).entry_type == EntryType::Directory {
            Command::event(Event::Navigate { tab, path })
        } else {
            Self::system(SystemOp::Open(vec![path.clone()])).and(Self::record_opens(
                model,
                tab,
                vec![path],
            ))
        }
    }

    /// Opens the selection: one folder is browsed into, several open in new
    /// tabs; files open in their apps.
    pub(super) fn open_selection(model: &Model, tab: u32) -> Command<Effect, Event> {
        let Some(id) = tab_id(tab) else {
            return Command::done();
        };
        let Some(state) = model.tabs.get(&id) else {
            return Command::done();
        };
        let (folders, files): (Vec<_>, Vec<_>) = state.chosen().into_iter().partition(|c| c.1);
        let files: Vec<String> = files.into_iter().map(|(path, _)| path).collect();
        let open_files = if files.is_empty() {
            Command::done()
        } else {
            Self::system(SystemOp::Open(files.clone())).and(Self::record_opens(model, tab, files))
        };
        let browse = match folders.as_slice() {
            [] => Command::done(),
            [(path, _)] => Command::event(Event::Navigate {
                tab,
                path: path.clone(),
            }),
            several => {
                let Some(window) = model.window_of(id).map(|index| model.windows[index].id) else {
                    return open_files;
                };
                Command::all(several.iter().map(|(path, _)| {
                    Command::event(Event::NewTab {
                        window: window.number(),
                        path: Some(path.clone()),
                    })
                }))
            }
        };
        open_files.and(browse)
    }

    pub(super) fn history(op: OpenHistoryOp) -> Command<Effect, Event> {
        Command::notify_shell(op).into()
    }

    /// Counts the opens, when the settings say to, and reads the tab again
    /// so its Opened and Opens columns follow.
    fn record_opens(model: &Model, tab: u32, paths: Vec<String>) -> Command<Effect, Event> {
        if !model.settings.track_open_history {
            return Command::done();
        }
        Self::history(OpenHistoryOp::Record(paths)).and(Command::event(Event::Refresh { tab }))
    }

    /// The events that hand things to the system.
    pub(super) fn system_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        let state = |tab: u32| tab_id(tab).and_then(|id| model.tabs.get(&id));
        match event {
            Event::RevealSelection { tab } => state(tab)
                .map(Tab::chosen_or_folder)
                .filter(|paths| !paths.is_empty())
                .map_or_else(Command::done, |paths| Self::system(SystemOp::Reveal(paths))),
            Event::GetInfo { tab } => state(tab)
                .map(Tab::chosen_or_folder)
                .filter(|paths| !paths.is_empty())
                .map_or_else(Command::done, |paths| {
                    Self::system(SystemOp::GetInfo(paths))
                }),
            Event::CopyPaths { tab } => state(tab)
                .map(Tab::chosen_or_folder)
                .filter(|paths| !paths.is_empty())
                .map_or_else(Command::done, |paths| {
                    Self::system(SystemOp::CopyText(paths.join("\n")))
                }),
            Event::OpenInTerminal { tab } => state(tab)
                .and_then(|state| state.path.clone())
                .map_or_else(Command::done, |folder| {
                    let app = model.settings.terminal.clone();
                    Self::system(SystemOp::Terminal { folder, app })
                }),
            Event::ShowPackageContents { tab } => {
                let package = state(tab).and_then(|state| {
                    let row = state.selection.cursor()?;
                    let entry = *state.order.get(row)? as usize;
                    (state.listing.meta(entry).entry_type == EntryType::Package)
                        .then(|| state.row_path(row))
                        .flatten()
                });
                package.map_or_else(Command::done, |path| {
                    Command::event(Event::Navigate {
                        tab,
                        path: path.to_string_lossy().into_owned(),
                    })
                })
            }
            Event::OpenWith { tab } => {
                let paths: Vec<String> = state(tab)
                    .map(|state| state.chosen().into_iter().map(|(path, _)| path).collect())
                    .unwrap_or_default();
                if paths.is_empty() {
                    return Command::done();
                }
                Command::request_from_shell(PickApp {
                    title: "Open with".into(),
                })
                .then_send(move |output| Event::AppPicked { paths, output })
            }
            Event::AppPicked {
                paths,
                output: PickFolderOutput::Picked(app),
            } => Self::system(SystemOp::OpenWith { app, paths }),
            Event::RevealPath { path } => Self::system(SystemOp::Reveal(vec![path])),
            Event::OpenInFileManager { path } => Self::system(SystemOp::OpenFolder(path)),
            Event::CopyPath { path } => Self::system(SystemOp::CopyText(path)),
            Event::OpenPrivacySettings => Self::system(SystemOp::OpenPrivacySettings),
            Event::ForgetOpened { tab } => {
                let paths: Vec<String> = state(tab)
                    .map(|state| state.chosen().into_iter().map(|(path, _)| path).collect())
                    .unwrap_or_default();
                if paths.is_empty() {
                    return Command::done();
                }
                Self::history(OpenHistoryOp::Forget(paths))
                    .and(Command::event(Event::Refresh { tab }))
            }
            Event::ClearOpenHistory => {
                // Tabs restored but not shown yet are read when shown.
                let refreshes: Vec<_> = model
                    .tabs
                    .iter()
                    .filter(|(_, tab)| tab.generation != 0)
                    .map(|(id, _)| Command::event(Event::Refresh { tab: id.number() }))
                    .collect();
                Self::history(OpenHistoryOp::Clear).and(Command::all(refreshes))
            }
            Event::SystemDone {
                done: SystemDone(Err(error)),
            } => Self::toast(model, ToastKind::Error, error),
            _ => Command::done(),
        }
    }
}
