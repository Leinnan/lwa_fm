use std::{borrow::Cow, fmt::Display, path::PathBuf, str::FromStr};

use crate::app::{dock::CurrentPath, DataSource};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalWindow {
    // NewDirectory,
    Settings,
    Commands,
}
impl Display for ModalWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Settings => write!(f, "Settings"),
            Self::Commands => write!(f, "Commands"),
        }
    }
}

/// Enum representing actions that can be performed within the application.
#[derive(Debug, Clone)]
pub enum ActionToPerform {
    /// Change the current working path to the specified path.
    ChangePaths(CurrentPath),

    /// Open a new tab with the specified path as the root.
    NewTab(PathBuf),

    /// Open the specified path in a terminal window.
    OpenInTerminal(PathBuf),

    /// Request a refresh of the currently displayed files.
    RequestFilesRefresh,
    /// Request a sort of the currently displayed files.
    FilesSort,
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
    /// `SearchInFavorites`
    SearchInFavorites(bool),
    /// Search filter changed
    FilterChanged,
    /// Search filter changed
    ViewSettingsChanged(DataSource),
    /// Open the specified path in the system's default application.
    SystemOpen(Cow<'static, str>),
}

impl From<&ActionToPerform> for Cow<'static, str> {
    fn from(val: &ActionToPerform) -> Self {
        match val {
            ActionToPerform::AddToFavorites(_) => Cow::Borrowed("Add to favorites"),
            ActionToPerform::RemoveFromFavorites(_) => Cow::Borrowed("Remove from favorites"),
            ActionToPerform::ChangePaths(_) => Cow::Borrowed("Open"),
            ActionToPerform::NewTab(_) => Cow::Borrowed("Open in new tab"),
            ActionToPerform::OpenInTerminal(_) => Cow::Borrowed("Open in terminal"),
            ActionToPerform::RequestFilesRefresh => todo!(),
            ActionToPerform::ToggleModalWindow(modal_window) => {
                Cow::Owned(format!("Toggle {modal_window}"))
            }
            ActionToPerform::CloseActiveModalWindow => Cow::Borrowed("Close popup"),
            ActionToPerform::ToggleTopEdit => Cow::Borrowed("Toggle path edit"),
            ActionToPerform::SearchInFavorites(search) => {
                if *search {
                    Cow::Borrowed("Search in favorites")
                } else {
                    Cow::Borrowed("Regular search")
                }
            }
            ActionToPerform::FilterChanged => Cow::Borrowed("Filter changed"),
            ActionToPerform::FilesSort => Cow::Borrowed("Sort files"),
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
                Some(Self::ChangePaths(path.into()))
            }
        })
    }
}
