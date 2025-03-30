use std::path::PathBuf;

/// Enum representing actions that can be performed within the application.
#[derive(Debug, Clone)]
pub enum ActionToPerform {
    /// Change the current working path to the specified path.
    ChangePath(PathBuf),

    /// Open a new tab with the specified path as the root.
    NewTab(PathBuf),

    /// Open the specified path in a terminal window.
    OpenInTerminal(PathBuf),

    /// Request a refresh of the currently displayed files.
    RequestFilesRefresh,
}
