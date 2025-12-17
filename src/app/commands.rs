use crate::app::{DataSource, dock::CurrentPath};
use crossbeam::queue::SegQueue;
use std::{borrow::Cow, fmt::Display, path::PathBuf, str::FromStr};

pub static COMMANDS_QUEUE: std::sync::LazyLock<SegQueue<ActionToPerform>> =
    std::sync::LazyLock::new(SegQueue::new);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ModalWindow {
    // NewDirectory,
    Settings,
    Commands,
    Rename,
}
impl Display for ModalWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Settings => write!(f, "Settings"),
            Self::Commands => write!(f, "Commands"),
            Self::Rename => write!(f, "Rename"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum TabAction {
    /// Change the current working path to the specified path.
    ChangePaths(CurrentPath),
    /// Request a refresh of the currently displayed files.
    RequestFilesRefresh,
    /// Request a sort of the currently displayed files.
    FilesSort,
    /// `SearchInFavorites`
    SearchInFavorites(bool),
    /// Search filter changed
    FilterChanged,
}

impl TabAction {
    pub fn schedule_tab(self, tab_id: u32) {
        COMMANDS_QUEUE.push(ActionToPerform::TabAction(
            TabTarget::TabWithId(tab_id),
            self,
        ));
    }
    pub fn schedule_active_tab(self) {
        COMMANDS_QUEUE.push(ActionToPerform::TabAction(TabTarget::ActiveTab, self));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TabTarget {
    #[default]
    ActiveTab,
    AllTabs,
    TabWithId(u32),
}
impl Display for TabTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ActiveTab => write!(f, "ActiveTab"),
            Self::AllTabs => write!(f, "AllTabs"),
            Self::TabWithId(id) => write!(f, "TabWithId({id})"),
        }
    }
}

/// Enum representing actions that can be performed within the application.
#[derive(Debug, Clone)]
pub enum ActionToPerform {
    TabAction(TabTarget, TabAction),
    /// Open a new tab with the specified path as the root.
    NewTab(PathBuf),

    /// Open the specified path in a terminal window.
    OpenInTerminal(PathBuf),

    /// Toggles the modal window.
    ToggleModalWindow(ModalWindow),
    /// Closes the active modal window.
    CloseActiveModalWindow,

    /// Shows the Path Edit
    ToggleTopEdit,

    /// Add to favorites
    AddToFavorites(Cow<'static, str>),
    /// Remove from favorites
    RemoveFromFavorites(Cow<'static, str>),
    /// Search filter changed
    ViewSettingsChanged(DataSource),
    /// Open the specified path in the system's default application.
    SystemOpen(Cow<'static, str>),
}

impl ActionToPerform {
    pub fn schedule(self) {
        COMMANDS_QUEUE.push(self);
    }
}

impl From<&ActionToPerform> for Cow<'static, str> {
    fn from(val: &ActionToPerform) -> Self {
        match val {
            ActionToPerform::TabAction(_, action) => match action {
                TabAction::ChangePaths(_) => Cow::Borrowed("Open"),
                TabAction::RequestFilesRefresh => Cow::Borrowed("Refresh"),
                TabAction::FilesSort => Cow::Borrowed("Sort"),
                TabAction::SearchInFavorites(favorites) => {
                    if *favorites {
                        Cow::Borrowed("Search in favorites")
                    } else {
                        Cow::Borrowed("Search")
                    }
                }
                TabAction::FilterChanged => Cow::Borrowed("Filter changed"),
            },
            ActionToPerform::AddToFavorites(_) => Cow::Borrowed("Add to favorites"),
            ActionToPerform::RemoveFromFavorites(_) => Cow::Borrowed("Remove from favorites"),
            ActionToPerform::NewTab(_) => Cow::Borrowed("Open in new tab"),
            ActionToPerform::OpenInTerminal(_) => Cow::Borrowed("Open in terminal"),
            ActionToPerform::ToggleModalWindow(modal_window) => {
                Cow::Owned(format!("Toggle {modal_window}"))
            }
            ActionToPerform::CloseActiveModalWindow => Cow::Borrowed("Close popup"),
            ActionToPerform::ToggleTopEdit => Cow::Borrowed("Toggle path edit"),
            ActionToPerform::ViewSettingsChanged(data_source) => {
                if data_source == &DataSource::Local {
                    Cow::Borrowed("Local view settings changed")
                } else {
                    Cow::Borrowed("Global view settings changed")
                }
            }
            ActionToPerform::SystemOpen(path) => Cow::Owned(format!("Open {path}")),
        }
    }
}

impl ActionToPerform {
    /// Creates a new `ActionToPerform` from a string path and a boolean indicating whether to open in a new tab.
    pub fn path_from_str(path: impl AsRef<str>, new_tab: bool) -> Option<Self> {
        PathBuf::from_str(path.as_ref()).map_or(None, |path| {
            if new_tab {
                Some(Self::NewTab(path))
            } else {
                Some(Self::TabAction(
                    TabTarget::ActiveTab,
                    TabAction::ChangePaths(path.into()),
                ))
            }
        })
    }
}
