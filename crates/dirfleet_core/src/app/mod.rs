//! The Crux app: events in, model updates and effects out, and the view
//! model every shell draws.
//!
//! So far: windows, each holding tabs that list a folder, open subfolders, go
//! back and refresh when the folder changes, kept between launches. Events
//! name their window or tab by number, so no shell needs a notion of "the
//! current one".

mod access;
mod archives;
mod batch;
mod checksums;
mod files;
mod finder;
pub mod format;
mod inspector;
pub mod layout;
mod navigation;
pub mod ops;
mod palette;
mod pathbar;
mod picker;
mod plugin_forms;
mod plugin_jobs;
mod plugin_results;
mod plugin_views;
mod plugins;
mod remote;
mod saved;
mod selection;
mod settings;
mod sidebar;
mod system;
mod thumbnails;
mod toasts;
mod trash;
mod undo;
mod workspace;

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use crux_core::command::AbortHandle;
use crux_core::macros::effect;
use crux_core::render::{RenderOperation, render};
use crux_core::{App, Command};
use facet::Facet;
use serde::{Deserialize, Serialize};

use crate::columns::{Column, ListColumns};
pub use crate::fileops::ArchiveFormat;
use crate::history::{History, Place};
use crate::ids::{IdAllocator, TabId, WindowId};
use crate::listing::{EntryType, Listing, is_shown};
use crate::location::Location;
use crate::plugins::CallContext;
use crate::search::Search;
use crate::{DisplayType, Sort};
pub use access::{
    AccessView, DiskAccess, DiskAccessCheck, DiskAccessPrompt, QuitApp, TerminalCheck,
};
pub use archives::{
    CompressView, FormatChoice, LocationView, Materialize, Materialized, PasswordView, Secret,
    SetArchivePassword, Unreadable, is_archive_name,
};
pub use batch::{BatchFields, BatchItem, BatchMode, BatchRenameView};
pub use checksums::{
    Algorithm, ChecksumRow, ChecksumVerdict, ChecksumsView, HashFile, HashUpdate, ReadSidecar,
    StopHash,
};
use egui_mev_core::selection::Selection;
pub use files::{
    ActivityState, ActivityView, CancelFileOp, ClipboardChanges, ClipboardDone, ClipboardOp,
    ConflictChoice, ConflictView, Conflicts, FileOpUpdate, FindConflicts, OperationState,
    OperationView, PauseFileOp, RenameView, RunFileOp, TransferTarget,
};
pub use finder::{
    ChipView, Clock, Delay, FilterChoice, FilterGroup, SearchHelpRow, SearchScope, SearchView,
    SearchWalk, StopWalk, TextRange, WIDER_DEPTH, WalkUpdate,
};
pub use inspector::{FolderSize, InspectorView, ItemInfo, KindCount, SelectionSummary, SizeInfo};
pub use layout::{FlatLayout, FlatPane, FlatSplit, NodeRef, Side};
use layout::{Node, Pane};
pub use ops::{
    FolderView, ListDir, ListDirOutput, ListView, LoadState, LoadedState, OpenWindow, OrderListing,
    Ordered, PersistedState, PersistedWindow, PickFolder, PickFolderOutput, QuickLook,
    STATE_VERSION, SaveState, Unwatch, Watch, WatchEvent, WindowFrame,
};
pub use palette::{CommandId, CommandView, PaletteItem, PaletteTarget, PaletteView};
pub use pathbar::{
    Anchor, ChildDirs, ChildDirsOutput, ChildrenView, CrumbView, GoToMatch, GoToView, crumbs,
};
pub use picker::{
    ChosenFrame, PickerFrame, PickerFrameView, PickerGapView, PickerTiming, ThumbnailChoice,
    ThumbnailChoiceOp, ThumbnailChoices, ThumbnailPickerView, VideoLength, VideoTiming,
};
pub use plugin_forms::{
    FormFieldKind, FormFieldView, FormOptionView, FormPathMode, FormPathView, PluginFormView,
};
pub use plugin_jobs::PluginJobView;
pub use plugin_results::{
    PluginResultsView, ResultsColumnView, ResultsMenuItemView, ResultsRowView,
};
pub use plugin_views::{BarSegmentView, PluginBarView, PluginSectionRow, PluginSectionView};
pub use plugins::{
    ActivatedPlugins, BrokenPluginView, CheckActivation, LoadPlugins, PluginActionView,
    PluginControl, PluginFolderChanged, PluginMessages, PluginNotice, PluginShortcutSetting,
    PluginShortcutView, PluginView, PluginsFound, PluginsView, WatchPlugins,
};
pub use remote::{
    Connect, ConnectFields, ConnectOutput, ConnectView, Connection, Disconnect, ForgetSecret,
    HostKeyView, SchemeChoice,
};
pub use saved::{SavedBadge, SavedSearchView};
pub use selection::RowRange;
pub use settings::{Appearance, IconSize, IconSizeChoice, QueueMode, Settings};
pub use sidebar::{
    Bookmark, DriveList, DriveStatusView, Eject, EjectConfirmView, EjectDone, SidebarIcon,
    SidebarItem, SidebarSection, SidebarSectionView, UserFolders, WatchDrives,
};
pub use system::{OpenHistoryOp, PickApp, SystemDone, SystemOp};
pub use thumbnails::{MediaNotice, MediaNotices, PreviewState, Thumbnail};
pub use toasts::{ToastKind, ToastView};
pub use trash::{ListTrash, TrashDone, TrashInfo, TrashOp};
pub use workspace::{AdoptedTab, TabTarget};

/// Rows sent beyond the visible ones, so small scrolls need no new view.
pub const OVERSCAN: u32 = 20;
/// Rows assumed visible before a shell reports its viewport.
pub const DEFAULT_VISIBLE_ROWS: u32 = 60;
/// The rows a tab's view sends: the visible ones and as many again before
/// and after (at least [`OVERSCAN`]), moving half a screen at a time, so
/// scrolling needs a new view only every half screen, and rows reach the
/// shell before they scroll into sight.
fn row_window(first_row: u32, visible_rows: u32, total_rows: u32) -> (u32, u32) {
    let visible = if visible_rows == 0 {
        DEFAULT_VISIBLE_ROWS
    } else {
        visible_rows
    };
    let step = (visible / 2).max(OVERSCAN / 2);
    let overscan = visible.max(OVERSCAN);
    // Both ends follow the first row rounded down to a step, so the window
    // holds still while the first row stays within that step.
    let base = first_row / step * step;
    let start = base.saturating_sub(overscan).min(total_rows);
    let end = base
        .saturating_add(step)
        .saturating_add(visible)
        .saturating_add(overscan)
        .min(total_rows);
    (start, end)
}

/// The warning when a folder can't be watched for changes, as egui's.
const LIVE_UPDATES_UNAVAILABLE: &str = "Live updates unavailable";

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum Event {
    /// The shell is up: restore the saved windows, or open one at `home`.
    /// `open`, a folder the app was started with, replaces the saved windows.
    Started {
        open: Option<String>,
        home: String,
    },
    /// The app is quitting: windows closing from now on stay saved.
    Quitting,
    /// Open a window with one tab, showing `path` when given. The shell is
    /// told the new window's id with [`OpenWindow`].
    NewWindow {
        path: Option<String>,
    },
    /// The user closed this window.
    WindowClosed {
        window: u32,
    },
    /// A tab at the end of the window's focused pane, showing `path`, or the
    /// focused tab's folder without one.
    NewTab {
        window: u32,
        path: Option<String>,
    },
    /// Close a tab, keeping its folder for Reopen. The first window keeps
    /// its last tab; another window closes with its last tab.
    CloseTab {
        tab: u32,
    },
    /// Show a tab in its pane and focus it.
    ActivateTab {
        tab: u32,
    },
    /// A copy of the tab right after it.
    DuplicateTab {
        tab: u32,
    },
    /// Close every other tab of the tab's window.
    CloseOtherTabs {
        tab: u32,
    },
    /// Open the most recently closed tab again, in this window.
    ReopenClosedTab {
        window: u32,
    },
    /// Show the next (or previous) tab of the focused pane, wrapping around.
    CycleTab {
        window: u32,
        forward: bool,
    },
    /// Show tab `number` (from 1) of the focused pane; 9 is the last.
    GoToTab {
        window: u32,
        number: u8,
    },
    /// Move the tab into a new pane on `side` of its pane.
    SplitTab {
        tab: u32,
        side: Side,
    },
    /// A copy of the tab in a new pane on `side` of its pane: how a window
    /// with one tab (every Mac window) gets a second pane.
    SplitNew {
        tab: u32,
        side: Side,
    },
    /// Open the most recently closed tab again in a window of its own (a
    /// native tab, on the Mac).
    ReopenClosedAsWindow,
    /// The split at `split` (in the window's flat layout) was dragged.
    ResizeSplit {
        window: u32,
        split: u32,
        permille: u16,
    },
    MoveTab {
        tab: u32,
        to: TabTarget,
    },
    /// The shell rearranged the window's tabs itself (egui after a drag).
    SetLayout {
        window: u32,
        layout: FlatLayout,
    },
    /// A window of tabs the shell made itself, with the ids it chose (egui
    /// makes its tabs before showing them), focused on the tab at `active`.
    AdoptWindow {
        tabs: Vec<AdoptedTab>,
        active: u32,
    },
    /// A tab the shell made itself, at the end of the window's focused pane.
    AdoptTab {
        window: u32,
        tab: AdoptedTab,
    },
    /// egui's rule for closing its main (first) window while others are open:
    /// the first window takes over the second's tabs, and the second closes.
    CloseFirstWindow,
    /// The window joined (or left) a native tab group (the Mac's).
    WindowGrouped {
        window: u32,
        group: Option<u32>,
    },
    /// The window came to the front: folders shown by when items were
    /// opened are read again, since other apps may have opened some.
    WindowFocused {
        window: u32,
    },
    /// The window moved or was resized; kept to put it back next time.
    WindowFramed {
        window: u32,
        frame: WindowFrame,
    },
    /// Move a sidebar section to where `to` is.
    MoveSidebarSection {
        section: SidebarSection,
        to: SidebarSection,
    },
    /// Run the saved search `id` in a new window (on the Mac, a new native
    /// tab), starting from its folder or `tab`'s.
    RunSavedSearchInNewWindow {
        tab: u32,
        id: u64,
    },
    /// Show this folder in the tab, remembering the current one for Back.
    Navigate {
        tab: u32,
        path: String,
    },
    /// Open the row at this position: folders are navigated into.
    OpenRow {
        tab: u32,
        row: u32,
    },
    Back {
        tab: u32,
    },
    Forward {
        tab: u32,
    },
    /// Show the folder holding the tab's folder.
    Up {
        tab: u32,
    },
    Refresh {
        tab: u32,
    },
    /// Ask the user for a folder and show it in the tab.
    OpenFolder {
        tab: u32,
    },
    /// Show the Recycle Bin / Trash in the tab.
    OpenTrash {
        tab: u32,
    },
    EmptyTrash,
    /// Delete the Trash tab's selected items for good.
    PurgeSelection {
        tab: u32,
    },
    /// Put the Trash tab's selected items back (not on macOS).
    RestoreSelection {
        tab: u32,
    },
    /// The search text as typed so far; opens the tab's search. Results
    /// follow each key; history records it after a pause.
    SetSearchText {
        tab: u32,
        text: String,
    },
    /// Enter in the search field: the typed text becomes chips.
    CommitSearchText {
        tab: u32,
    },
    /// Add a filter chip written as it is typed (`kind:image`, `size:>1mb`),
    /// or remove it when the search has it: the presets' toggle.
    ToggleSearchFilter {
        tab: u32,
        token: String,
    },
    RemoveSearchChip {
        tab: u32,
        index: u32,
    },
    SetSearchCase {
        tab: u32,
        on: bool,
    },
    /// 1 searches the folder; more searches subfolders that deep too.
    SetSearchDepth {
        tab: u32,
        depth: u32,
    },
    /// Names match when any word does, rather than all.
    SetSearchAny {
        tab: u32,
        any: bool,
    },
    /// Close the search: the folder shows all of itself again.
    CloseSearch {
        tab: u32,
    },
    /// Escape in the search field: clears the text, then the filters, then
    /// closes the search.
    SearchEscape {
        tab: u32,
    },
    /// Stop reading subfolders, keeping what was found.
    StopSearch {
        tab: u32,
    },
    /// Search the favorites instead of the tab's folder, or go back to it.
    SetSearchFavorites {
        tab: u32,
        on: bool,
    },
    AddFavorite {
        path: String,
    },
    RemoveFavorite {
        path: String,
    },
    /// Move the favorite at `from` to `to`.
    MoveFavorite {
        from: u32,
        to: u32,
    },
    SetSectionCollapsed {
        section: SidebarSection,
        collapsed: bool,
    },
    /// Eject (or disconnect) the drive mounted at `mount`; when file
    /// operations still run on it, the shell asks first (`eject_confirm`).
    EjectDrive {
        mount: String,
    },
    /// Eject anyway, cancelling the operations on the drive.
    ConfirmEject,
    CancelEject,
    /// A right-click on `row`: outside the selection, it selects that row
    /// first, so the menu acts on it.
    ContextClick {
        tab: u32,
        row: u32,
    },
    DismissToast {
        id: u64,
    },
    /// Show the selection (or the folder) in the system's file manager.
    RevealSelection {
        tab: u32,
    },
    /// Open the selection with an app the user picks.
    OpenWith {
        tab: u32,
    },
    /// Open a terminal in the tab's folder.
    OpenInTerminal {
        tab: u32,
    },
    /// The system's information window for the selection (or the folder).
    GetInfo {
        tab: u32,
    },
    /// Copy the selection's paths (or the folder's) as text.
    CopyPaths {
        tab: u32,
    },
    /// Show `path` in the system's file manager (a crumb or sidebar row).
    RevealPath {
        path: String,
    },
    /// Open the folder `path` in the system's file manager.
    OpenInFileManager {
        path: String,
    },
    /// Copy `path` as text.
    CopyPath {
        path: String,
    },
    /// Open the system's privacy settings, where the app is given access to
    /// folders (Full Disk Access on macOS).
    OpenPrivacySettings,
    /// The Full Disk Access banner's Open System Settings: open them and
    /// watch for the grant.
    OpenDiskAccessSettings,
    /// The banner's Not now, or (`forever`) Don't ask again.
    DismissDiskAccess {
        forever: bool,
    },
    /// The banner's Restart: start again so the grant takes effect.
    RestartApp,
    /// Browse into the selected package (an app bundle) as a folder.
    ShowPackageContents {
        tab: u32,
    },
    /// Make a folder in the tab's folder and rename it once it shows.
    NewFolder {
        tab: u32,
    },
    NewTextFile {
        tab: u32,
    },
    /// Rename the row under the cursor in place.
    StartRename {
        tab: u32,
    },
    /// Finish the rename with `name`.
    Rename {
        tab: u32,
        name: String,
    },
    CancelRename {
        tab: u32,
    },
    /// Copy each selected item beside itself.
    Duplicate {
        tab: u32,
    },
    TrashSelection {
        tab: u32,
    },
    /// Delete the selection for good; shells confirm first.
    DeleteSelection {
        tab: u32,
    },
    CopyTo {
        tab: u32,
        folder: String,
    },
    MoveTo {
        tab: u32,
        folder: String,
    },
    /// How clashing names go for the copy or move waiting on it.
    ResolveConflict {
        choice: ConflictChoice,
    },
    Undo,
    Redo,
    CancelOperation {
        id: u64,
    },
    CopySelection {
        tab: u32,
    },
    CutSelection {
        tab: u32,
    },
    /// Copy (or move, after a cut) the clipboard's files into the tab's folder.
    Paste {
        tab: u32,
    },
    /// Paste into `folder`, from its menu.
    PasteInto {
        tab: u32,
        folder: String,
    },
    /// Open batch rename for the selected items (several of them).
    StartBatchRename {
        tab: u32,
    },
    /// The batch rename dialog's fields changed.
    SetBatchRename {
        tab: u32,
        fields: BatchFields,
    },
    /// Rename the items as previewed, as one undoable step.
    ApplyBatchRename {
        tab: u32,
    },
    CancelBatchRename {
        tab: u32,
    },
    /// Items dragged onto `target` (a folder or an app; the tab's folder when
    /// `None`). `copy` and `force_move` come from the shell's keys.
    DropItems {
        tab: u32,
        paths: Vec<String>,
        target: Option<String>,
        copy: bool,
        force_move: bool,
    },
    /// Items dragged onto the Trash.
    DropOnTrash {
        tab: u32,
        paths: Vec<String>,
    },
    /// Read the subfolders of `path`, for a crumb's menu.
    ListChildren {
        tab: u32,
        path: String,
    },
    /// Open Go to Folder, starting from the tab's folder.
    OpenGoTo {
        tab: u32,
    },
    GoToTyped {
        tab: u32,
        text: String,
    },
    /// Open `path` (a chosen match), or what the text names when `None`.
    GoToSubmit {
        tab: u32,
        path: Option<String>,
    },
    CloseGoTo {
        tab: u32,
    },
    /// Save the tab's search under `name` (made unique).
    SaveSearch {
        tab: u32,
        name: String,
    },
    /// Save the tab's search over the saved search it came from.
    UpdateSavedSearch {
        tab: u32,
    },
    /// Put the tab's search back to the saved search it came from.
    RevertSearch {
        tab: u32,
    },
    RunSavedSearch {
        tab: u32,
        id: u64,
    },
    DeleteSavedSearch {
        id: u64,
    },
    DuplicateSavedSearch {
        id: u64,
    },
    MoveSavedSearch {
        from: u32,
        to: u32,
    },
    /// The saved search editor's result: the query as typed and its options.
    EditSavedSearch {
        id: u64,
        name: String,
        text: String,
        case_sensitive: bool,
        depth: u32,
        any: bool,
        favorites: bool,
    },
    /// Replace the settings with the shell's edited copy.
    ChangeSettings {
        settings: Settings,
    },
    RestoreDefaultSettings,
    /// The view of folders without their own.
    SetFolderDefaults {
        view: FolderView,
    },
    /// Open the command palette, or change what is typed in it.
    SetPaletteText {
        text: String,
    },
    ClosePalette,
    /// Forget that the selected files were opened.
    ForgetOpened {
        tab: u32,
    },
    ClearOpenHistory,
    /// Show or hide the inspector (kept between launches).
    SetInspector {
        shown: bool,
    },
    /// Turn a plugin on or off (Settings ▸ Plugins).
    SetPluginEnabled {
        id: String,
        enabled: bool,
    },
    /// Set the shortcut of a plugin's action; its manifest's (or "" for
    /// none) otherwise.
    SetPluginShortcut {
        plugin: String,
        action: String,
        text: String,
    },
    /// Allow a plugin the permissions it asks for.
    ApprovePlugin {
        id: String,
    },
    /// Find the plugins again.
    ReloadPlugins,
    /// Run the plugin action `action` (`<plugin id>.<action id>`) on the
    /// tab's selection.
    RunPluginAction {
        tab: u32,
        action: String,
    },
    /// A value typed or picked in the plugin form's field `id`, as text
    /// ("true" or "false" for a checkbox).
    SetPluginFormField {
        id: String,
        text: String,
    },
    SubmitPluginForm,
    CancelPluginForm,
    /// Stop a plugin job, or drop it while it waits.
    CancelPluginJob {
        id: u64,
    },
    /// The shown results dialog: run (or restart) with its options.
    RunPluginResults,
    StopPluginResults,
    /// Hide it; its job keeps running.
    HidePluginResults,
    /// Bring back the results dialog `key` (`<plugin id>.<action id>`).
    ShowPluginResults {
        key: String,
    },
    SetPluginResultsField {
        id: String,
        text: String,
    },
    /// Sort by the column at `column`; again reverses.
    SortPluginResults {
        column: u32,
    },
    /// Hide rows smaller than `amount` megabytes or gigabytes.
    FilterPluginResults {
        amount: String,
        gigabytes: bool,
    },
    /// Open a row's folder in a new tab.
    OpenPluginResultsRow {
        path: String,
    },
    /// Measure a row's folder again.
    RefreshPluginResultsRow {
        path: String,
    },
    /// Run one of a row's own actions on its folder.
    PluginResultsRowAction {
        path: String,
        action: String,
    },
    /// Choose the thumbnail of the video at `row` from its frames.
    SelectThumbnail {
        tab: u32,
        row: u32,
    },
    /// The video at `row` goes back to its default thumbnail.
    ResetThumbnail {
        tab: u32,
        row: u32,
    },
    /// Every video goes back to its default thumbnail (Settings).
    ResetAllThumbnails,
    /// Select the picker's frame at `at_ms`.
    SelectPickerFrame {
        at_ms: u64,
    },
    /// Select the finished frame `steps` before or after the selected one
    /// (arrow keys: one, or a row of the sheet).
    StepPickerFrame {
        forward: bool,
        steps: u32,
    },
    /// Add frames to the picker between `from` and `to`.
    FillPickerGap {
        from: u64,
        to: u64,
    },
    /// Make the picker's selected frame the thumbnail, and close it.
    UsePickerFrame,
    /// Go back to the default thumbnail from the picker, and close it.
    ResetPickerFrame,
    ClosePicker,
    /// Try again to make the inspector's preview of `path`.
    RetryPreview {
        path: String,
    },
    /// The pointer entered (`inside`) or left the grid tile `row`; a video
    /// or GIF there plays once it rests.
    HoverRow {
        tab: u32,
        row: u32,
        inside: bool,
    },
    /// Add up the size of the folder at `path`, for the inspector.
    CalculateSize {
        path: String,
    },
    /// Rows `first_row..first_row + rows` of the tab are on screen.
    ViewportChanged {
        tab: u32,
        first_row: u32,
        rows: u32,
    },
    /// Sort the tab's folder by `sort`; the current sort again reverses it.
    SortBy {
        tab: u32,
        sort: Sort,
    },
    SetReverse {
        tab: u32,
        reverse: bool,
    },
    SetDisplay {
        tab: u32,
        display: DisplayType,
    },
    SetShowHidden {
        tab: u32,
        show: bool,
    },
    /// Show or hide an optional list column, in every window.
    ToggleColumn {
        column: Column,
    },
    /// The user dragged a column to `width` points.
    SetColumnWidth {
        column: Column,
        width: u32,
    },
    /// Back to the default columns and widths.
    ResetColumns,
    /// A click on a row: Cmd toggles it, Shift selects from the anchor.
    Click {
        tab: u32,
        row: u32,
        command: bool,
        shift: bool,
    },
    /// The keyboard moved the cursor: Shift extends, Cmd moves only the cursor.
    MoveCursor {
        tab: u32,
        row: u32,
        command: bool,
        shift: bool,
    },
    /// The shell's own list changed its selection to these rows.
    SetSelection {
        tab: u32,
        rows: Vec<u32>,
    },
    SelectAll {
        tab: u32,
    },
    InvertSelection {
        tab: u32,
    },
    ClearSelection {
        tab: u32,
    },
    /// Open the selection: a single folder is navigated into.
    OpenSelection {
        tab: u32,
    },
    /// Preview the selection, or the row under the cursor, with Quick Look.
    QuickLook {
        tab: u32,
    },
    /// Open the item Quick Look shows (Return in its panel, Enter in egui's).
    OpenPreviewed {
        tab: u32,
        path: String,
    },
    /// Letters typed in the list: jump to the next name starting with what
    /// was typed in the last second.
    TypeSelect {
        tab: u32,
        text: String,
    },
    /// Hold a copy, move or deletion where it is. One that has not started
    /// loses its turn; one that runs frees its drive for those waiting.
    PauseOperation {
        id: u64,
    },
    /// Let a paused operation go on, or wait its turn again.
    ResumeOperation {
        id: u64,
    },
    PauseAllOperations,
    ResumeAllOperations,
    /// Start a waiting operation without waiting for its turn.
    StartOperationNow {
        id: u64,
    },
    /// Take the selected archives' contents out beside them; inside an
    /// archive, the selected entries (or the folder shown) into the folder
    /// the archive is in.
    ExtractHere {
        tab: u32,
    },
    /// The same, into `folder`.
    ExtractTo {
        tab: u32,
        folder: String,
    },
    /// Open the dialog that connects to a server: a new one, or the saved
    /// connection `edit`. Once connected, `tab` goes there.
    OpenConnect {
        tab: u32,
        edit: Option<String>,
    },
    /// What the connect dialog's fields hold now.
    SetConnect {
        fields: ConnectFields,
    },
    /// Save the dialog's connection and connect, with the password typed
    /// (empty for the one in the keychain); `remember` keeps it there.
    SubmitConnect {
        secret: Secret,
        remember: bool,
    },
    CancelConnect,
    /// The answer to "is this the right server?" about a key it showed.
    AcceptHostKey {
        accept: bool,
    },
    /// Forget a saved connection and its password.
    RemoveConnection {
        id: String,
    },
    /// Close a server's connection; it stays saved.
    DisconnectServer {
        id: String,
    },
    /// Open the Compress dialog for the tab's selection.
    StartCompress {
        tab: u32,
    },
    /// The dialog's name (without the format's ending) and format.
    SetCompress {
        tab: u32,
        name: String,
        format: ArchiveFormat,
    },
    /// Make the archive beside what goes into it.
    ApplyCompress {
        tab: u32,
    },
    CancelCompress {
        tab: u32,
    },
    /// The password an archive asked for; what waited for it goes on.
    SubmitArchivePassword {
        password: Secret,
    },
    /// No password: what waited for it is given up.
    CancelArchivePassword,
    /// Work out one of a file's checksums, for the inspector.
    ComputeChecksum {
        path: String,
        algorithm: Algorithm,
    },
    /// Stop reading the file for its checksums.
    CancelChecksum {
        path: String,
    },
    /// Compare the file with a checksum the user pasted: the text may hold
    /// more than the checksum, and its length says which kind it is. Empty
    /// text ends the comparison.
    VerifyChecksum {
        path: String,
        expected: String,
    },
    /// Compare the file with the checksum the checksum file `sidecar`, in
    /// the same folder, holds for it.
    VerifySidecar {
        path: String,
        sidecar: String,
    },
    /// Copy one of the file's checksums as text.
    CopyChecksum {
        path: String,
        algorithm: Algorithm,
    },

    #[serde(skip)]
    #[facet(skip)]
    ConnectAnswered {
        id: String,
        output: ConnectOutput,
    },
    #[serde(skip)]
    #[facet(skip)]
    Materialized {
        location: String,
        output: Materialized,
    },
    #[serde(skip)]
    #[facet(skip)]
    ChecksumUpdated {
        id: u64,
        path: String,
        update: HashUpdate,
    },
    #[serde(skip)]
    #[facet(skip)]
    SidecarRead {
        path: String,
        sidecar: String,
        expected: Option<String>,
    },
    #[serde(skip)]
    #[facet(skip)]
    Listed {
        tab: u32,
        generation: u64,
        output: ListDirOutput,
    },
    #[serde(skip)]
    #[facet(skip)]
    Watched {
        tab: u32,
        generation: u64,
        event: WatchEvent,
    },
    #[serde(skip)]
    #[facet(skip)]
    FolderPicked {
        tab: u32,
        output: PickFolderOutput,
    },
    #[serde(skip)]
    #[facet(skip)]
    Ordered {
        tab: u32,
        output: Ordered,
    },
    #[serde(skip)]
    #[facet(skip)]
    StateLoaded {
        open: Option<String>,
        home: String,
        loaded: LoadedState,
    },
    #[serde(skip)]
    #[facet(skip)]
    TrashChanged {
        done: TrashDone,
    },
    #[serde(skip)]
    #[facet(skip)]
    TypingPaused {
        tab: u32,
        typed: u64,
    },
    #[serde(skip)]
    #[facet(skip)]
    SizeCalculated {
        path: String,
        total: Option<u64>,
    },
    #[serde(skip)]
    #[facet(skip)]
    ThumbnailMade {
        path: String,
        edge: u32,
        animated: bool,
        revision: u128,
        size: u64,
        /// The frame chosen for a video's poster when it was asked for.
        frame: Option<u64>,
        file: Option<String>,
    },
    #[serde(skip)]
    #[facet(skip)]
    DiskAccessChecked {
        found: DiskAccess,
    },
    #[serde(skip)]
    #[facet(skip)]
    RecheckDiskAccess,
    #[serde(skip)]
    #[facet(skip)]
    Relaunching {
        done: SystemDone,
    },
    #[serde(skip)]
    #[facet(skip)]
    TerminalChecked {
        found: bool,
    },
    #[serde(skip)]
    #[facet(skip)]
    ClockTicked {
        now: u64,
    },
    #[serde(skip)]
    #[facet(skip)]
    HoverRested {
        tab: u32,
        count: u64,
    },
    #[serde(skip)]
    #[facet(skip)]
    MediaNoticed {
        notice: MediaNotice,
    },
    #[serde(skip)]
    #[facet(skip)]
    ChildrenListed {
        tab: u32,
        path: String,
        output: ChildDirsOutput,
    },
    #[serde(skip)]
    #[facet(skip)]
    GoToListed {
        tab: u32,
        parent: String,
        output: ChildDirsOutput,
    },
    #[serde(skip)]
    #[facet(skip)]
    GoToChecked {
        tab: u32,
        target: String,
        output: ChildDirsOutput,
    },
    #[serde(skip)]
    #[facet(skip)]
    FileOpUpdated {
        id: u64,
        update: FileOpUpdate,
    },
    #[serde(skip)]
    #[facet(skip)]
    ConflictsFound {
        conflicts: Conflicts,
    },
    #[serde(skip)]
    #[facet(skip)]
    ClipboardDone {
        tab: u32,
        done: ClipboardDone,
    },
    /// The clipboard's change count: right after a cut, or to check
    /// whether another app replaced the cut items.
    #[serde(skip)]
    #[facet(skip)]
    ClipboardCounted {
        count: u64,
        cut: bool,
    },
    #[serde(skip)]
    #[facet(skip)]
    PastedInto {
        tab: u32,
        folder: String,
        done: ClipboardDone,
    },
    #[serde(skip)]
    #[facet(skip)]
    AppPicked {
        paths: Vec<String>,
        output: PickFolderOutput,
    },
    #[serde(skip)]
    #[facet(skip)]
    SystemDone {
        done: SystemDone,
    },
    #[serde(skip)]
    #[facet(skip)]
    UserFoldersFound {
        folders: Vec<Bookmark>,
    },
    #[serde(skip)]
    #[facet(skip)]
    DrivesChanged {
        drives: DriveList,
    },
    #[serde(skip)]
    #[facet(skip)]
    Ejected {
        mount: String,
        done: EjectDone,
    },
    #[serde(skip)]
    #[facet(skip)]
    SearchPaused {
        tab: u32,
        typed: u64,
    },
    #[serde(skip)]
    #[facet(skip)]
    Walked {
        tab: u32,
        generation: u64,
        update: WalkUpdate,
    },
    #[serde(skip)]
    #[facet(skip)]
    PickerTimed {
        session: u64,
        timing: VideoTiming,
    },
    #[serde(skip)]
    #[facet(skip)]
    PickerFrameMade {
        session: u64,
        at_ms: u64,
        file: Option<String>,
    },
    #[serde(skip)]
    #[facet(skip)]
    ThumbnailChoicesChanged {
        choices: ThumbnailChoices,
    },
    #[serde(skip)]
    #[facet(skip)]
    PluginsLoaded {
        found: PluginsFound,
    },
    #[serde(skip)]
    #[facet(skip)]
    PluginNoticed {
        notice: PluginNotice,
    },
    #[serde(skip)]
    #[facet(skip)]
    PluginsActivated {
        dir: String,
        plugins: ActivatedPlugins,
    },
    /// The selection rested: plugins hear of it, unless it moved again.
    #[serde(skip)]
    #[facet(skip)]
    SelectionSettled {
        count: u64,
    },
    #[serde(skip)]
    #[facet(skip)]
    PluginFolderChanged {
        folder: String,
    },
}

impl Event {
    /// Opening, closing, moving and arranging tabs (`workspace`).
    const fn is_workspace(&self) -> bool {
        matches!(
            self,
            Self::NewTab { .. }
                | Self::CloseTab { .. }
                | Self::ActivateTab { .. }
                | Self::DuplicateTab { .. }
                | Self::CloseOtherTabs { .. }
                | Self::ReopenClosedTab { .. }
                | Self::CycleTab { .. }
                | Self::GoToTab { .. }
                | Self::SplitTab { .. }
                | Self::SplitNew { .. }
                | Self::ReopenClosedAsWindow
                | Self::ResizeSplit { .. }
                | Self::MoveTab { .. }
                | Self::SetLayout { .. }
                | Self::WindowGrouped { .. }
                | Self::WindowFramed { .. }
                | Self::WindowFocused { .. }
                | Self::AdoptWindow { .. }
                | Self::AdoptTab { .. }
                | Self::CloseFirstWindow
        )
    }

    /// Changing a tab's search and its results arriving (`finder`).
    const fn is_search(&self) -> bool {
        matches!(
            self,
            Self::SetSearchText { .. }
                | Self::CommitSearchText { .. }
                | Self::ToggleSearchFilter { .. }
                | Self::RemoveSearchChip { .. }
                | Self::SetSearchCase { .. }
                | Self::SetSearchDepth { .. }
                | Self::SetSearchAny { .. }
                | Self::CloseSearch { .. }
                | Self::SearchEscape { .. }
                | Self::StopSearch { .. }
                | Self::SetSearchFavorites { .. }
                | Self::SearchPaused { .. }
                | Self::Walked { .. }
        )
    }

    /// The app-wide panels: toasts, the palette, the inspector, settings.
    const fn is_panel(&self) -> bool {
        matches!(
            self,
            Self::DismissToast { .. }
                | Self::SetPaletteText { .. }
                | Self::ClosePalette
                | Self::SetInspector { .. }
                | Self::CalculateSize { .. }
                | Self::SizeCalculated { .. }
                | Self::ThumbnailMade { .. }
                | Self::OpenDiskAccessSettings
                | Self::DismissDiskAccess { .. }
                | Self::RestartApp
                | Self::DiskAccessChecked { .. }
                | Self::RecheckDiskAccess
                | Self::Relaunching { .. }
                | Self::TerminalChecked { .. }
                | Self::RetryPreview { .. }
                | Self::HoverRow { .. }
                | Self::HoverRested { .. }
                | Self::MediaNoticed { .. }
                | Self::ChangeSettings { .. }
                | Self::RestoreDefaultSettings
                | Self::SetFolderDefaults { .. }
        )
    }

    /// Plugins and Settings ▸ Plugins (`plugins`).
    const fn is_plugin(&self) -> bool {
        matches!(
            self,
            Self::SetPluginEnabled { .. }
                | Self::ApprovePlugin { .. }
                | Self::SetPluginShortcut { .. }
                | Self::ReloadPlugins
                | Self::RunPluginAction { .. }
                | Self::SetPluginFormField { .. }
                | Self::SubmitPluginForm
                | Self::CancelPluginForm
                | Self::CancelPluginJob { .. }
                | Self::RunPluginResults
                | Self::StopPluginResults
                | Self::HidePluginResults
                | Self::ShowPluginResults { .. }
                | Self::SetPluginResultsField { .. }
                | Self::SortPluginResults { .. }
                | Self::FilterPluginResults { .. }
                | Self::OpenPluginResultsRow { .. }
                | Self::RefreshPluginResultsRow { .. }
                | Self::PluginResultsRowAction { .. }
                | Self::PluginsLoaded { .. }
                | Self::PluginNoticed { .. }
                | Self::PluginsActivated { .. }
                | Self::SelectionSettled { .. }
                | Self::PluginFolderChanged { .. }
        )
    }

    /// The thumbnail picker and chosen thumbnails (`picker`).
    const fn is_picker(&self) -> bool {
        matches!(
            self,
            Self::SelectThumbnail { .. }
                | Self::ResetThumbnail { .. }
                | Self::ResetAllThumbnails
                | Self::SelectPickerFrame { .. }
                | Self::StepPickerFrame { .. }
                | Self::FillPickerGap { .. }
                | Self::UsePickerFrame
                | Self::ResetPickerFrame
                | Self::ClosePicker
                | Self::PickerTimed { .. }
                | Self::PickerFrameMade { .. }
                | Self::ThumbnailChoicesChanged { .. }
        )
    }

    /// Saving, editing and running saved searches (`saved`).
    const fn is_saved(&self) -> bool {
        matches!(
            self,
            Self::SaveSearch { .. }
                | Self::UpdateSavedSearch { .. }
                | Self::RevertSearch { .. }
                | Self::RunSavedSearch { .. }
                | Self::RunSavedSearchInNewWindow { .. }
                | Self::DeleteSavedSearch { .. }
                | Self::DuplicateSavedSearch { .. }
                | Self::MoveSavedSearch { .. }
                | Self::EditSavedSearch { .. }
        )
    }

    /// Crumbs' subfolders and Go to Folder (`pathbar`).
    const fn is_pathbar(&self) -> bool {
        matches!(
            self,
            Self::ListChildren { .. }
                | Self::OpenGoTo { .. }
                | Self::GoToTyped { .. }
                | Self::GoToSubmit { .. }
                | Self::CloseGoTo { .. }
                | Self::ChildrenListed { .. }
                | Self::GoToListed { .. }
                | Self::GoToChecked { .. }
        )
    }

    /// Checksums, archives and servers: see [`DirFleet::place_event`].
    const fn is_place(&self) -> bool {
        self.is_checksum() || self.is_archive() || self.is_remote()
    }

    /// Connecting to servers (`remote`).
    const fn is_remote(&self) -> bool {
        matches!(
            self,
            Self::OpenConnect { .. }
                | Self::SetConnect { .. }
                | Self::SubmitConnect { .. }
                | Self::CancelConnect
                | Self::AcceptHostKey { .. }
                | Self::RemoveConnection { .. }
                | Self::DisconnectServer { .. }
                | Self::ConnectAnswered { .. }
        )
    }

    /// Extracting, and archive passwords (`archives`).
    const fn is_archive(&self) -> bool {
        matches!(
            self,
            Self::ExtractHere { .. }
                | Self::ExtractTo { .. }
                | Self::SubmitArchivePassword { .. }
                | Self::CancelArchivePassword
                | Self::StartCompress { .. }
                | Self::SetCompress { .. }
                | Self::ApplyCompress { .. }
                | Self::CancelCompress { .. }
        )
    }

    /// The inspector's checksums (`checksums`).
    const fn is_checksum(&self) -> bool {
        matches!(
            self,
            Self::ComputeChecksum { .. }
                | Self::CancelChecksum { .. }
                | Self::VerifyChecksum { .. }
                | Self::VerifySidecar { .. }
                | Self::CopyChecksum { .. }
                | Self::ChecksumUpdated { .. }
                | Self::SidecarRead { .. }
        )
    }

    /// File operations, renames and the clipboard (`files`).
    const fn is_file(&self) -> bool {
        matches!(
            self,
            Self::NewFolder { .. }
                | Self::NewTextFile { .. }
                | Self::StartRename { .. }
                | Self::Rename { .. }
                | Self::CancelRename { .. }
                | Self::Duplicate { .. }
                | Self::TrashSelection { .. }
                | Self::DeleteSelection { .. }
                | Self::CopyTo { .. }
                | Self::MoveTo { .. }
                | Self::ResolveConflict { .. }
                | Self::Undo
                | Self::Redo
                | Self::CancelOperation { .. }
                | Self::PauseOperation { .. }
                | Self::ResumeOperation { .. }
                | Self::PauseAllOperations
                | Self::ResumeAllOperations
                | Self::StartOperationNow { .. }
                | Self::CopySelection { .. }
                | Self::CutSelection { .. }
                | Self::Paste { .. }
                | Self::PasteInto { .. }
                | Self::PastedInto { .. }
                | Self::StartBatchRename { .. }
                | Self::SetBatchRename { .. }
                | Self::ApplyBatchRename { .. }
                | Self::CancelBatchRename { .. }
                | Self::DropItems { .. }
                | Self::DropOnTrash { .. }
                | Self::FileOpUpdated { .. }
                | Self::ConflictsFound { .. }
                | Self::ClipboardDone { .. }
                | Self::ClipboardCounted { .. }
        )
    }

    /// Handing things to the system (`system`).
    const fn is_system(&self) -> bool {
        matches!(
            self,
            Self::RevealSelection { .. }
                | Self::OpenWith { .. }
                | Self::OpenInTerminal { .. }
                | Self::GetInfo { .. }
                | Self::CopyPaths { .. }
                | Self::ShowPackageContents { .. }
                | Self::RevealPath { .. }
                | Self::OpenInFileManager { .. }
                | Self::CopyPath { .. }
                | Self::OpenPrivacySettings
                | Self::ForgetOpened { .. }
                | Self::ClearOpenHistory
                | Self::AppPicked { .. }
                | Self::SystemDone { .. }
                | Self::Materialized { .. }
        )
    }

    /// Favorites, sections and drives (`sidebar`).
    const fn is_sidebar(&self) -> bool {
        matches!(
            self,
            Self::AddFavorite { .. }
                | Self::RemoveFavorite { .. }
                | Self::MoveFavorite { .. }
                | Self::SetSectionCollapsed { .. }
                | Self::MoveSidebarSection { .. }
                | Self::EjectDrive { .. }
                | Self::ConfirmEject
                | Self::CancelEject
                | Self::UserFoldersFound { .. }
                | Self::DrivesChanged { .. }
                | Self::Ejected { .. }
        )
    }

    /// Selecting rows and acting on the selection (`selection`).
    const fn is_selection(&self) -> bool {
        matches!(
            self,
            Self::Click { .. }
                | Self::ContextClick { .. }
                | Self::MoveCursor { .. }
                | Self::SetSelection { .. }
                | Self::SelectAll { .. }
                | Self::InvertSelection { .. }
                | Self::ClearSelection { .. }
                | Self::OpenSelection { .. }
                | Self::QuickLook { .. }
                | Self::OpenPreviewed { .. }
        )
    }
}

/// Every effect the core asks for. Shells only see their part of it
/// (`dirfleet_ffi::Effect`), so these operations need not cross the bridge.
#[effect]
#[derive(Debug)]
pub enum Effect {
    Render(RenderOperation),
    ListDir(ListDir),
    Watch(Watch),
    Unwatch(Unwatch),
    PickFolder(PickFolder),
    OpenWindow(OpenWindow),
    LoadState(LoadState),
    SaveState(SaveState),
    OrderListing(OrderListing),
    QuickLook(QuickLook),
    ListTrash(ListTrash),
    Trash(TrashOp),
    SearchWalk(SearchWalk),
    StopWalk(StopWalk),
    Delay(Delay),
    System(SystemOp),
    ChildDirs(ChildDirs),
    OpenHistory(OpenHistoryOp),
    FolderSize(FolderSize),
    HashFile(HashFile),
    Materialize(Materialize),
    SetArchivePassword(SetArchivePassword),
    Connect(Connect),
    Disconnect(Disconnect),
    ForgetSecret(ForgetSecret),
    StopHash(StopHash),
    ReadSidecar(ReadSidecar),
    RunFileOp(RunFileOp),
    CancelFileOp(CancelFileOp),
    PauseFileOp(PauseFileOp),
    FindConflicts(FindConflicts),
    Clipboard(ClipboardOp),
    ClipboardChanges(ClipboardChanges),
    PickApp(PickApp),
    UserFolders(UserFolders),
    WatchDrives(WatchDrives),
    Eject(Eject),
    Thumbnail(Thumbnail),
    ThumbnailChoice(ThumbnailChoiceOp),
    PickerTiming(PickerTiming),
    PickerFrame(PickerFrame),
    LoadPlugins(LoadPlugins),
    PluginControl(PluginControl),
    PluginMessages(PluginMessages),
    CheckActivation(CheckActivation),
    WatchPlugins(WatchPlugins),
    MediaNotices(MediaNotices),
    Clock(Clock),
    DiskAccessCheck(DiskAccessCheck),
    TerminalCheck(TerminalCheck),
    QuitApp(QuitApp),
}

#[derive(Default)]
pub struct Model {
    /// In the order they were opened.
    windows: Vec<Window>,
    tabs: BTreeMap<TabId, Tab>,
    window_ids: IdAllocator,
    tab_ids: IdAllocator,
    /// Bumped on every navigation in any tab; results of older ones are dropped.
    generation: u64,
    persistence: Persistence,
    /// Each folder's own view, by path.
    folder_views: BTreeMap<String, FolderView>,
    /// The view of folders without their own.
    default_view: FolderView,
    columns: ListColumns,
    /// Folders of the last closed tabs, newest last.
    closed: Vec<String>,
    sidebar: sidebar::Sidebar,
    files: files::Files,
    toasts: toasts::Toasts,
    inspector_shown: bool,
    settings: Settings,
    saved: crate::search::saved::SavedSearches,
    /// The palette's text, while it is open.
    palette: Option<String>,
    /// Folder sizes asked for in the inspector.
    sizes: inspector::Sizes,
    checksums: checksums::Checksums,
    archives: archives::Archives,
    remote: remote::Remote,
    thumbnails: thumbnails::Thumbnails,
    access: access::Access,
    plugins: plugins::Plugins,
    /// The thumbnail picker, while open, and how many were opened.
    picker: Option<picker::Picker>,
    picker_sessions: u64,
    /// Unix seconds, from the shell's clock; 0 until it first ticks, when
    /// dates show exactly rather than as "3 days ago".
    now: u64,
    /// The user's home folder, where tabs go when their drive leaves.
    home: String,
}

impl Model {
    fn view_for(&self, path: &str) -> FolderView {
        self.folder_views
            .get(path)
            .copied()
            .unwrap_or(self.default_view)
    }
}

/// Saving starts once the saved state is loaded, so an empty start never
/// overwrites it, and stops when the app quits, so closing its windows on
/// the way out doesn't forget them.
#[derive(Default)]
struct Persistence {
    loaded: bool,
    quitting: bool,
    /// The state last sent to be saved.
    saved: Option<PersistedState>,
}

struct Window {
    id: WindowId,
    layout: Node,
    /// The tab keyboard commands go to: always a pane's shown tab.
    focused: TabId,
    /// The Mac's native tab group, kept to restore the grouping.
    group: Option<u32>,
    /// Where the window is on screen, as the shell last said.
    frame: Option<WindowFrame>,
}

#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent flags of a tab's state"
)]
struct Tab {
    path: Option<String>,
    history: History,
    /// Shows the Trash rather than a folder (`path` is then `None`).
    trash: bool,
    /// Where to be once the folder is listed, when Back or Forward got here.
    returning_to: Option<Place>,
    /// A row the shell should scroll to, and a count that changes each time.
    scroll_to: Option<u32>,
    scroll_count: u32,
    listing: Arc<Listing>,
    /// `(times opened, last opened)` per entry of `listing`, when known.
    opened: Arc<Vec<(u32, u32)>>,
    /// The entries shown, as indices into `listing`, in display order.
    order: Vec<u32>,
    view: FolderView,
    /// Rows selected, as positions in `order`.
    selection: selection::Tracked,
    /// The generation of the tab's latest navigation.
    generation: u64,
    loading: bool,
    error: Option<String>,
    first_row: u32,
    visible_rows: u32,
    /// The folder's watch: its id and the handle stopping its stream.
    watch: Option<(u64, AbortHandle)>,
    /// The search, while the tab's search is open.
    search: Option<Search>,
    /// `search` while it has a query, shared with the listings made for it.
    filter: Option<Arc<Search>>,
    /// The search as last recorded in history.
    committed: Option<Search>,
    /// Counts keystrokes; a pause in typing records only the latest.
    typed: u64,
    /// What the listing was read from.
    read: Option<finder::ReadKey>,
    /// A running walk through subfolders: its id and the handle stopping it.
    walk: Option<(u64, AbortHandle)>,
    /// The folder as the last walk named it, for the Folder column.
    walk_root: Option<String>,
    /// The folders searched instead of the tab's own (the favorites).
    scope: Option<Vec<String>>,
    renaming: Option<files::Renaming>,
    /// Items an operation made, selected once the listing has them.
    select_after: Vec<std::path::PathBuf>,
    /// Rename the first of `select_after` too (New folder).
    rename_after: bool,
    /// What was typed to select by name, until a pause clears it.
    type_prefix: String,
    type_count: u64,
    /// A crumb's subfolders, once read: the crumb's path and their names.
    children: Option<(String, Vec<String>)>,
    goto: Option<pathbar::GoTo>,
    /// The grid tile the pointer is on.
    hover: thumbnails::Hover,
    /// The folder couldn't be read for lack of permission.
    denied: bool,
    /// The batch rename dialog, while it is open.
    batch: Option<batch::BatchRename>,
    /// The Compress dialog, while it is open.
    compress: Option<archives::Compress>,
    /// The folder is being read: a listing was asked for and hasn't come.
    reading: bool,
    /// The watcher started, or a change came, while the folder was being
    /// read: read it once more when that listing has come.
    refresh_when_listed: bool,
}

impl Tab {
    /// Drops what the tab showed, for a new place read with `generation`.
    fn reset_for(&mut self, generation: u64) {
        self.generation = generation;
        self.listing = Arc::default();
        self.order = Vec::new();
        *self.selection = Selection::default();
        self.loading = true;
        self.error = None;
        self.first_row = 0;
        self.renaming = None;
        self.reading = false;
        self.refresh_when_listed = false;
    }

    fn title(&self) -> String {
        if self.trash {
            crate::current_path::TRASH_LABEL.to_owned()
        } else {
            title_of(self.path.as_deref().unwrap_or_default())
        }
    }

    /// Listed, and sorted by when items were last opened or filtered by it.
    fn shows_by_opening(&self) -> bool {
        self.generation != 0
            && (self.view.sort == Sort::Accessed
                || self.search.as_ref().is_some_and(|search| {
                    search
                        .effective_chips()
                        .iter()
                        .any(|chip| matches!(chip, crate::search::Chip::Opened(_)))
                }))
    }

    /// The key the tab's view is kept under: its folder, or the Trash's.
    fn view_key(&self) -> Option<String> {
        if self.trash {
            Some(trash::TRASH_VIEW.to_owned())
        } else {
            self.path.clone()
        }
    }

    /// Stops watching and reading, for a tab that closes.
    fn stop_reading(&mut self) -> Command<Effect, Event> {
        self.stop_watching().and(self.stop_walk())
    }

    /// Stops watching the folder, telling the handler too (crux #622).
    fn stop_watching(&mut self) -> Command<Effect, Event> {
        match self.watch.take() {
            Some((id, handle)) => {
                handle.abort();
                Command::notify_shell(Unwatch { id }).into()
            }
            None => Command::done(),
        }
    }
}

/// Every window, each with only its visible tab's rows.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct ViewModel {
    pub windows: Vec<WindowView>,
    /// Every sort, in menu order, with its label.
    pub sorts: Vec<SortChoice>,
    /// The list view's columns, in order, Name first.
    pub columns: Vec<ColumnView>,
    /// The columns that can be shown or hidden, in menu order.
    pub optional_columns: Vec<ColumnChoice>,
    pub trash: TrashInfo,
    /// The preset filters a search offers.
    pub search_filters: Vec<FilterGroup>,
    /// Short messages, newest last, each until it expires or `DismissToast`.
    pub toasts: Vec<ToastView>,
    pub inspector_shown: bool,
    pub settings: Settings,
    /// Every command with its label and shortcut.
    pub commands: Vec<CommandView>,
    pub palette: Option<PaletteView>,
    pub saved_searches: Vec<SavedSearchView>,
    pub icon_sizes: Vec<IconSizeChoice>,
    /// The view of folders without their own.
    pub folder_defaults: FolderView,
    /// File operations still running.
    pub operations: Vec<OperationView>,
    /// Finished operations, newest first (the last 30).
    pub activity: Vec<ActivityView>,
    /// Ejecting a drive waits for this question's answer.
    pub eject_confirm: Option<EjectConfirmView>,
    /// The search syntax, for the search bar's help.
    pub search_help: Vec<SearchHelpRow>,
    /// Full Disk Access, the terminal check and About, for the banner and
    /// Settings.
    pub access: AccessView,
    /// Items cut to the clipboard and not pasted yet ("3 cut").
    pub cut_count: u32,
    /// The "Choose thumbnail" dialog, while open.
    pub thumbnail_picker: Option<ThumbnailPickerView>,
    /// Settings ▸ Plugins.
    pub plugins: PluginsView,
    /// A plugin action's form, while open.
    pub plugin_form: Option<PluginFormView>,
    /// Plugin jobs running or waiting, oldest first.
    pub plugin_jobs: Vec<PluginJobView>,
    /// Plugin actions bound to shortcuts, for the menu bar.
    pub plugin_shortcuts: Vec<PluginShortcutView>,
    /// A plugin's results dialog, while shown.
    pub plugin_results: Option<PluginResultsView>,
    /// What Undo and Redo would do ("rename"), when they can.
    pub undo: Option<String>,
    pub redo: Option<String>,
    /// A copy or move waiting for the user to say how clashing names go.
    pub conflict: Option<ConflictView>,
    /// An archive asks for its password.
    pub archive_password: Option<PasswordView>,
    /// The dialog that connects to a server, while it is open.
    pub connect: Option<ConnectView>,
    /// A server showed a key nobody accepted yet.
    pub host_key: Option<HostKeyView>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ColumnView {
    pub column: Column,
    pub title: String,
    /// Points; 0 for Name, which takes the space left.
    pub width: u32,
    pub right_aligned: bool,
    /// The sort clicking the header chooses, if any.
    pub sort: Option<Sort>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ColumnChoice {
    pub column: Column,
    pub title: String,
    pub shown: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SortChoice {
    pub sort: Sort,
    pub label: String,
}

impl ViewModel {
    pub fn window(&self, id: u32) -> Option<&WindowView> {
        self.windows.iter().find(|window| window.id == id)
    }
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct WindowView {
    pub id: u32,
    /// Every tab of the window, pane by pane.
    pub tabs: Vec<TabTitle>,
    /// The focused tab.
    pub active: TabViewModel,
    /// Each pane: its tabs and the one it shows.
    pub panes: Vec<PaneView>,
    /// How the panes are arranged; tabs by number.
    pub layout: FlatLayout,
    pub group: Option<u32>,
    /// Where the window was on screen, for the shell to put it back.
    pub frame: Option<WindowFrame>,
    /// The window's sidebar: rows mark what its focused tab shows.
    pub sidebar: Vec<SidebarSectionView>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PaneView {
    pub tabs: Vec<TabTitle>,
    pub shown: TabViewModel,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TabTitle {
    pub id: u32,
    pub title: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "what a shell draws, flag by flag"
)]
pub struct TabViewModel {
    pub id: u32,
    pub path: String,
    /// What sort of place `path` is, and what can be done there.
    pub location: LocationView,
    /// Extract has something to take: archives are selected, or the tab is
    /// inside one.
    pub can_extract: bool,
    /// The Compress dialog, while it is open.
    pub compress: Option<CompressView>,
    pub title: String,
    /// How the folder is shown.
    pub view: FolderView,
    /// Where Back, Forward and Up can go.
    pub nav: NavView,
    /// Scroll so this row shows, when `scroll_count` changes (Back and
    /// Forward return to where you were).
    pub scroll_to: Option<u32>,
    pub scroll_count: u32,
    pub loading: bool,
    pub error: Option<String>,
    /// The folder couldn't be read for lack of permission: the shell offers
    /// the Privacy settings.
    pub denied: bool,
    /// What a search through subfolders is doing ("Scanning 12 folders…").
    pub progress: String,
    pub total_rows: u32,
    /// When nothing is shown: how many entries are hidden items, so the
    /// shell can offer to show them. 0 otherwise.
    pub hidden_count: u32,
    /// The tab shows the Trash: its Modified column holds deletion times.
    pub in_trash: bool,
    /// The batch rename dialog, while it is open.
    pub batch_rename: Option<BatchRenameView>,
    /// The plugin actions the item menu offers for the selection.
    pub plugin_actions: Vec<PluginActionView>,
    /// The plugin actions on the folder, for the empty area's menu.
    pub folder_plugin_actions: Vec<PluginActionView>,
    /// Plugins' segments of the bar below the folder.
    pub plugin_bar: Vec<PluginBarView>,
    /// Plugins' inspector sections for the selection, while the inspector
    /// is shown.
    pub plugin_sections: Vec<PluginSectionView>,
    /// The drive the folder is on: free space, details, eject.
    pub drive_status: Option<DriveStatusView>,
    /// Where Move to / Copy to can send the selection.
    pub transfer_targets: Vec<TransferTarget>,
    /// The tab's search, while it is open.
    pub search: Option<SearchView>,
    /// Results come from several folders: rows fill `folder`, and lists
    /// show the Folder column.
    pub show_folder: bool,
    /// The row being renamed in place.
    pub renaming: Option<RenameView>,
    /// The path bar's crumbs, from the anchor on.
    pub crumbs: Vec<CrumbView>,
    /// The folders above the anchor, for its menu.
    pub crumbs_above: Vec<CrumbView>,
    pub children: Option<ChildrenView>,
    pub goto: Option<GoToView>,
    /// The selection described, while the inspector is shown.
    pub inspector: Option<InspectorView>,
    /// The saved search the tab's search came from.
    pub saved: Option<SavedBadge>,
    /// The selected rows, as ranges over every row.
    pub selection: Vec<RowRange>,
    pub selected_count: u32,
    /// The row keyboard navigation moves from.
    pub cursor: Option<u32>,
    /// The position of `rows[0]`.
    pub first_row: u32,
    pub rows: Vec<RowView>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[allow(clippy::struct_excessive_bools, reason = "one flag per button")]
pub struct NavView {
    pub back: bool,
    pub forward: bool,
    pub up: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RowView {
    pub index: u32,
    pub name: String,
    pub is_dir: bool,
    /// The text of each column after Name, in `ViewModel::columns` order.
    pub cells: Vec<String>,
    pub selected: bool,
    /// Where the name matches the search's words.
    pub highlights: Vec<TextRange>,
    /// Cut to the clipboard: drawn dimmed until pasted.
    pub cut: bool,
    /// A video's thumbnail can be chosen (and was).
    pub thumbnail_choice: ThumbnailChoice,
    /// The entry's folder, below the tab's, when `show_folder`.
    pub folder: String,
    /// The thumbnail's image file, or "" while there is none.
    pub thumbnail: String,
    /// The animated GIF to play instead, for the grid tile that plays, or "".
    pub animation: String,
    /// The exact dates the columns show as "3 days ago", one per line; ""
    /// until the time is known and dates show exactly anyway.
    pub tooltip: String,
    /// A file's size, for the line under large grid tiles ("" for folders;
    /// search results show `folder` there instead).
    pub detail: String,
}

fn tab_id(number: u32) -> Option<TabId> {
    NonZeroU32::new(number).map(TabId::new)
}

fn window_id(number: u32) -> Option<WindowId> {
    NonZeroU32::new(number).map(WindowId::new)
}

fn title_of(path: &str) -> String {
    Location::parse(path).title()
}

#[derive(Default)]
pub struct DirFleet;

impl DirFleet {
    /// Shows `path` in the tab `id` with `search`, dropping what it showed
    /// before.
    fn go(
        model: &mut Model,
        id: TabId,
        path: String,
        search: Option<Search>,
    ) -> Command<Effect, Event> {
        let view = model.view_for(&path);
        let Some(tab) = model.tabs.get_mut(&id) else {
            return Command::done();
        };
        tab.view = view;
        tab.scope = None;
        // Arriving at the current entry (Back, Forward) records nothing.
        tab.history.visit(navigation::entry(&path, search.clone()));
        tab.committed.clone_from(&search);
        tab.set_search(search);
        model.generation += 1;
        let generation = model.generation;
        tab.reset_for(generation);
        tab.path = Some(path.clone());
        tab.trash = false;

        let stop_old = tab.stop_watching();
        let number = id.number();
        let watch = Command::stream_from_shell(Watch {
            id: generation,
            path,
        })
        .then_send(move |event| Event::Watched {
            tab: number,
            generation,
            event,
        });
        tab.watch = Some((generation, watch.abort_handle()));

        let load = Self::load(tab, number);
        let opened = CallContext {
            dir: tab.path.clone().map(std::path::PathBuf::from),
            tab: Some(number),
            ..Default::default()
        };
        render()
            .and(stop_old)
            .and(load)
            .and(watch)
            .and(Self::emit_plugin_event(
                model,
                "folder_opened",
                &opened,
                &serde_json::Value::Null,
            ))
    }

    fn list(tab: u32, path: String, view: ListView, generation: u64) -> Command<Effect, Event> {
        Command::request_from_shell(ListDir { path, view }).then_send(move |output| Event::Listed {
            tab,
            generation,
            output,
        })
    }

    fn new_window(model: &mut Model, path: Option<String>) -> Command<Effect, Event> {
        Self::open_window(model, vec![path], 0, None, None)
    }

    /// Opens a window with a tab per path. Only the visible tab, at
    /// `active`, is listed: the others only remember their folder.
    /// Opens a window with a tab per path, arranged by `layout` (positions
    /// in `paths`; one pane of every tab without it), focused on the tab at
    /// `focused`. Only each pane's shown tab is listed: the others only
    /// remember their folder until they are shown.
    fn open_window(
        model: &mut Model,
        paths: Vec<Option<String>>,
        focused: usize,
        layout: Option<&FlatLayout>,
        group: Option<u32>,
    ) -> Command<Effect, Event> {
        if paths.is_empty() {
            return Command::done();
        }
        let window = WindowId::new(model.window_ids.next_id());
        let tabs: Vec<TabId> = paths
            .into_iter()
            .map(|path| {
                let tab = TabId::new(model.tab_ids.next_id());
                model.tabs.insert(
                    tab,
                    Tab {
                        path,
                        ..Tab::default()
                    },
                );
                tab
            })
            .collect();
        let focused = tabs[focused.min(tabs.len() - 1)];
        let layout = layout
            .and_then(|layout| Node::unflatten(layout, |index| tabs.get(index as usize).copied()))
            .filter(|layout| layout.tabs().len() == tabs.len())
            .unwrap_or_else(|| {
                Node::Pane(Pane {
                    active: tabs.iter().position(|&tab| tab == focused).unwrap_or(0),
                    tabs: tabs.clone(),
                })
            });
        let mut layout = layout;
        layout.activate(focused);
        let shown = layout.shown();
        model.windows.push(Window {
            id: window,
            layout,
            focused,
            group,
            frame: None,
        });
        let open: Command<Effect, Event> = Command::notify_shell(OpenWindow {
            window: window.number(),
        })
        .into();
        let lists: Vec<_> = shown
            .into_iter()
            .map(|tab| Self::show_tab(model, tab))
            .collect();
        open.and(Command::all(lists)).and(render())
    }

    /// Lists a tab that only remembered its folder.
    fn show_tab(model: &mut Model, id: TabId) -> Command<Effect, Event> {
        let Some(path) = model
            .tabs
            .get(&id)
            .filter(|tab| tab.generation == 0)
            .and_then(|tab| tab.path.clone())
        else {
            return Command::done();
        };
        Self::go(model, id, path, None)
    }

    fn state_loaded(
        model: &mut Model,
        open: Option<String>,
        home: String,
        loaded: LoadedState,
    ) -> Command<Effect, Event> {
        model.persistence.loaded = true;
        let state = match loaded {
            LoadedState::Found(state) => state,
            LoadedState::Missing | LoadedState::Failed(_) => PersistedState::default(),
        };
        model.folder_views = state.folder_views;
        model.default_view = state.default_view;
        model.columns = state.columns;
        model.sidebar.favorites = state.favorites;
        model.sidebar.recent = state.recent;
        model.sidebar.collapsed = state.collapsed_sections;
        model.sidebar.order = state.section_order;
        model.inspector_shown = state.inspector_shown;
        model.settings = state.settings;
        model.saved = state.saved_searches.normalized();
        model.remote.connections = state.connections;
        model.plugins.settings = state.plugins;
        if let Some(path) = open {
            return Self::new_window(model, Some(path));
        }
        let saved: Vec<_> = state
            .windows
            .into_iter()
            .filter(|window| !window.tabs.is_empty())
            .collect();
        if saved.is_empty() {
            return Self::new_window(model, Some(home));
        }
        // What is on disk already; restoring it needs no save.
        model.persistence.saved = Some(PersistedState {
            windows: saved.clone(),
            ..Self::persisted(model)
        });
        let windows: Vec<_> = saved
            .into_iter()
            .map(|window| {
                let paths = window.tabs.into_iter().map(Some).collect();
                let opened = Self::open_window(
                    model,
                    paths,
                    window.active as usize,
                    window.layout.as_ref(),
                    window.group,
                );
                if let Some(opened) = model.windows.last_mut() {
                    opened.frame = window.frame;
                }
                opened
            })
            .collect();
        Command::all(windows)
    }

    /// What would be saved now.
    fn persisted(model: &Model) -> PersistedState {
        let windows = model
            .windows
            .iter()
            .filter_map(|window| {
                // Tabs without a folder are not kept.
                let mut layout = window.layout.clone();
                for tab in window.layout.tabs() {
                    if model.tabs.get(&tab).and_then(|t| t.path.as_ref()).is_none() {
                        layout.remove(tab);
                    }
                }
                let kept = layout.tabs();
                if kept.is_empty() || layout.is_empty() {
                    return None;
                }
                let tabs: Vec<String> = kept
                    .iter()
                    .filter_map(|tab| model.tabs.get(tab)?.path.clone())
                    .collect();
                let position = |tab: TabId| {
                    kept.iter()
                        .position(|&t| t == tab)
                        .and_then(|index| u32::try_from(index).ok())
                        .unwrap_or_default()
                };
                let flat = layout.flatten(position);
                let single_pane = flat.splits.is_empty();
                Some(PersistedWindow {
                    active: position(window.focused),
                    // One pane needs no layout: state stays as before splits.
                    layout: (!single_pane).then_some(flat),
                    group: window.group,
                    frame: window.frame,
                    tabs,
                })
            })
            .collect();
        PersistedState {
            version: STATE_VERSION,
            windows,
            folder_views: model.folder_views.clone(),
            default_view: model.default_view,
            columns: model.columns.clone(),
            favorites: model.sidebar.favorites.clone(),
            recent: model.sidebar.recent.clone(),
            collapsed_sections: model.sidebar.collapsed.clone(),
            section_order: model.sidebar.order.clone(),
            inspector_shown: model.inspector_shown,
            settings: model.settings.clone(),
            saved_searches: model.saved.clone(),
            connections: model.remote.connections.clone(),
            plugins: model.plugins.settings.clone(),
        }
    }

    /// Asks for the state to be saved when it changed since it last was.
    fn save_if_changed(model: &mut Model) -> Command<Effect, Event> {
        let persistence = &model.persistence;
        if !persistence.loaded || persistence.quitting {
            return Command::done();
        }
        let state = Self::persisted(model);
        if persistence.saved.as_ref() == Some(&state) {
            return Command::done();
        }
        model.persistence.saved = Some(state.clone());
        Command::notify_shell(SaveState { state }).into()
    }

    /// The window was closed: its tabs stop, and their folders go on the
    /// reopen list unless the app is quitting.
    fn close_window(model: &mut Model, id: WindowId) -> Command<Effect, Event> {
        let Some(position) = model.windows.iter().position(|window| window.id == id) else {
            return Command::done();
        };
        let window = model.windows.remove(position);
        let quitting = model.persistence.quitting;
        let stops: Vec<_> = window
            .layout
            .tabs()
            .iter()
            .filter_map(|tab| model.tabs.remove(tab))
            .map(|mut tab| {
                if let Some(path) = tab.path.take().filter(|_| !quitting) {
                    model.closed.push(path);
                }
                tab.stop_reading()
            })
            .collect();
        let excess = model.closed.len().saturating_sub(workspace::CLOSED_KEPT);
        model.closed.drain(..excess);
        Command::all(stops).and(render())
    }

    /// The tab `tab` when `generation` is its latest navigation.
    fn current(model: &mut Model, tab: u32, generation: u64) -> Option<&mut Tab> {
        tab_id(tab)
            .and_then(|id| model.tabs.get_mut(&id))
            .filter(|state| state.generation == generation)
    }

    fn listed(
        model: &mut Model,
        tab: u32,
        generation: u64,
        output: ListDirOutput,
    ) -> Command<Effect, Event> {
        let Some(state) = Self::current(model, tab, generation) else {
            return Command::done();
        };
        state.loading = false;
        state.reading = false;
        // What the folder contains may have changed which plugins apply.
        if let Some(path) = state.path.clone() {
            model.plugins.forget_activation(&path);
            model.plugins.views.folder_changed(&path);
        }
        if let ListDirOutput::NeedsPassword { wrong } = output {
            return Self::listing_needs_password(model, tab, wrong);
        }
        if let ListDirOutput::NeedsConnection { connection } = &output {
            return Self::listing_needs_connection(model, tab, connection);
        }
        let Some(state) = Self::current(model, tab, generation) else {
            return Command::done();
        };
        match output {
            // Answered above.
            ListDirOutput::NeedsPassword { .. } | ListDirOutput::NeedsConnection { .. } => {}
            ListDirOutput::Listed {
                listing,
                order,
                view,
                opened,
            } => {
                let kept = state.keep_selection();
                state.listing = listing;
                state.opened = opened;
                state.order = order;
                state.restore_selection(&kept);
                // Back or Forward: where you were.
                if let Some(place) = state.returning_to.take() {
                    state.return_to(&place);
                }
                state.select_new();
                state.error = None;
                state.denied = false;
                // The view or search changed while the folder was read.
                if view != state.list_view() {
                    return Self::reorder(state, tab).and(render());
                }
            }
            ListDirOutput::Failed(error) => {
                state.error = Some(error);
                state.denied = false;
            }
            ListDirOutput::Denied(error) => {
                state.error = Some(error);
                state.denied = true;
            }
        }
        render().and(Self::refresh_if_asked(state, tab))
    }

    /// Reads the folder once more when the watcher started, or a change
    /// came, while the read that just ended was under way: that read may
    /// have missed it. Its rows show meanwhile.
    fn refresh_if_asked(state: &mut Tab, tab: u32) -> Command<Effect, Event> {
        if std::mem::take(&mut state.refresh_when_listed) {
            Command::event(Event::Refresh { tab })
        } else {
            Command::done()
        }
    }

    fn watched(
        model: &mut Model,
        tab: u32,
        generation: u64,
        event: &WatchEvent,
    ) -> Command<Effect, Event> {
        // Watches outlive re-reads for a search, so they go by their own id.
        let Some(state) = tab_id(tab)
            .and_then(|id| model.tabs.get_mut(&id))
            .filter(|state| {
                state
                    .watch
                    .as_ref()
                    .is_some_and(|(id, _)| *id == generation)
            })
        else {
            return Command::done();
        };
        match event {
            // The listing may have been read before the watcher started.
            // While it is still being read, reading again at once would
            // throw that read away and double the wait for the first rows:
            // it is read again when it has come.
            WatchEvent::Ready if state.reading => {
                state.refresh_when_listed = true;
                Command::done()
            }
            WatchEvent::Ready => Command::event(Event::Refresh { tab }),
            WatchEvent::Changed => {
                let dir = state.path.clone().unwrap_or_default();
                // During a read, reading again at once would throw that
                // read away, and changes that keep coming would keep a large
                // folder from ever showing: one more read follows it.
                let refresh = if state.reading {
                    state.refresh_when_listed = true;
                    Command::done()
                } else {
                    Command::event(Event::Refresh { tab })
                };
                refresh.and(Self::emit_plugin_event(
                    model,
                    "fs_changed",
                    &CallContext::default(),
                    &serde_json::json!({ "dirs": [dir] }),
                ))
            }
            // As egui: the listing stays, only without live updates.
            WatchEvent::Failed(_) => {
                state.watch = None;
                if model
                    .toasts
                    .is_shown(ToastKind::Warning, LIVE_UPDATES_UNAVAILABLE)
                {
                    return Command::done();
                }
                Self::toast(model, ToastKind::Warning, LIVE_UPDATES_UNAVAILABLE)
            }
        }
    }

    /// Changes how the tab's folder is shown, in every tab showing it, and
    /// remembers it for the folder. Listings are re-ordered, not re-read.
    fn change_view(
        model: &mut Model,
        tab: u32,
        change: impl FnOnce(&mut FolderView),
    ) -> Command<Effect, Event> {
        let Some(path) = tab_id(tab)
            .and_then(|id| model.tabs.get(&id))
            .and_then(Tab::view_key)
        else {
            return Command::done();
        };
        let mut view = model.view_for(&path);
        change(&mut view);
        model.folder_views.insert(path.clone(), view);
        let Model {
            tabs, generation, ..
        } = model;
        let reorders: Vec<_> = tabs
            .iter_mut()
            .filter(|(_, state)| state.view_key().as_deref() == Some(path.as_str()))
            .filter_map(|(id, state)| {
                let relist = state.view.list_view() != view.list_view();
                state.view = view;
                relist.then(|| Self::refilter(state, id.number(), generation))
            })
            .collect();
        render().and(Command::all(reorders))
    }

    /// Orders the tab's listing for its view; the tab is busy until then.
    fn reorder(state: &mut Tab, tab: u32) -> Command<Effect, Event> {
        state.loading = true;
        Command::request_from_shell(OrderListing {
            listing: Arc::clone(&state.listing),
            view: state.list_view(),
        })
        .then_send(move |output| Event::Ordered { tab, output })
    }

    /// Reads the tab's folder again; a walk starts over.
    fn refresh(model: &mut Model, tab: u32) -> Command<Effect, Event> {
        let Model {
            tabs, generation, ..
        } = model;
        let Some(state) = tab_id(tab).and_then(|id| tabs.get_mut(&id)) else {
            return Command::done();
        };
        if state.shows_folders() {
            return Self::reread(state, tab, generation);
        }
        Self::load(state, tab)
    }

    /// The events of the app-wide panels, each to its module.
    fn panel_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        match event {
            Event::DismissToast { id } => Self::dismiss_toast(model, id),
            Event::SetPaletteText { text } => {
                model.palette = Some(text);
                render()
            }
            Event::ClosePalette => {
                model.palette = None;
                render()
            }
            Event::ChangeSettings { .. }
            | Event::RestoreDefaultSettings
            | Event::SetFolderDefaults { .. } => Self::settings_event(model, event),
            Event::OpenDiskAccessSettings
            | Event::DismissDiskAccess { .. }
            | Event::RestartApp
            | Event::DiskAccessChecked { .. }
            | Event::RecheckDiskAccess
            | Event::Relaunching { .. }
            | Event::TerminalChecked { .. } => Self::access_event(model, event),
            Event::ThumbnailMade { .. }
            | Event::RetryPreview { .. }
            | Event::HoverRow { .. }
            | Event::HoverRested { .. }
            | Event::MediaNoticed { .. } => Self::thumbnail_event(model, event),
            event => Self::inspector_event(model, event),
        }
    }

    /// Forgets the typed text, unless typing went on since.
    fn typing_paused(model: &mut Model, tab: u32, typed: u64) -> Command<Effect, Event> {
        if let Some(state) = tab_id(tab).and_then(|id| model.tabs.get_mut(&id))
            && state.type_count == typed
        {
            state.type_prefix.clear();
        }
        Command::done()
    }

    /// Selects by typed name; the text is forgotten after a second's pause.
    fn type_select(model: &mut Model, tab: u32, text: &str) -> Command<Effect, Event> {
        use unicode_normalization::UnicodeNormalization;
        let Some(state) = tab_id(tab).and_then(|id| model.tabs.get_mut(&id)) else {
            return Command::done();
        };
        let continuing = !state.type_prefix.is_empty();
        // Folded like the listing's folded names.
        state
            .type_prefix
            .extend(text.nfc().flat_map(char::to_lowercase));
        state.type_count += 1;
        let typed = state.type_count;
        let prefix = state.type_prefix.clone();
        state.type_select(&prefix, continuing);
        render().and(
            Command::request_from_shell(Delay { millis: 1000 })
                .then_send(move |()| Event::TypingPaused { tab, typed }),
        )
    }

    /// The events that select rows or act on the selection.
    fn selection_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        let tab = match &event {
            Event::Click { tab, .. }
            | Event::ContextClick { tab, .. }
            | Event::MoveCursor { tab, .. }
            | Event::SetSelection { tab, .. }
            | Event::SelectAll { tab }
            | Event::InvertSelection { tab }
            | Event::ClearSelection { tab }
            | Event::OpenSelection { tab }
            | Event::QuickLook { tab }
            | Event::OpenPreviewed { tab, .. } => *tab,
            _ => return Command::done(),
        };
        let Some(state) = tab_id(tab).and_then(|id| model.tabs.get_mut(&id)) else {
            return Command::done();
        };
        match event {
            Event::Click {
                row,
                command,
                shift,
                ..
            } => state.click(row, command, shift),
            Event::ContextClick { row, .. } => {
                if state.selection.contains(row as usize) {
                    return Command::done();
                }
                state.click(row, false, false);
            }
            Event::MoveCursor {
                row,
                command,
                shift,
                ..
            } => state.move_cursor(row, command, shift),
            Event::SetSelection { rows, .. } => state.set_selection(&rows),
            Event::SelectAll { .. } => state.selection.select_all(state.order.len()),
            Event::InvertSelection { .. } => state.selection.invert(state.order.len()),
            Event::ClearSelection { .. } => state.selection.deselect_all(),
            Event::OpenSelection { .. } => return Self::open_selection(model, tab),
            Event::QuickLook { .. } => {
                let (paths, index) = state.selected_paths();
                if paths.is_empty() {
                    return Command::done();
                }
                return Command::notify_shell(QuickLook {
                    tab,
                    paths,
                    index: index as u32,
                })
                .into();
            }
            // The panel steps through items on its own: open by path.
            Event::OpenPreviewed { path, .. } => {
                let row = (0..state.order.len())
                    .find(|&row| {
                        state
                            .row_path(row)
                            .is_some_and(|row| row.to_string_lossy() == path)
                    })
                    .and_then(|row| u32::try_from(row).ok());
                return row.map_or_else(Command::done, |row| {
                    Command::event(Event::OpenRow { tab, row })
                });
            }
            _ => {}
        }
        render().and(Self::selection_moved(model, tab))
    }

    /// The events that change how a folder is shown.
    fn view_event(model: &mut Model, event: &Event) -> Command<Effect, Event> {
        match *event {
            Event::SortBy { tab, sort } => Self::change_view(model, tab, |view| view.sort_by(sort)),
            Event::SetReverse { tab, reverse } => {
                Self::change_view(model, tab, |view| view.invert = reverse)
            }
            Event::SetDisplay { tab, display } => {
                Self::change_view(model, tab, |view| view.display = display)
            }
            Event::SetShowHidden { tab, show } => {
                Self::change_view(model, tab, |view| view.show_hidden = show)
            }
            _ => Command::done(),
        }
    }

    /// The events that choose and size the list's columns.
    fn column_event(model: &mut Model, event: &Event) -> Command<Effect, Event> {
        match *event {
            Event::ToggleColumn { column } if Column::OPTIONAL.contains(&column) => {
                model.columns.toggle(column);
            }
            Event::SetColumnWidth { column, width } => {
                model.columns.set_width(column, width as f32);
            }
            Event::ResetColumns => model.columns = ListColumns::default(),
            _ => {}
        }
        render()
    }

    /// A new order for the tab, unless its listing was replaced meanwhile.
    fn ordered(model: &mut Model, tab: u32, output: Ordered) -> Command<Effect, Event> {
        let Some(state) = tab_id(tab).and_then(|id| model.tabs.get_mut(&id)) else {
            return Command::done();
        };
        if !Arc::ptr_eq(&state.listing, &output.listing) {
            return Command::done();
        }
        let kept = state.keep_selection();
        state.order = output.order;
        state.restore_selection(&kept);
        // The view or search changed again while this order was made.
        if output.view != state.list_view() {
            return Self::reorder(state, tab).and(render());
        }
        state.loading = false;
        render().and(Self::refresh_if_asked(state, tab))
    }

    /// The row at `position` of the tab.
    fn row_view(
        tab: &Tab,
        position: u32,
        model: &Model,
        highlights: Option<&crate::search::filter::Highlights>,
        show_folder: bool,
        edge: u32,
    ) -> RowView {
        let (columns, files) = (&model.columns, &model.files);
        let entry = tab.order[position as usize] as usize;
        let meta = tab.listing.meta(entry);
        let name = tab.listing.name(entry);
        let path = tab.row_path(position as usize);
        RowView {
            index: position,
            name: name.to_owned(),
            is_dir: meta.entry_type == EntryType::Directory,
            selected: tab.selection.contains(position as usize),
            cells: columns
                .shown
                .iter()
                .map(|&column| match column {
                    Column::Modified => format::when(meta.modified_at, model.now),
                    Column::Created => format::when(meta.created_at, model.now),
                    Column::Accessed => format::when(tab.last_opened(entry), model.now),
                    Column::OpenCount => match tab.opened.get(entry) {
                        Some(&(count, _)) if count > 0 => count.to_string(),
                        _ => String::new(),
                    },
                    _ => format::cell(column, name, meta),
                })
                .collect(),
            // Dates are exact until the clock ticks: no tooltip needed then.
            tooltip: if model.now == 0 {
                String::new()
            } else {
                tab.exact_dates(entry)
            },
            detail: if show_folder {
                String::new()
            } else if meta.entry_type == EntryType::File {
                format::size(meta)
            } else {
                String::new()
            },
            highlights: finder::ranges(highlights, name),
            cut: !files.cut_is_empty() && path.as_ref().is_some_and(|path| files.is_cut(path)),
            thumbnail_choice: path.as_ref().map_or(ThumbnailChoice::None, |path| {
                model.thumbnails.choice(&path.to_string_lossy(), meta)
            }),
            thumbnail: path.as_ref().map_or_else(String::new, |path| {
                model.thumbnails.file(&path.to_string_lossy(), edge, meta)
            }),
            animation: match &path {
                Some(path)
                    if tab.playing_row(model.settings.animate_selected_previews)
                        == Some(position as usize) =>
                {
                    model
                        .thumbnails
                        .animation(&path.to_string_lossy(), edge, meta)
                }
                _ => String::new(),
            },
            folder: if show_folder {
                finder::relative_dir(
                    tab.listing.dir(entry),
                    // Results from several roots show their full folder.
                    if tab.scope.is_some() {
                        None
                    } else {
                        tab.walk_root.as_deref().or(tab.path.as_deref())
                    },
                )
            } else {
                String::new()
            },
        }
    }

    fn tab_view(id: TabId, tab: &Tab, model: &Model) -> TabViewModel {
        let (crumbs_above, mut crumbs) = tab.crumb_views(&model.home);
        // A server's top is called what its connection is.
        for crumb in &mut crumbs {
            if let Some(name) = model.remote.top_title(&crumb.path) {
                crumb.title = name;
            }
        }
        let listing = &tab.listing;
        let total_rows = u32::try_from(tab.order.len()).unwrap_or(u32::MAX);
        let (first_row, end) = row_window(tab.first_row, tab.visible_rows, total_rows);
        let highlights = tab.highlights();
        let show_folder = tab.shows_folders();
        let edge = thumbnails::row_edge(tab, model);
        let rows = (first_row..end)
            .map(|position| {
                Self::row_view(tab, position, model, highlights.as_ref(), show_folder, edge)
            })
            .collect();
        // Counted only for an empty view, so a large folder costs nothing.
        let hidden_count = if tab.order.is_empty() && !tab.view.show_hidden {
            let hidden = (0..listing.len())
                .filter(|&index| {
                    is_shown(listing.name(index), listing.meta(index), true)
                        && !is_shown(listing.name(index), listing.meta(index), false)
                })
                .count();
            u32::try_from(hidden).unwrap_or(u32::MAX)
        } else {
            0
        };
        let path = tab.path.clone().unwrap_or_default();
        let can_go_up = Location::parse(&path).parent().is_some();
        TabViewModel {
            id: id.number(),
            title: model.remote.top_title(&path).unwrap_or_else(|| tab.title()),
            path,
            location: tab.location_view(),
            can_extract: tab.can_extract(),
            compress: tab.compress_view(),
            view: tab.view,
            nav: NavView {
                back: tab.history.can_go_back(),
                forward: tab.history.can_go_forward(),
                up: can_go_up,
            },
            scroll_to: tab.scroll_to,
            scroll_count: tab.scroll_count,
            loading: tab.loading || tab.walk.is_some(),
            error: tab.error.clone(),
            denied: tab.denied,
            progress: if tab.walk.is_some() {
                let folders = tab.listing.dirs().len();
                format!(
                    "Scanning {folders} folder{}…",
                    if folders == 1 { "" } else { "s" }
                )
            } else {
                String::new()
            },
            total_rows,
            hidden_count,
            in_trash: tab.trash,
            batch_rename: tab.batch.as_ref().map(batch::BatchRename::view),
            folder_plugin_actions: model.plugins.folder_actions_of(tab),
            plugin_bar: Self::plugin_bar_view(model, tab),
            plugin_sections: Self::plugin_sections_view(model, id.number(), tab),
            plugin_actions: model.plugins.item_actions_of(tab),
            drive_status: tab
                .path
                .as_deref()
                .and_then(|path| Self::drive_status(model, path)),
            transfer_targets: if tab.trash {
                Vec::new()
            } else {
                Self::transfer_targets(model, tab.path.as_deref())
            },
            search: tab.search_view(!model.sidebar.favorites.is_empty()),
            show_folder,
            renaming: tab.rename_view(),
            crumbs,
            crumbs_above,
            children: tab.children_view(),
            goto: Self::goto_view(model, tab),
            inspector: model
                .inspector_shown
                .then(|| tab.inspector_view(&model.sizes, &model.thumbnails, &model.checksums)),
            saved: tab.saved_badge(&model.saved.searches),
            selection: tab.selected_ranges(),
            selected_count: u32::try_from(tab.selection.len()).unwrap_or(u32::MAX),
            cursor: tab.selection.cursor().map(|row| row as u32),
            first_row,
            rows,
        }
    }
}

impl App for DirFleet {
    type Event = Event;
    type Model = Model;
    type ViewModel = ViewModel;
    type Effect = Effect;

    fn update(&self, event: Event, model: &mut Model) -> Command<Effect, Event> {
        let command = Self::handle(event, model);
        command
            .and(Self::save_if_changed(model))
            .and(Self::request_thumbnails(model))
            .and(Self::request_activation(model))
            .and(Self::request_plugin_views(model))
    }

    fn view(&self, model: &Model) -> ViewModel {
        Self::view_of(model)
    }
}

impl DirFleet {
    /// Reads the saved state and starts what runs for as long as the app
    /// does: drives, media notices, plugin messages and the clock.
    fn started(model: &mut Model, open: Option<String>, home: String) -> Command<Effect, Event> {
        model.home.clone_from(&home);
        Command::request_from_shell(LoadState {
            version: STATE_VERSION,
        })
        .then_send(move |loaded| Event::StateLoaded { open, home, loaded })
        .and(Self::start_sidebar())
        .and(Self::start_media())
        .and(Self::load_thumbnail_choices())
        .and(Self::start_plugins())
        .and(Command::stream_from_shell(Clock).then_send(|now| Event::ClockTicked { now }))
    }

    fn handle(event: Event, model: &mut Model) -> Command<Effect, Event> {
        match event {
            Event::Started { open, home } => Self::started(model, open, home),
            Event::StateLoaded { open, home, loaded } => {
                let command = Self::state_loaded(model, open, home, loaded);
                command
                    .and(Self::check_access(model))
                    .and(Self::load_plugins())
            }
            Event::ClockTicked { now } => {
                model.now = now;
                render()
            }
            Event::Quitting => {
                model.persistence.quitting = true;
                Command::done()
            }
            Event::NewWindow { path } => Self::new_window(model, path),
            Event::WindowClosed { window } => {
                window_id(window).map_or_else(Command::done, |id| Self::close_window(model, id))
            }
            Event::Navigate { tab, path } => Self::navigate(model, tab, path),
            Event::OpenRow { tab, row } => Self::open_row(model, tab, row),
            Event::Back { tab } => Self::back(model, tab),
            Event::Forward { tab } => Self::forward(model, tab),
            Event::Up { tab } => Self::up(model, tab),
            Event::Refresh { tab } => Self::refresh(model, tab),
            Event::OpenTrash { tab } => Self::open_trash(model, tab),
            Event::EmptyTrash
            | Event::PurgeSelection { .. }
            | Event::RestoreSelection { .. }
            | Event::TrashChanged { .. } => Self::trash_event(model, event),
            Event::OpenFolder { tab } => Command::request_from_shell(PickFolder {
                title: "Open folder".into(),
            })
            .then_send(move |output| Event::FolderPicked { tab, output }),
            Event::FolderPicked {
                tab,
                output: PickFolderOutput::Picked(path),
            } => Command::event(Event::Navigate { tab, path }),
            Event::FolderPicked {
                output: PickFolderOutput::Cancelled,
                ..
            } => Command::done(),
            Event::SortBy { .. }
            | Event::SetReverse { .. }
            | Event::SetDisplay { .. }
            | Event::SetShowHidden { .. } => Self::view_event(model, &event),
            Event::ToggleColumn { .. } | Event::SetColumnWidth { .. } | Event::ResetColumns => {
                Self::column_event(model, &event)
            }
            Event::Ordered { tab, output } => Self::ordered(model, tab, output),
            Event::ViewportChanged {
                tab,
                first_row,
                rows,
            } => {
                let Some(state) = tab_id(tab).and_then(|id| model.tabs.get_mut(&id)) else {
                    return Command::done();
                };
                if (first_row, rows) == (state.first_row, state.visible_rows) {
                    return Command::done();
                }
                let total = u32::try_from(state.order.len()).unwrap_or(u32::MAX);
                let before = row_window(state.first_row, state.visible_rows, total);
                state.first_row = first_row;
                state.visible_rows = rows;
                // Within the rows already sent: nothing new to show. The
                // thumbnails post-pass still sees the new viewport.
                if row_window(first_row, rows, total) == before {
                    return Command::done();
                }
                render()
            }
            Event::Listed {
                tab,
                generation,
                output,
            } => Self::listed(model, tab, generation, output),
            Event::Watched {
                tab,
                generation,
                event,
            } => Self::watched(model, tab, generation, &event),
            // Tabs and panes, selection or search: each module has its events.
            event if event.is_selection() => Self::selection_event(model, event),
            event if event.is_search() => Self::search_event(model, event),
            event if event.is_sidebar() => Self::sidebar_event(model, event),
            Event::TypeSelect { tab, text } => Self::type_select(model, tab, &text),
            Event::TypingPaused { tab, typed } => Self::typing_paused(model, tab, typed),
            event if event.is_saved() => Self::saved_event(model, event),
            event if event.is_picker() => Self::picker_event(model, event),
            event if event.is_plugin() => Self::plugin_event(model, event),
            event if event.is_place() => Self::place_event(model, event),
            event if event.is_panel() => Self::panel_event(model, event),
            event if event.is_system() => Self::system_event(model, event),
            event if event.is_file() => Self::file_event(model, event),
            event if event.is_pathbar() => Self::pathbar_event(model, event),
            event => {
                debug_assert!(event.is_workspace(), "unrouted event: {event:?}");
                Self::workspace_event(model, event)
            }
        }
    }

    /// The events of checksums, archives and servers: each module has its own.
    fn place_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        if event.is_checksum() {
            Self::checksum_event(model, event)
        } else if event.is_archive() {
            Self::archive_event(model, event)
        } else {
            Self::remote_event(model, event)
        }
    }

    /// Every sort, in menu order, with its label.
    fn sort_choices() -> Vec<SortChoice> {
        Sort::ALL
            .iter()
            .map(|&sort| SortChoice {
                sort,
                label: sort.label().to_owned(),
            })
            .collect()
    }

    /// One window: its panes' tabs, the focused one among them, its sidebar.
    fn window_view(model: &Model, window: &Window) -> WindowView {
        let title = |id: &TabId| {
            let tab = model.tabs.get(id)?;
            Some(TabTitle {
                id: id.number(),
                title: tab
                    .path
                    .as_deref()
                    .and_then(|path| model.remote.top_title(path))
                    .unwrap_or_else(|| tab.title()),
            })
        };
        let tab_view = |id: TabId| {
            model
                .tabs
                .get(&id)
                .map(|tab| Self::tab_view(id, tab, model))
                .unwrap_or_default()
        };
        let panes: Vec<PaneView> = window
            .layout
            .panes()
            .into_iter()
            .map(|pane| PaneView {
                tabs: pane.tabs.iter().filter_map(title).collect(),
                shown: pane.active_tab().map(tab_view).unwrap_or_default(),
            })
            .collect();
        // The focused tab is one of the panes': built once, copied.
        let focused = window.focused.number();
        let active = panes
            .iter()
            .find(|pane| pane.shown.id == focused)
            .map_or_else(|| tab_view(window.focused), |pane| pane.shown.clone());
        WindowView {
            id: window.id.number(),
            tabs: window.layout.tabs().iter().filter_map(title).collect(),
            active,
            panes,
            layout: window.layout.flatten(TabId::number),
            group: window.group,
            frame: window.frame,
            sidebar: model.sidebar.view(
                model.tabs.get(&window.focused),
                &model.settings,
                &model.saved.searches,
                &model.remote,
            ),
        }
    }

    fn view_of(model: &Model) -> ViewModel {
        let windows = model
            .windows
            .iter()
            .map(|window| Self::window_view(model, window))
            .collect();
        let columns = std::iter::once(Column::Name)
            .chain(model.columns.shown.iter().copied())
            .map(|column| ColumnView {
                column,
                title: column.title(false).to_owned(),
                width: if column == Column::Name {
                    0
                } else {
                    model.columns.width(column) as u32
                },
                right_aligned: column.right_aligned(),
                sort: column.sort(),
            })
            .collect();
        let optional_columns = Column::OPTIONAL
            .iter()
            .map(|&column| ColumnChoice {
                column,
                title: column.title(false).to_owned(),
                shown: model.columns.is_shown(column),
            })
            .collect();
        ViewModel {
            windows,
            sorts: Self::sort_choices(),
            columns,
            optional_columns,
            trash: TrashInfo::here(),
            search_filters: finder::filter_groups(),
            toasts: model.toasts.views(),
            inspector_shown: model.inspector_shown,
            settings: model.settings.clone(),
            commands: palette::command_views(),
            palette: model.palette.as_ref().map(|text| PaletteView {
                text: text.clone(),
                items: palette::palette_items(model, text),
            }),
            saved_searches: saved::views(&model.saved.searches),
            icon_sizes: settings::icon_size_choices(),
            folder_defaults: model.default_view,
            operations: model.files.operations(),
            activity: model.files.activity(model.now),
            eject_confirm: Self::eject_confirm(model),
            search_help: finder::search_help(),
            access: model.access.view(),
            cut_count: model.files.cut_count(),
            thumbnail_picker: model.picker.as_ref().map(picker::Picker::view),
            plugins: model.plugins.view(),
            plugin_form: model.plugins.form_view(),
            plugin_jobs: model.plugins.job_views(),
            plugin_shortcuts: model.plugins.bindings().0,
            plugin_results: Self::results_view(model),
            undo: model.files.undo_label(),
            redo: model.files.redo_label(),
            conflict: model.files.conflict(),
            archive_password: model.archives.password_view(),
            connect: model.remote.connect_view(),
            host_key: model.remote.host_key_view(),
        }
    }
}

#[cfg(test)]
mod tests;
