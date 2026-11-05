use crate::app::dock::{TabId, WindowId};
use crate::app::{DataSource, dock::CurrentPath};
use crate::data::files::Listing;
use crossbeam::queue::SegQueue;
use std::cell::Cell;
use std::{borrow::Cow, fmt::Display, path::PathBuf, str::FromStr};

pub static COMMANDS_QUEUE: std::sync::LazyLock<CommandQueue> =
    std::sync::LazyLock::new(CommandQueue::default);

thread_local! {
    /// The window being drawn or handling a command on this thread.
    static CURRENT_WINDOW: Cell<Option<WindowId>> = const { Cell::new(None) };
}

/// Runs `f` with commands scheduled meanwhile marked as coming from `window`.
pub fn in_window<R>(window: WindowId, f: impl FnOnce() -> R) -> R {
    let previous = CURRENT_WINDOW.replace(Some(window));
    let result = f();
    CURRENT_WINDOW.set(previous);
    result
}

/// An action and the window it came from: `None` for background threads,
/// whose actions name their tab.
#[derive(Debug)]
pub struct Queued {
    pub window: Option<WindowId>,
    pub action: ActionToPerform,
}

/// Actions waiting for the UI thread. Each remembers the window that
/// scheduled it, so "the active tab" means that window's active tab.
#[derive(Debug, Default)]
pub struct CommandQueue(SegQueue<Queued>);

impl CommandQueue {
    pub fn push(&self, action: ActionToPerform) {
        self.0.push(Queued {
            window: CURRENT_WINDOW.get(),
            action,
        });
    }
    #[cfg(test)]
    pub fn pop(&self) -> Option<ActionToPerform> {
        self.0.pop().map(|queued| queued.action)
    }
    pub fn pop_queued(&self) -> Option<Queued> {
        self.0.pop()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ModalWindow {
    // NewDirectory,
    Settings,
    Commands,
    Rename,
    Shortcuts,
    ConfirmDelete,
    /// Items were dropped onto a folder: move or copy them?
    ConfirmDrop,
    Conflict,
    ChooseFolder,
    EditSavedSearch,
    BatchRename,
    ConfirmEject,
    /// A plugin action's form.
    PluginForm,
    /// A plugin action's options and results.
    PluginResults,
    /// Choose which overview frame is a video's thumbnail.
    SelectThumbnail,
}
impl Display for ModalWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Settings => write!(f, "Settings"),
            Self::Commands => write!(f, "Commands"),
            Self::Rename => write!(f, "Rename"),
            Self::Shortcuts => write!(f, "Keyboard shortcuts"),
            Self::ConfirmDelete => write!(f, "Delete permanently"),
            Self::ConfirmDrop => write!(f, "Move or copy"),
            Self::Conflict => write!(f, "Replace or skip"),
            Self::ConfirmEject => write!(f, "Eject drive"),
            Self::PluginForm => write!(f, "Plugin"),
            Self::PluginResults => write!(f, "Plugin results"),
            Self::ChooseFolder => write!(f, "Choose folder"),
            Self::EditSavedSearch => write!(f, "Saved search"),
            Self::BatchRename => write!(f, "Rename items"),
            Self::SelectThumbnail => write!(f, "Select thumbnail"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum TabAction {
    /// Change the current working path to the specified path.
    ChangePaths(CurrentPath),
    /// Go back or forward to a history entry: its folder and its search.
    Restore(crate::app::history::HistoryEntry),
    /// Request a refresh of the currently displayed files.
    RequestFilesRefresh,
    /// Force a full refresh: invalidates sled cache and re-reads from filesystem.
    ForceRefresh,
    /// Request a sort of the currently displayed files.
    FilesSort,
    /// `SearchInFavorites`
    SearchInFavorites(bool),
    /// Search filter changed
    FilterChanged,
    /// Background thread has finished loading directory entries.
    FilesLoaded {
        listing: std::sync::Arc<Listing>,
        order: Vec<u32>,
        visible: Vec<u32>,
        generation: u64,
        /// Directory symlinks the walk followed (extra watch roots).
        linked_dirs: Vec<std::path::PathBuf>,
    },
    /// Partial results of a deep walk that is still running.
    FilesPartial {
        listing: std::sync::Arc<Listing>,
        order: Vec<u32>,
        visible: Vec<u32>,
        generation: u64,
    },
    /// Background thread progress update.
    FilesProgress { progress: String, generation: u64 },
}

impl TabAction {
    pub fn schedule_tab(self, tab_id: TabId) {
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
    TabWithId(TabId),
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
    ToggleSidebar,
    ToggleInspector,
    TabAction(TabTarget, TabAction),
    /// Open a new tab with the specified path as the root.
    NewTab(PathBuf),
    /// Open a new window showing the specified folder.
    NewWindow(PathBuf),
    /// Show the Recycle Bin, in the active tab or a new one.
    OpenTrash {
        new_tab: bool,
    },
    /// Ask before emptying the Recycle Bin.
    ConfirmEmptyTrash,
    /// Put the selected Recycle Bin rows back where they came from.
    RestoreTrashSelection,

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
    /// Run an app-wide command, the same one its keyboard shortcut runs.
    RunCommand(crate::app::keymap::Command),
    /// Run a file operation on the background worker.
    FileOp(crate::fileops::FileOp),
    /// Progress or completion reported by the file operation worker.
    FileOpEvent(crate::fileops::FileOpEvent),
    /// Copy (or move) items into a folder, asking about name conflicts.
    Transfer {
        srcs: Vec<PathBuf>,
        dst: PathBuf,
        moving: bool,
    },
    /// Items dragged onto a folder: like `Transfer`, but asks first when
    /// the setting says so, since a drag can end over the wrong folder.
    DropTransfer {
        srcs: Vec<PathBuf>,
        dst: PathBuf,
        moving: bool,
    },
    /// Items dragged onto the Recycle Bin.
    DropToTrash(Vec<PathBuf>),
    /// Open the rename dialog for an item.
    BeginRename(PathBuf),
    /// Paste the clipboard's files into a folder.
    PasteInto(PathBuf),
    /// Ask for a destination folder, then copy or move the items there.
    ChooseTransferTarget {
        srcs: Vec<PathBuf>,
        moving: bool,
    },
    /// Open a saved search's folders with its query.
    RunSavedSearch {
        id: u64,
        target: crate::app::saved_searches::RunTarget,
    },
    EditSavedSearch(u64),
    DuplicateSavedSearch(u64),
    DeleteSavedSearch(u64),
    MoveSavedSearch {
        id: u64,
        to: usize,
    },
    /// Eject (or disconnect) the drive mounted at this path. `force` skips
    /// asking about file operations still running on it.
    EjectDrive {
        mount: PathBuf,
        force: bool,
    },
    /// The background eject of a drive finished.
    DriveEjected {
        mount: PathBuf,
        name: String,
        result: Result<(), String>,
    },
    /// Start a plugin action (`<plugin id>.<action id>`): its form, or the
    /// action itself. Empty targets and folder mean the active tab's.
    RunPluginAction {
        action: String,
        context: crate::plugins::CallContext,
    },
    /// A message from a plugin's thread.
    Plugin(Box<crate::plugins::PluginMsg>),
    /// Open the dialog choosing a video's thumbnail from its overview frames.
    SelectThumbnail(Box<crate::data::files::DirEntry>),
    /// Go back to the default thumbnail of a video.
    ResetThumbnail(PathBuf),
    /// Show Explorer's own context menu for these items, all in one folder.
    #[cfg(windows)]
    ShowSystemContextMenu(Vec<PathBuf>),
}

impl ActionToPerform {
    pub fn schedule(self) {
        COMMANDS_QUEUE.push(self);
    }
}

impl From<&ActionToPerform> for Cow<'static, str> {
    fn from(val: &ActionToPerform) -> Self {
        match val {
            ActionToPerform::ToggleSidebar => Cow::Borrowed("Toggle sidebar"),
            ActionToPerform::ToggleInspector => Cow::Borrowed("Toggle inspector"),
            ActionToPerform::TabAction(_, action) => match action {
                TabAction::ChangePaths(_) => Cow::Borrowed("Open"),
                TabAction::Restore(_) => Cow::Borrowed("Go to"),
                TabAction::RequestFilesRefresh => Cow::Borrowed("Refresh"),
                TabAction::ForceRefresh => Cow::Borrowed("Force refresh"),
                TabAction::FilesSort => Cow::Borrowed("Sort"),
                TabAction::SearchInFavorites(favorites) => {
                    if *favorites {
                        Cow::Borrowed("Search in favorites")
                    } else {
                        Cow::Borrowed("Search")
                    }
                }
                TabAction::FilterChanged => Cow::Borrowed("Filter changed"),
                TabAction::FilesLoaded { .. } => Cow::Borrowed("Files loaded"),
                TabAction::FilesPartial { .. } => Cow::Borrowed("Files partially loaded"),
                TabAction::FilesProgress { .. } => Cow::Borrowed("Files loading progress"),
            },
            ActionToPerform::AddToFavorites(_) => Cow::Borrowed("Add to favorites"),
            ActionToPerform::RemoveFromFavorites(_) => Cow::Borrowed("Remove from favorites"),
            ActionToPerform::NewTab(_) => Cow::Borrowed("Open in new tab"),
            ActionToPerform::NewWindow(_) => Cow::Borrowed("Open in new window"),
            ActionToPerform::OpenTrash { .. } | ActionToPerform::DropToTrash(_) => {
                Cow::Borrowed(crate::fileops::trash_bin::LABEL)
            }
            ActionToPerform::ConfirmEmptyTrash => Cow::Borrowed("Empty the Recycle Bin"),
            ActionToPerform::RestoreTrashSelection => Cow::Borrowed("Restore"),
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
            ActionToPerform::RunCommand(command) => Cow::Borrowed(command.label()),
            ActionToPerform::FileOp(op) => Cow::Owned(op.describe()),
            ActionToPerform::FileOpEvent(_) => Cow::Borrowed("File operation progress"),
            ActionToPerform::Transfer { moving, .. }
            | ActionToPerform::DropTransfer { moving, .. } => {
                Cow::Borrowed(if *moving { "Move" } else { "Copy" })
            }
            ActionToPerform::BeginRename(_) => Cow::Borrowed("Rename"),
            ActionToPerform::PasteInto(_) => Cow::Borrowed("Paste into folder"),
            ActionToPerform::RunSavedSearch { .. } => Cow::Borrowed("Run saved search"),
            ActionToPerform::EditSavedSearch(_) => Cow::Borrowed("Edit saved search"),
            ActionToPerform::DuplicateSavedSearch(_) => Cow::Borrowed("Duplicate saved search"),
            ActionToPerform::DeleteSavedSearch(_) => Cow::Borrowed("Delete saved search"),
            ActionToPerform::MoveSavedSearch { .. } => Cow::Borrowed("Move saved search"),
            ActionToPerform::ChooseTransferTarget { moving, .. } => {
                Cow::Borrowed(if *moving { "Move to…" } else { "Copy to…" })
            }
            ActionToPerform::EjectDrive { .. } => Cow::Borrowed("Eject"),
            ActionToPerform::DriveEjected { .. } => Cow::Borrowed("Drive ejected"),
            ActionToPerform::RunPluginAction { action, .. } => {
                let registry = crate::plugins::registry();
                Cow::Owned(
                    registry
                        .actions
                        .iter()
                        .find(|entry| entry.full_id == *action)
                        .map_or_else(|| action.clone(), |entry| entry.def.title.clone()),
                )
            }
            ActionToPerform::Plugin(_) => Cow::Borrowed("Plugin message"),
            ActionToPerform::SelectThumbnail(_) => Cow::Borrowed("Select thumbnail…"),
            ActionToPerform::ResetThumbnail(_) => Cow::Borrowed("Reset thumbnail"),
            #[cfg(windows)]
            ActionToPerform::ShowSystemContextMenu(_) => Cow::Borrowed("Windows context menu"),
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
