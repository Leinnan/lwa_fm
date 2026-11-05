use crate::app::assets::AssetManager;
use crate::app::commands::{COMMANDS_QUEUE, TabAction, TabTarget};
use crate::app::directory_path_info::DirectoryPathInfo;
use crate::app::directory_view_settings::{DirectoryShowHidden, DirectoryViewSettings};
use crate::app::dock::{CurrentPath, WindowId};
use crate::helper::DataHolder;
use crate::locations::Locations;
use crate::watcher::DirectoryWatchers;
use crate::{app::settings::ApplicationSettings, locations::Location};
use command_palette::CommandPalette;
use commands::{ActionToPerform, ModalWindow};
use egui::TextBuffer;
use egui_mev_widgets::{ToastKind, Toasts};
use rayon::ThreadPoolBuilder;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

pub mod assets;
mod breadcrumbs;
mod central_panel;
pub mod command_palette;
pub mod commands;
pub mod database;
pub mod dir_handling;
pub mod directory_path_info;
mod directory_view_settings;
#[cfg(target_os = "macos")]
mod disk_access;
pub mod dock;
mod drives_ui;
pub mod file_ops;
pub mod history;
mod inspector;
pub mod keymap;
pub mod saved_searches;
pub mod search;
pub mod search_state;
pub mod undo;
pub use search::{MatchMode, SavedSearch, SavedSearches, Search, SearchTermType};
pub mod menus;
pub mod open_history;
mod path_editor;
#[cfg(test)]
mod perf_tests;
mod plugins_ui;
mod settings;
mod side_panel;
pub mod thumbnail_choice;
mod thumbnail_picker;
mod top_bottom;
mod trash_view;
pub mod ui;
#[cfg(test)]
mod ui_tests;

/// Dedicated thread pool for filesystem reads. Deep walks run their
/// per-directory tasks here, so it scales with the machine (bounded, to keep
/// disk concurrency sane). It stays separate from the global rayon pool, which
/// the UI thread uses for sorting and filtering.
static BG_POOL: LazyLock<rayon::ThreadPool> = LazyLock::new(|| {
    ThreadPoolBuilder::new()
        .num_threads(std::thread::available_parallelism().map_or(2, |n| n.get().clamp(2, 8)))
        .thread_name(|i| format!("lwa_fm_bg_{i}"))
        .build()
        .expect("Failed to create background thread pool")
});

/// Toasts waiting for the next frame. `toast!` runs on threads without a `Context` too.
static PENDING_TOASTS: std::sync::Mutex<Vec<(ToastKind, String)>> =
    std::sync::Mutex::new(Vec::new());

pub fn queue_toast(kind: ToastKind, text: String) {
    PENDING_TOASTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push((kind, text));
    crate::watcher::request_repaint();
}

fn show_toasts(ctx: &egui::Context) {
    let pending = std::mem::take(
        &mut *PENDING_TOASTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    for (kind, text) in pending {
        Toasts::push(ctx, text, kind);
    }
    Toasts::show(ctx);
}

#[macro_export]
macro_rules! toast{
        (Basic, $($format:expr),+) => {
            $crate::app::queue_toast(egui_mev_widgets::ToastKind::Info, format!($($format),+));
        };
        (Info, $($format:expr),+) => {
            $crate::app::queue_toast(egui_mev_widgets::ToastKind::Info, format!($($format),+));
        };
        (Warning, $($format:expr),+) => {
            $crate::app::queue_toast(egui_mev_widgets::ToastKind::Warn, format!($($format),+));
        };
        (Error, $($format:expr),+) => {
            $crate::app::queue_toast(egui_mev_widgets::ToastKind::Error, format!($($format),+));
        };
        (Success, $($format:expr),+) => {
            $crate::app::queue_toast(egui_mev_widgets::ToastKind::Ok, format!($($format),+));
        };
    }

#[derive(Deserialize, Serialize)]
#[serde(default)] // if we add new fields, give them default values when deserializing old state
pub struct App {
    #[cfg(windows)]
    #[serde(skip)]
    chrome: egui_mev_app::NativeChrome,
    user_locations: Locations,
    /// Mounted drives, kept current by a background monitor.
    #[serde(skip)]
    drives: drives_ui::DrivesState,
    /// The Full Disk Access banner's state.
    #[cfg(target_os = "macos")]
    #[serde(skip)]
    disk_access: disk_access::DiskAccessPrompt,
    #[serde(skip)]
    tabs: crate::app::dock::Workspace,
    /// Folders of the open tabs, saved so they come back after a restart.
    #[serde(default)]
    open_tabs: Vec<PathBuf>,
    /// Every window's panes and tab folders, saved so they come back after
    /// a restart. Newer than `open_tabs`, which only older builds read.
    #[serde(default)]
    windows: Vec<crate::app::dock::SavedWindow>,
    /// Recently opened folders, newest first.
    #[serde(default)]
    recent_folders: Vec<PathBuf>,
    /// [`open_history::generation`] the views were last sorted for.
    open_history_generation: u64,
    /// Whether the window had focus last frame.
    window_focused: bool,
    pub settings: ApplicationSettings,
    #[serde(skip)]
    display_modal: Option<ModalWindow>,
    /// The window showing the open dialog; the others wait behind it.
    #[serde(skip)]
    modal_window: Option<WindowId>,
    /// The window that last had keyboard focus. Toasts show there, and
    /// background work that names no window acts on it.
    #[serde(skip, default = "main_window")]
    focused_window: WindowId,
    #[serde(skip)]
    command_palette: CommandPalette,
    #[serde(skip, default)]
    pub watchers: DirectoryWatchers,
    #[serde(skip, default)]
    assets: AssetManager,
    /// Structural (folder-level) changes awaiting a debounced refresh, mapped to
    /// the instant they were first seen. Coalesces bursts into one refresh.
    #[serde(skip, default)]
    pending_structural_dirs: BTreeMap<PathBuf, Instant>,
    #[serde(skip)]
    file_ops: file_ops::FileOps,
    #[serde(skip)]
    rename: Option<file_ops::RenameState>,
    #[serde(skip)]
    pending_delete: file_ops::PendingDelete,
    #[serde(skip)]
    pending_drop: Option<file_ops::PendingDrop>,
    #[serde(skip)]
    pending_transfer: Option<file_ops::PendingTransfer>,
    #[serde(skip)]
    choose_folder: Option<file_ops::ChooseFolder>,
    #[serde(skip)]
    saved_editor: Option<saved_searches::SavedSearchEditor>,
    #[serde(skip)]
    batch_rename: Option<file_ops::BatchRename>,
    #[serde(skip)]
    thumbnail_picker: Option<thumbnail_picker::ThumbnailPicker>,
    /// Lua plugins: their workers, jobs, form and bar contents.
    #[serde(skip)]
    plugins: crate::plugins::PluginHost,
    /// Clipboard sequence number when items were cut, to notice replacement.
    #[serde(skip)]
    cut_sequence: u64,
    #[cfg(feature = "profiling")]
    #[serde(skip)]
    profiler_visible: bool,
    #[cfg(feature = "profiling")]
    #[serde(skip)]
    frame_tracker: crate::profiler::FrameTracker,
}

impl App {
    fn render_modal(&mut self, ctx: &egui::Context) {
        // Another dialog replaced the thumbnail picker: free its frames.
        if self.thumbnail_picker.is_some()
            && self.display_modal != Some(ModalWindow::SelectThumbnail)
        {
            self.thumbnail_picker = None;
            self.assets.close_overview_frames();
        }
        if let Some(modal) = &self.display_modal {
            match modal {
                ModalWindow::Settings => {
                    self.settings.display(ctx, &mut self.plugins);
                }
                ModalWindow::Commands => {
                    self.command_palette.ui(ctx);
                }
                ModalWindow::Shortcuts => {
                    if keymap::help_ui(ctx) {
                        ActionToPerform::CloseActiveModalWindow.schedule();
                    }
                }
                ModalWindow::Rename => self.rename_ui(ctx),
                ModalWindow::ConfirmDelete => self.confirm_delete_ui(ctx),
                ModalWindow::ConfirmDrop => self.confirm_drop_ui(ctx),
                ModalWindow::Conflict => self.conflict_ui(ctx),
                ModalWindow::ChooseFolder => self.choose_folder_ui(ctx),
                ModalWindow::EditSavedSearch => self.saved_search_editor_ui(ctx),
                ModalWindow::BatchRename => self.batch_rename_ui(ctx),
                ModalWindow::ConfirmEject => self.confirm_eject_ui(ctx),
                ModalWindow::PluginForm => {
                    if self.plugins.form_ui(ctx) {
                        // A form opened from a results dialog goes back to it.
                        if self.plugins.take_form_return().is_some() {
                            ActionToPerform::ToggleModalWindow(ModalWindow::PluginResults)
                                .schedule();
                        } else {
                            ActionToPerform::CloseActiveModalWindow.schedule();
                        }
                    }
                }
                ModalWindow::PluginResults => match self.plugins.panel_ui(ctx) {
                    crate::plugins::panel::PanelOutcome::Open => {}
                    crate::plugins::panel::PanelOutcome::Hide => {
                        ActionToPerform::CloseActiveModalWindow.schedule();
                    }
                    crate::plugins::panel::PanelOutcome::OpenRow(path) => {
                        ActionToPerform::CloseActiveModalWindow.schedule();
                        ActionToPerform::NewTab(path).schedule();
                    }
                    crate::plugins::panel::PanelOutcome::RowAction { action, path } => {
                        self.run_plugin_action(
                            &action,
                            crate::plugins::CallContext {
                                dir: Some(path),
                                ..Default::default()
                            },
                        );
                    }
                },
                ModalWindow::SelectThumbnail => self.select_thumbnail_ui(ctx),
            }
        }
    }

    fn reconcile_tab_watchers(&mut self, tab_id: crate::app::dock::TabId) {
        let (old_watcher_specs, new_watcher_specs) = {
            let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                return;
            };
            let old_watcher_specs = tab.active_watch_specs.clone();
            let new_watcher_specs = tab.watcher_specs();
            tab.active_watch_specs.clone_from(&new_watcher_specs);
            (old_watcher_specs, new_watcher_specs)
        };

        let old_specs_by_path = old_watcher_specs
            .iter()
            .cloned()
            .collect::<std::collections::BTreeMap<_, _>>();
        let new_specs_by_path = new_watcher_specs
            .iter()
            .cloned()
            .collect::<std::collections::BTreeMap<_, _>>();

        let removed_watchers = old_specs_by_path
            .keys()
            .filter(|path| !new_specs_by_path.contains_key(*path))
            .cloned()
            .collect::<Vec<_>>();
        let added_watchers = new_specs_by_path
            .iter()
            .filter(|(path, _)| !old_specs_by_path.contains_key(*path))
            .map(|(path, mode)| (path.clone(), *mode))
            .collect::<Vec<_>>();
        let restarted_watchers = new_specs_by_path
            .iter()
            .filter_map(|(path, new_mode)| {
                old_specs_by_path.get(path).and_then(|old_mode| {
                    if old_mode == new_mode {
                        None
                    } else {
                        Some((path.clone(), *new_mode))
                    }
                })
            })
            .collect::<Vec<_>>();

        self.watchers.stop_many(removed_watchers);
        self.watchers
            .stop_many(restarted_watchers.iter().map(|(path, _)| path.clone()));
        self.watchers.start_many(added_watchers);
        self.watchers.start_many(restarted_watchers);
    }

    fn process_file_system_changes(&mut self, ctx: &egui::Context) {
        const STRUCTURAL_COALESCE: Duration = Duration::from_millis(150);
        self.watchers.check_for_new_watchers();

        let changes = self.watchers.check_for_file_system_events();
        let now = Instant::now();
        let mut degraded_structural: BTreeSet<PathBuf> = BTreeSet::new();
        self.route_file_changes(changes.changes, ctx, &mut degraded_structural);

        // Debounce structural changes (watcher-reported + surgical fallbacks)
        // so a burst (git checkout, npm install) collapses into one refresh per
        // tab instead of one per frame. Modified-file changes above are handled
        // independently and stay near-instant.
        for dir in changes
            .structural_dirs
            .iter()
            .chain(degraded_structural.iter())
        {
            self.pending_structural_dirs
                .entry(dir.clone())
                .or_insert(now);
        }
        let ready_structural: BTreeSet<PathBuf> = self
            .pending_structural_dirs
            .iter()
            .filter(|&(_dir, first_seen)| now.duration_since(*first_seen) >= STRUCTURAL_COALESCE)
            .map(|(dir, _first_seen)| dir.clone())
            .collect();
        for dir in &ready_structural {
            self.pending_structural_dirs.remove(dir);
        }
        if !self.pending_structural_dirs.is_empty() {
            ctx.request_repaint_after(STRUCTURAL_COALESCE);
        }

        if ready_structural.is_empty() {
            return;
        }

        self.assets
            .invalidate_directories(ready_structural.iter().cloned());
        crate::app::database::invalidate_dirs(ready_structural.iter().cloned());
        let refreshed: Vec<PathBuf> = ready_structural.iter().cloned().collect();
        self.plugins.folders_changed(&refreshed);
        if self.plugins.wants("fs_changed") {
            let dirs: Vec<String> = refreshed
                .iter()
                .map(|dir| dir.to_string_lossy().into_owned())
                .collect();
            self.plugins.emit(
                "fs_changed",
                &crate::plugins::CallContext::default(),
                &serde_json::json!({ "dirs": dirs }),
            );
        }

        let tab_ids = self.tabs.get_tab_ids();
        let affected_tabs = tab_ids
            .iter()
            .copied()
            .filter(|tab_id| {
                self.tabs
                    .get_tab_by_id(*tab_id)
                    .is_some_and(|tab| tab.should_refresh_for_directories(&ready_structural))
            })
            .collect::<Vec<_>>();

        for tab_id in affected_tabs {
            self.handle_action(
                ctx,
                ActionToPerform::TabAction(
                    TabTarget::TabWithId(tab_id),
                    TabAction::RequestFilesRefresh,
                ),
            );
        }
    }

    /// Route a watcher batch grouped by parent. Each tab and the persistent
    /// directory cache see one mutation per parent, followed by one sort/filter
    /// and one asynchronous cache write.
    fn route_file_changes(
        &mut self,
        file_changes: Vec<crate::watcher::ResolvedChange>,
        ctx: &egui::Context,
        degraded_structural: &mut BTreeSet<PathBuf>,
    ) {
        let mut by_parent = BTreeMap::<PathBuf, Vec<crate::watcher::ResolvedChange>>::new();
        for change in file_changes {
            let Some(parent) = change.path.parent().map(crate::helper::canonical_form) else {
                continue;
            };
            by_parent.entry(parent).or_default().push(change);
        }

        for (parent, changes) in by_parent {
            self.assets
                .invalidate_files(changes.iter().map(|change| change.path.clone()));
            crate::app::database::apply_file_changes(parent.clone(), changes.clone());
            self.plugins.folders_changed(std::slice::from_ref(&parent));
            self.apply_file_changes_to_tabs(&parent, &changes, ctx, degraded_structural);
        }
    }

    fn apply_file_changes_to_tabs(
        &mut self,
        parent: &Path,
        changes: &[crate::watcher::ResolvedChange],
        ctx: &egui::Context,
        degraded: &mut BTreeSet<PathBuf>,
    ) {
        let tab_ids = self.tabs.get_tab_ids();
        let affected_parent = BTreeSet::from([parent.to_path_buf()]);
        for tab_id in tab_ids {
            let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                continue;
            };
            if !tab.should_refresh_for_directories(&affected_parent) {
                continue;
            }
            let settings =
                ctx.data_get_path_or_persisted::<DirectoryViewSettings>(&tab.current_path);
            if !tab.apply_file_changes(changes, &settings.data) {
                degraded.insert(parent.to_path_buf());
            }
        }
    }

    /// Re-sorts views ordered by opening when the open history changes, and
    /// re-reads their folders when the window regains focus, as files may
    /// have been opened in other apps meanwhile.
    fn sync_open_history(&mut self, ctx: &egui::Context) {
        open_history::set_tracking(self.settings.track_open_history);
        let by_opening = |sorting: Sort| matches!(sorting, Sort::Accessed | Sort::MostOpened);
        let generation = open_history::generation();
        if generation != self.open_history_generation {
            self.open_history_generation = generation;
            for tab_id in self.tabs.get_tab_ids() {
                let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                    continue;
                };
                let settings =
                    ctx.data_get_path_or_persisted::<DirectoryViewSettings>(&tab.current_path);
                if by_opening(settings.data.sorting) || tab.filters_by_opening() {
                    tab.sort_entries(&settings.data);
                    tab.update_visible_entries();
                }
            }
            ctx.request_repaint();
        }
        let focused = ctx.input(|input| input.viewport().focused.unwrap_or(true));
        if focused && !self.window_focused {
            for tab in self.tabs.tabs() {
                let settings =
                    ctx.data_get_path_or_persisted::<DirectoryViewSettings>(&tab.current_path);
                if settings.data.sorting == Sort::Accessed || tab.filters_by_opening() {
                    TabAction::ForceRefresh.schedule_tab(tab.id);
                }
            }
        }
        self.window_focused = focused;
    }

    /// Runs queued actions, each in the window it came from.
    fn drain_command_queue(&mut self, ctx: &egui::Context) {
        let current = self.tabs.current_window();
        while let Some(queued) = COMMANDS_QUEUE.pop_queued() {
            self.tabs
                .set_current_window(queued.window.unwrap_or(self.focused_window));
            let window = self.tabs.current_window();
            commands::in_window(window, || self.handle_action(ctx, queued.action));
            self.claim_modal();
        }
        self.tabs.set_current_window(current);
    }

    /// A dialog that just opened belongs to the current window.
    const fn claim_modal(&mut self) {
        match (&self.display_modal, self.modal_window) {
            (Some(_), None) => self.modal_window = Some(self.tabs.current_window()),
            (None, Some(_)) => self.modal_window = None,
            _ => {}
        }
    }

    /// Width of the current window's content, as of its last frame.
    fn window_width(&self, ctx: &egui::Context) -> f32 {
        self.tabs
            .window()
            .content_rect
            .unwrap_or_else(|| ctx.content_rect())
            .width()
    }
}

#[derive(Deserialize, Serialize, Default, PartialEq, Eq, Debug, Clone, Copy)]
pub enum Sort {
    #[default]
    Name,
    Modified,
    Created,
    Accessed,
    MostOpened,
    Size,
    Kind,
    Random,
}

impl Sort {
    pub const ALL: [Self; 8] = [
        Self::Name,
        Self::Modified,
        Self::Created,
        Self::Accessed,
        Self::MostOpened,
        Self::Size,
        Self::Kind,
        Self::Random,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Modified => "Date modified",
            Self::Created => "Date created",
            Self::Accessed => "Date opened",
            Self::MostOpened => "Times opened",
            Self::Size => "Size",
            Self::Kind => "Kind",
            Self::Random => "Random",
        }
    }
}

#[derive(Deserialize, Serialize, Default, PartialEq, Eq, Debug, Clone, Copy)]
pub enum DisplayType {
    #[default]
    List,
    Icons,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum DataSource {
    Settings,
    Local,
    #[default]
    Generated,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
pub struct Data<T> {
    pub data: T,
    pub source: DataSource,
}

#[allow(dead_code)]
impl<T> Data<T> {
    /// Returns true if the data is from the local source.
    pub const fn is_local(&self) -> bool {
        matches!(self.source, DataSource::Local)
    }
    /// Returns true if the data is from the global source.
    pub const fn is_global(&self) -> bool {
        matches!(self.source, DataSource::Settings)
    }
    /// Creates a new data instance with the local source.
    pub const fn from_local(data: T) -> Self {
        Self {
            data,
            source: DataSource::Local,
        }
    }
    /// Creates a new data instance with the global source.
    pub const fn from_settings(data: T) -> Self {
        Self {
            data,
            source: DataSource::Settings,
        }
    }
    /// Creates a new data instance with the generated source.
    pub const fn generated(data: T) -> Self {
        Self {
            data,
            source: DataSource::Generated,
        }
    }
}

impl<T> Deref for Data<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}
impl<T> DerefMut for Data<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}

impl Default for App {
    fn default() -> Self {
        let command_palette = CommandPalette::default();
        Self {
            drives: drives_ui::DrivesState::default(),
            #[cfg(target_os = "macos")]
            disk_access: disk_access::DiskAccessPrompt::default(),
            #[cfg(windows)]
            chrome: egui_mev_app::NativeChrome::new(egui_mev_theme::AccentShades::system()),
            user_locations: Locations::get_user_dirs(),
            tabs: crate::app::dock::Workspace::new(&get_starting_path()),
            open_tabs: Vec::new(),
            windows: Vec::new(),
            recent_folders: Vec::new(),
            open_history_generation: open_history::generation(),
            window_focused: true,
            settings: ApplicationSettings::default(),
            display_modal: None,
            modal_window: None,
            focused_window: WindowId::MAIN,
            command_palette,
            watchers: DirectoryWatchers::default(),
            assets: AssetManager::default(),
            pending_structural_dirs: BTreeMap::new(),
            file_ops: file_ops::FileOps::default(),
            rename: None,
            pending_delete: file_ops::PendingDelete::default(),
            pending_drop: None,
            pending_transfer: None,
            cut_sequence: 0,
            choose_folder: None,
            saved_editor: None,
            batch_rename: None,
            thumbnail_picker: None,
            plugins: crate::plugins::PluginHost::default(),
            #[cfg(feature = "profiling")]
            profiler_visible: true,
            #[cfg(feature = "profiling")]
            frame_tracker: crate::profiler::FrameTracker::new(),
        }
    }
}

impl App {
    fn load_locations(&mut self, ctx: &egui::Context) {
        self.drives = drives_ui::DrivesState::start(ctx);
        self.sync_drive_targets();
        self.user_locations = Locations::get_user_dirs();
    }
    /// Called once before the first frame.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        ui::install_theme(&cc.egui_ctx);
        crate::watcher::set_repaint_context(&cc.egui_ctx);

        #[cfg(debug_assertions)]
        cc.egui_ctx.global_style_mut(|style| {
            style.debug.show_unaligned = false;
        });

        // Load previous app state (if any).
        // Note that you must enable the `persistence` feature for this to work.
        if let Some(storage) = cc.storage {
            let mut value: Self = eframe::get_value(storage, eframe::APP_KEY).unwrap_or_default();

            value.load_locations(&cc.egui_ctx);
            value.tabs = restore_tabs(std::mem::take(&mut value.windows), &value.open_tabs);
            value.sync_drive_targets();
            value.assets = AssetManager::default();
            value.assets.set_icon_size(value.settings.icon_size);
            value.plugins = crate::plugins::PluginHost::start();
            #[cfg(target_os = "macos")]
            {
                value.disk_access =
                    disk_access::DiskAccessPrompt::at_launch(value.settings.ask_full_disk_access);
            }
            #[cfg(feature = "profiling")]
            {
                value.profiler_visible = true;
            }
            return value;
        }

        let mut app = Self::default();
        app.load_locations(&cc.egui_ctx);
        app.plugins = crate::plugins::PluginHost::start();
        #[cfg(target_os = "macos")]
        {
            app.disk_access = disk_access::DiskAccessPrompt::at_launch(true);
        }
        app
    }

    #[allow(clippy::too_many_lines)]
    fn handle_action(&mut self, ctx: &egui::Context, action: ActionToPerform) {
        #[cfg(feature = "profiling")]
        puffin::profile_function!("lwa_fm::handle_action");

        match action {
            ActionToPerform::ToggleSidebar => {
                if self.window_width(ctx) < 800.0 {
                    let window = self.tabs.window_mut();
                    window.sidebar_overlay = !window.sidebar_overlay;
                } else {
                    self.settings.sidebar_visible = !self.settings.sidebar_visible;
                }
            }
            ActionToPerform::ToggleInspector => {
                if self.window_width(ctx) < 1000.0 {
                    let window = self.tabs.window_mut();
                    window.inspector_overlay = !window.inspector_overlay;
                } else {
                    self.settings.inspector_visible = !self.settings.inspector_visible;
                }
            }
            ActionToPerform::TabAction(target, action) => {
                if target == TabTarget::AllTabs {
                    let tabs_ids = self.tabs.get_tab_ids();
                    for id in tabs_ids {
                        ActionToPerform::TabAction(TabTarget::TabWithId(id), action.clone())
                            .schedule();
                    }
                    return;
                }
                let tab_id = match target {
                    TabTarget::ActiveTab => self.tabs.get_current_index(),
                    TabTarget::TabWithId(id) => Some(id),
                    TabTarget::AllTabs => None,
                };
                let Some(tab_id) = tab_id else {
                    return;
                };
                match action {
                    commands::TabAction::ChangePaths(path) => {
                        #[cfg(feature = "profiling")]
                        puffin::profile_scope!(
                            "lwa_fm::handle_action::ChangePaths: {}",
                            path.get_name_from_path()
                        );
                        if let CurrentPath::One(folder) = &path
                            && self.plugins.wants("folder_opened")
                        {
                            self.plugins.emit(
                                "folder_opened",
                                &crate::plugins::CallContext {
                                    dir: Some(folder.clone()),
                                    tab: Some(tab_id.number()),
                                    ..Default::default()
                                },
                                &serde_json::Value::Null,
                            );
                        }
                        let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                            return;
                        };
                        if let CurrentPath::One(folder) = &path {
                            self.recent_folders.retain(|recent| recent != folder);
                            self.recent_folders.insert(0, folder.clone());
                            self.recent_folders.truncate(12);
                        }
                        // Opening a folder ends the search (the history keeps
                        // it), unless a history entry is being restored.
                        if std::mem::take(&mut tab.keep_search_once) {
                        } else if matches!(path, CurrentPath::One(_)) && tab.search.is_some() {
                            tab.end_search();
                            tab.search_committed = None;
                        }
                        tab.set_path(path);
                        tab.refresh_generation
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if let Some(edit) = &tab.path_edit {
                            // An address being typed survives only when it
                            // is where the tab went.
                            tab.path_edit = tab
                                .current_path
                                .single_path()
                                .filter(|p| Path::new(&edit.text_input) == p.as_path())
                                .map(|p| DirectoryPathInfo::build(&p));
                        }
                        self.handle_action(
                            ctx,
                            ActionToPerform::TabAction(
                                TabTarget::TabWithId(tab_id),
                                TabAction::RequestFilesRefresh,
                            ),
                        );
                    }
                    commands::TabAction::Restore(entry) => {
                        let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                            return;
                        };
                        tab.search_dirty_since = None;
                        tab.search.clone_from(&entry.search);
                        tab.search_committed = entry.search;
                        if tab.current_path == entry.path {
                            tab.update_visible_entries();
                            TabAction::RequestFilesRefresh.schedule_tab(tab_id);
                        } else {
                            tab.keep_search_once = true;
                            self.handle_action(
                                ctx,
                                ActionToPerform::TabAction(
                                    TabTarget::TabWithId(tab_id),
                                    TabAction::ChangePaths(entry.path),
                                ),
                            );
                        }
                    }
                    commands::TabAction::FilterChanged => {
                        #[cfg(feature = "profiling")]
                        puffin::profile_scope!("lwa_fm::handle_action::FilterChanged");
                        let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                            return;
                        };
                        tab.update_settings(ctx);
                        tab.update_visible_entries();
                    }
                    commands::TabAction::ForceRefresh => {
                        #[cfg(feature = "profiling")]
                        puffin::profile_scope!("lwa_fm::handle_action::ForceRefresh");
                        // Invalidate sled cache for all watched directories
                        let force_dirs: Vec<PathBuf> = {
                            #[cfg(feature = "profiling")]
                            puffin::profile_scope!(
                                "lwa_fm::handle_action::ForceRefresh::collect_dirs"
                            );
                            let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                                return;
                            };
                            let mut dirs: Vec<PathBuf> = tab.current_path.get_paths();
                            if let Some(search) = &tab.search {
                                dirs.extend(search.extra_dirs().cloned());
                                dirs.sort();
                                dirs.dedup();
                            }
                            dirs
                        };
                        crate::app::database::invalidate_dirs(force_dirs);
                        // Fall through to normal refresh
                        self.handle_action(
                            ctx,
                            ActionToPerform::TabAction(
                                TabTarget::TabWithId(tab_id),
                                TabAction::RequestFilesRefresh,
                            ),
                        );
                    }
                    commands::TabAction::RequestFilesRefresh => {
                        #[cfg(feature = "profiling")]
                        puffin::profile_scope!("lwa_fm::handle_action::RefreshFiles");
                        // A tab not shown yet reads its folder when it is.
                        if self
                            .tabs
                            .get_tab_by_id(tab_id)
                            .is_some_and(|tab| tab.unloaded)
                        {
                            return;
                        }
                        self.reconcile_tab_watchers(tab_id);
                        let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                            return;
                        };
                        tab.refresh_generation
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        tab.cancel_token
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                        if tab.load.defer_refresh() {
                            return;
                        }
                        tab.update_settings(ctx);

                        if tab.current_path.is_trash() {
                            Self::load_trash(tab, ctx);
                            return;
                        }
                        let mut directories: Vec<PathBuf> = tab.current_path.get_paths();
                        if let Some(search) = &tab.search {
                            directories.extend(search.extra_dirs().cloned());
                            directories.sort();
                            directories.dedup();
                        }
                        let depth = tab.search_depth();
                        let show_hidden = tab.show_hidden;
                        let current_path = tab.current_path.clone();
                        let search = tab.search.clone();

                        let settings =
                            ctx.data_get_path_or_persisted::<DirectoryViewSettings>(&current_path);

                        tab.load = crate::app::dock::LoadState::loading(None);
                        tab.cancel_token
                            .store(false, std::sync::atomic::Ordering::SeqCst);
                        let refresh_gen = std::sync::Arc::clone(&tab.refresh_generation);
                        let generation = refresh_gen.load(std::sync::atomic::Ordering::SeqCst);
                        let cancel = std::sync::Arc::clone(&tab.cancel_token);

                        BG_POOL.spawn(move || {
                            #[cfg(feature = "profiling")]
                            puffin::profile_scope!(
                                "lwa_fm::handle_action::RefreshFiles::bg_thread"
                            );
                            // A superseded or cancelled task MUST still post a terminal
                            // `FilesLoaded`. That handler is the only thing that clears
                            // the loading state, and it is where a deferred rerun (set when a
                            // refresh is requested while one is already running) gets
                            // re-spawned. Bailing silently leaves the tab parked in
                            // loading forever, so a depth/path change made during a
                            // slow (e.g. deep favorites) search would never take effect.
                            //
                            // The generation carried here is always stale at abort time
                            // (cancel/gen are only flipped by a *newer* RequestFilesRefresh),
                            // so the handler discards the empty payload without blanking
                            // the view, clears `loading`, and re-spawns with fresh inputs.
                            let abort = move || {
                                COMMANDS_QUEUE.push(ActionToPerform::TabAction(
                                    TabTarget::TabWithId(tab_id),
                                    TabAction::FilesLoaded {
                                        listing: std::sync::Arc::default(),
                                        order: Vec::new(),
                                        visible: Vec::new(),
                                        generation,
                                        linked_dirs: Vec::new(),
                                    },
                                ));
                            };
                            if refresh_gen.load(std::sync::atomic::Ordering::SeqCst) != generation {
                                abort();
                                return;
                            }
                            if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                                abort();
                                return;
                            }

                            let progress = |msg: &str| {
                                COMMANDS_QUEUE.push(ActionToPerform::TabAction(
                                    TabTarget::TabWithId(tab_id),
                                    TabAction::FilesProgress {
                                        progress: msg.to_string(),
                                        generation,
                                    },
                                ));
                            };

                            progress(&format!(
                                "Scanning {} director{}…",
                                directories.len(),
                                if directories.len() == 1 { "y" } else { "ies" }
                            ));

                            // Deep walks publish partial (sorted, filtered) results
                            // so the first matches appear while scanning continues.
                            let partial_search = search.clone();
                            let partial_settings = settings.data.clone();
                            let partial_refresh_gen = std::sync::Arc::clone(&refresh_gen);
                            let on_partial =
                                move |listing: std::sync::Arc<crate::data::files::Listing>| {
                                    if partial_refresh_gen.load(std::sync::atomic::Ordering::SeqCst)
                                        != generation
                                    {
                                        return;
                                    }
                                    let order = crate::app::dir_handling::sort_order(
                                        &listing,
                                        &partial_settings,
                                    );
                                    let visible = crate::app::dir_handling::filter_visible(
                                        &listing,
                                        &order,
                                        show_hidden,
                                        partial_search.as_ref(),
                                    );
                                    COMMANDS_QUEUE.push(ActionToPerform::TabAction(
                                        TabTarget::TabWithId(tab_id),
                                        TabAction::FilesPartial {
                                            listing,
                                            order,
                                            visible,
                                            generation,
                                        },
                                    ));
                                };
                            let crate::app::dir_handling::WalkResult {
                                listing,
                                linked_dirs,
                            } = crate::app::dir_handling::read_directory_streaming(
                                &directories,
                                depth,
                                show_hidden,
                                &cancel,
                                &on_partial,
                            );
                            if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                                abort();
                                return;
                            }

                            progress(&format!("Sorting {} entries…", listing.len()));

                            let order =
                                crate::app::dir_handling::sort_order(&listing, &settings.data);
                            #[cfg(feature = "profiling")]
                            puffin::profile_scope!(
                                "lwa_fm::handle_action::RefreshFiles::filter_visible"
                            );
                            let visible = crate::app::dir_handling::filter_visible(
                                &listing,
                                &order,
                                show_hidden,
                                search.as_ref(),
                            );
                            COMMANDS_QUEUE.push(ActionToPerform::TabAction(
                                TabTarget::TabWithId(tab_id),
                                TabAction::FilesLoaded {
                                    listing,
                                    order,
                                    visible,
                                    generation,
                                    linked_dirs,
                                },
                            ));
                        });
                    }
                    commands::TabAction::FilesSort => {
                        #[cfg(feature = "profiling")]
                        puffin::profile_scope!("lwa_fm::handle_action::FilesSort");
                        let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                            return;
                        };

                        let settings = ctx
                            .data_get_path_or_persisted::<DirectoryViewSettings>(&tab.current_path);
                        tab.sort_entries(&settings.data);
                        tab.update_visible_entries();
                    }
                    commands::TabAction::FilesLoaded {
                        listing,
                        order,
                        visible,
                        generation,
                        linked_dirs,
                    } => {
                        #[cfg(feature = "profiling")]
                        puffin::profile_scope!("lwa_fm::handle_action::FilesLoaded");
                        let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                            return;
                        };
                        if tab
                            .refresh_generation
                            .load(std::sync::atomic::Ordering::SeqCst)
                            != generation
                        {
                            // Stale generation — but if the list is empty, apply as a
                            // visual fallback so the user sees something while the
                            // "true" refresh is still in flight.
                            if tab.visible_entries.is_empty() {
                                tab.set_listing(listing, order, visible);
                            }
                            if tab.load.finish() {
                                TabAction::RequestFilesRefresh.schedule_tab(tab_id);
                            }
                            return;
                        }
                        tab.update_settings(ctx);
                        tab.set_listing(listing, order, visible);
                        let linked_changed = tab.set_linked_watch_roots(linked_dirs);
                        if tab.load.finish() {
                            TabAction::RequestFilesRefresh.schedule_tab(tab_id);
                        }
                        if linked_changed {
                            self.reconcile_tab_watchers(tab_id);
                        }
                    }
                    commands::TabAction::FilesPartial {
                        listing,
                        order,
                        visible,
                        generation,
                    } => {
                        let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                            return;
                        };
                        if tab.load.is_loading()
                            && tab
                                .refresh_generation
                                .load(std::sync::atomic::Ordering::SeqCst)
                                == generation
                        {
                            tab.set_listing(listing, order, visible);
                        }
                    }
                    commands::TabAction::FilesProgress {
                        progress,
                        generation: progress_gen,
                    } => {
                        let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                            return;
                        };
                        if tab
                            .refresh_generation
                            .load(std::sync::atomic::Ordering::SeqCst)
                            != progress_gen
                        {
                            return;
                        }
                        tab.load.set_progress(progress);
                    }
                    commands::TabAction::SearchInFavorites(start) => {
                        let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                            return;
                        };
                        let favorites = ctx.data_get_persisted::<Locations>().unwrap_or_default();
                        if favorites.locations.is_empty() {
                            return;
                        }
                        if start {
                            self.handle_action(
                                ctx,
                                ActionToPerform::TabAction(
                                    TabTarget::TabWithId(tab_id),
                                    TabAction::ChangePaths(CurrentPath::Multiple(
                                        favorites.paths(),
                                    )),
                                ),
                            );
                        } else {
                            let Some(previous_path_action) = tab.go_back() else {
                                return;
                            };
                            self.handle_action(ctx, previous_path_action);
                        }
                    }
                }
            }
            ActionToPerform::NewTab(path) => {
                self.tabs.open_in_new_tab(&path);
            }
            ActionToPerform::NewWindow(path) => {
                self.tabs.open_window_at(&path);
            }
            ActionToPerform::OpenTrash { new_tab } => self.open_trash(ctx, new_tab),
            ActionToPerform::ConfirmEmptyTrash => self.confirm_empty_trash(),
            ActionToPerform::EjectDrive { mount, force } => self.eject_drive(ctx, &mount, force),
            ActionToPerform::DriveEjected {
                mount,
                name,
                result,
            } => self.drive_ejected(mount, &name, result),
            ActionToPerform::RestoreTrashSelection => self.restore_trash_selection(ctx),
            ActionToPerform::OpenInTerminal(path_buf) => {
                match self.settings.open_in_terminal(&path_buf) {
                    Ok(_) => {
                        toast!(Success, "Open in terminal");
                    }
                    Err(_) => {
                        toast!(Error, "Failed to open directory");
                    }
                }
            }
            ActionToPerform::CloseActiveModalWindow => {
                self.display_modal = None;
                self.assets.set_icon_size(self.settings.icon_size);
                TabAction::RequestFilesRefresh.schedule_active_tab();
            }
            ActionToPerform::ViewSettingsChanged(source) => {
                let Some(active_tab) = self.tabs.get_current_tab() else {
                    return;
                };
                let active_path = active_tab.current_path.clone();
                for tab_id in self.tabs.get_tab_ids() {
                    if let Some(tab) = self.tabs.get_tab_by_id(tab_id)
                        && (source == DataSource::Settings || tab.current_path == active_path)
                    {
                        let hidden_before = tab.show_hidden;
                        tab.update_settings(ctx);
                        if tab.show_hidden == hidden_before {
                            TabAction::FilesSort.schedule_tab(tab_id);
                        } else {
                            TabAction::RequestFilesRefresh.schedule_tab(tab_id);
                        }
                    }
                }
            }
            ActionToPerform::ToggleModalWindow(modal_window) => {
                if let Some(modal) = &self.display_modal {
                    if modal.eq(&modal_window) {
                        self.display_modal = None;
                    } else {
                        self.display_modal = Some(modal_window);
                    }
                } else {
                    self.display_modal = Some(modal_window);
                }
            }
            ActionToPerform::ToggleTopEdit => {
                let favorites: Vec<(String, PathBuf)> = ctx
                    .data_get_persisted::<Locations>()
                    .unwrap_or_default()
                    .locations
                    .iter()
                    .map(|location| {
                        (
                            location.name.to_string(),
                            PathBuf::from(location.path.as_ref()),
                        )
                    })
                    .collect();
                let recent: &[PathBuf] = if self.settings.show_recent_folders {
                    &self.recent_folders
                } else {
                    &[]
                };
                let Some(tab) = self.tabs.get_current_tab() else {
                    return;
                };
                if tab.path_edit.take().is_none()
                    && let Some(path) = tab.current_path.single_path()
                {
                    tab.path_edit =
                        Some(DirectoryPathInfo::build(&path).with_shortcuts(favorites, recent));
                    ctx.memory_mut(|memory| {
                        memory.request_focus(egui::Id::unique(("path_editor", tab.id)));
                    });
                }
            }
            ActionToPerform::AddToFavorites(path) => {
                let mut favorites = ctx.data_get_persisted::<Locations>().unwrap_or_default();
                if favorites
                    .locations
                    .iter()
                    .any(|location| location.path == path)
                {
                    return;
                }
                let path_buf = PathBuf::from_str(&path).unwrap();
                let Some(name) = path_buf.iter().next_back() else {
                    toast!(Error, "Could not get name of file");
                    return;
                };
                favorites.locations.push(Location {
                    name: Cow::Owned(name.to_string_lossy().to_string()),
                    path,
                });
                ctx.data_set_persisted(favorites);
            }
            ActionToPerform::RemoveFromFavorites(path_buf) => {
                let mut favorites = ctx.data_get_persisted::<Locations>().unwrap_or_default();
                favorites.locations.retain(|s| s.path != path_buf);
                ctx.data_set_persisted(favorites);
            }
            ActionToPerform::SystemOpen(cow) => {
                if open::that_detached(cow.as_str()).is_err() {
                    toast!(Error, "Could not open {cow}");
                } else {
                    open_history::opened(Path::new(cow.as_str()));
                }
            }
            ActionToPerform::RunCommand(command) => self.run_command(ctx, command),
            ActionToPerform::FileOp(op) => {
                self.submit_file_op(ctx, op, file_ops::AfterOp::Select);
            }
            ActionToPerform::FileOpEvent(event) => self.handle_file_op_event(event),
            ActionToPerform::Transfer { srcs, dst, moving } => {
                self.transfer(ctx, srcs, dst, moving);
            }
            ActionToPerform::DropTransfer { srcs, dst, moving } => {
                self.drop_transfer(ctx, srcs, dst, moving);
            }
            ActionToPerform::DropToTrash(paths) => self.drop_to_trash(ctx, paths),
            ActionToPerform::BeginRename(path) => self.begin_rename(path),
            ActionToPerform::PasteInto(folder) => self.paste_into(ctx, folder),
            ActionToPerform::RunSavedSearch { id, target } => {
                self.run_saved_search(ctx, id, target);
            }
            ActionToPerform::EditSavedSearch(id) => self.open_edit_dialog(ctx, id),
            ActionToPerform::DuplicateSavedSearch(id) => self.duplicate_saved_search(ctx, id),
            ActionToPerform::DeleteSavedSearch(id) => self.delete_saved_search(ctx, id),
            ActionToPerform::MoveSavedSearch { id, to } => self.move_saved_search(ctx, id, to),
            ActionToPerform::ChooseTransferTarget { srcs, moving } => {
                let start = self.current_folder().unwrap_or_default();
                self.choose_folder = Some(file_ops::ChooseFolder::new(srcs, moving, &start));
                self.display_modal = Some(ModalWindow::ChooseFolder);
            }
            ActionToPerform::RunPluginAction { action, context } => {
                self.run_plugin_action(&action, context);
            }
            ActionToPerform::Plugin(message) => self.handle_plugin_message(ctx, *message),
            ActionToPerform::SelectThumbnail(entry) => self.open_thumbnail_picker(*entry),
            ActionToPerform::ResetThumbnail(path) => self.set_thumbnail_choice(&path, None),
        }
    }
}

impl App {
    /// Runs an app-wide command from the keymap (shortcut or mouse button).
    #[allow(clippy::too_many_lines)]
    pub(crate) fn run_command(&mut self, ctx: &egui::Context, command: keymap::Command) {
        use keymap::Command;
        // Recycle Bin rows are not files at their shown paths, so only
        // delete-for-good applies to them.
        if self.in_trash_view() {
            match command {
                Command::Delete | Command::DeletePermanently => {
                    self.purge_trash_selection();
                    return;
                }
                Command::Rename
                | Command::NewFolder
                | Command::Duplicate
                | Command::Cut
                | Command::Copy
                | Command::Paste
                | Command::CopyAsPath => return,
                _ => {}
            }
        }
        match command {
            Command::Back | Command::Forward | Command::Up => {
                let Some(tab) = self.tabs.get_current_tab() else {
                    return;
                };
                let action = match command {
                    Command::Back => tab.go_back(),
                    Command::Forward => tab.go_forward(),
                    _ => tab.current_path.parent().map(|parent| {
                        ActionToPerform::TabAction(
                            TabTarget::TabWithId(tab.id),
                            TabAction::ChangePaths(parent.into()),
                        )
                    }),
                };
                if let Some(action) = action {
                    action.schedule();
                }
            }
            Command::Refresh => TabAction::ForceRefresh.schedule_active_tab(),
            Command::FocusSearch => {
                let Some(tab) = self.tabs.get_current_tab() else {
                    return;
                };
                if tab.search.is_none() {
                    tab.toggle_search();
                    TabAction::FilterChanged.schedule_tab(tab.id);
                } else {
                    // The search panel re-requests focus when the flag is gone.
                    let id = egui::Id::unique(("search_focus", tab.id));
                    ctx.data_mut(|data| data.remove::<bool>(id));
                }
            }
            Command::FocusAddress => {
                let Some(tab) = self.tabs.get_current_tab() else {
                    return;
                };
                if tab.path_edit.is_some() {
                    ctx.memory_mut(|memory| {
                        memory.request_focus(egui::Id::unique(("path_editor", tab.id)));
                    });
                } else {
                    ActionToPerform::ToggleTopEdit.schedule();
                }
            }
            Command::Palette => {
                if let Some(current_path) = self.tabs.get_current_path() {
                    let favorites = ctx.data_get_persisted::<Locations>().unwrap_or_default();
                    self.command_palette.build_for_path(
                        &CurrentPath::One(current_path.clone()),
                        &current_path,
                        &favorites,
                    );
                    self.command_palette
                        .commands
                        .extend(command_palette::app_commands(ctx));
                    let drive_commands = self.drive_palette_commands();
                    self.command_palette.commands.extend(drive_commands);
                    let plugin_commands = self.plugin_palette_commands(ctx);
                    self.command_palette.commands.extend(plugin_commands);
                    self.command_palette
                        .commands
                        .extend(self.recent_folders.iter().skip(1).map(|folder| {
                            command_palette::ValidAction {
                                action: ActionToPerform::TabAction(
                                    TabTarget::ActiveTab,
                                    TabAction::ChangePaths(folder.clone().into()),
                                ),
                                name: Cow::Owned(format!(
                                    "Go to recent folder: {}",
                                    folder.display()
                                )),
                                shortcut: None,
                            }
                        }));
                    self.command_palette.commands.extend(
                        saved_searches::load_saved(ctx)
                            .searches
                            .into_iter()
                            .map(|saved| command_palette::ValidAction {
                                action: ActionToPerform::RunSavedSearch {
                                    id: saved.id,
                                    target: saved_searches::RunTarget::ActiveTab,
                                },
                                name: Cow::Owned(format!("Run saved search: {}", saved.name)),
                                shortcut: None,
                            }),
                    );
                    ActionToPerform::ToggleModalWindow(ModalWindow::Commands).schedule();
                }
            }
            Command::Settings => {
                ActionToPerform::ToggleModalWindow(ModalWindow::Settings).schedule();
            }
            Command::ShortcutHelp => {
                ActionToPerform::ToggleModalWindow(ModalWindow::Shortcuts).schedule();
            }
            Command::ToggleSidebar => ActionToPerform::ToggleSidebar.schedule(),
            Command::ToggleInspector => ActionToPerform::ToggleInspector.schedule(),
            Command::ToggleHidden => {
                let Some(tab) = self.tabs.get_current_tab() else {
                    return;
                };
                let mut show_hidden =
                    ctx.data_get_path_or_persisted::<DirectoryShowHidden>(&tab.current_path);
                show_hidden.0 = !show_hidden.0;
                ctx.data_set_path(&tab.current_path, show_hidden.data);
                ActionToPerform::ViewSettingsChanged(DataSource::Local).schedule();
            }
            Command::ViewList | Command::ViewGrid => {
                let Some(tab) = self.tabs.get_current_tab() else {
                    return;
                };
                let mut view = ctx
                    .data_get_path_or_persisted::<DirectoryViewSettings>(&tab.current_path)
                    .data;
                view.display_type = if command == Command::ViewList {
                    DisplayType::List
                } else {
                    DisplayType::Icons
                };
                ctx.data_set_path(&tab.current_path, view);
                ActionToPerform::ViewSettingsChanged(DataSource::Local).schedule();
            }
            Command::NewTab => {
                if let Some(path) = self.tabs.get_current_path() {
                    self.tabs.open_in_new_tab(&path);
                }
            }
            Command::NewWindow => {
                if let Some(path) = self.tabs.get_current_path() {
                    self.tabs.open_window_at(&path);
                }
            }
            Command::Delete => self.trash_selection(ctx),
            Command::DeletePermanently => self.confirm_delete_selection(),
            Command::Rename => self.rename_selection(),
            Command::NewFolder => self.new_item(ctx, true),
            Command::Duplicate => self.duplicate_selection(ctx),
            Command::Cut => self.copy_selection(crate::clipboard::Mode::Cut),
            Command::Copy => self.copy_selection(crate::clipboard::Mode::Copy),
            Command::Paste => {
                if let Some(folder) = self.current_folder() {
                    self.paste_into(ctx, folder);
                } else {
                    toast!(Info, "Open a single folder to paste into it.");
                }
            }
            Command::CopyAsPath => {
                let paths = self.selected_paths();
                if !paths.is_empty() {
                    ctx.copy_text(file_ops::paths_as_text(&paths));
                }
            }
            Command::SaveSearch => {
                let saved = self
                    .tabs
                    .get_current_tab()
                    .and_then(|tab| tab.search.as_ref())
                    .map(|search| search.saved_from);
                match saved {
                    Some(Some(_)) => self.update_saved_from_tab(ctx),
                    Some(None) => self.open_save_dialog(ctx),
                    None => {}
                }
            }
            Command::MatchCase => {
                let Some(tab) = self.tabs.get_current_tab() else {
                    return;
                };
                let tab_id = tab.id;
                if let Some(search) = tab.search.as_mut() {
                    search.case_sensitive = !search.case_sensitive;
                    TabAction::FilterChanged.schedule_tab(tab_id);
                    self.commit_search(tab_id, "match case");
                }
            }
            Command::Undo => self.undo(ctx, false),
            Command::Redo => self.undo(ctx, true),
            Command::CloseTab => self.tabs.close_active_tab(),
            Command::NextTab => self.tabs.cycle_active_tab(true),
            Command::SelectTab(n) => self.tabs.select_tab(n),
            Command::ReopenClosedTab => self.tabs.reopen_closed_tab(),
            Command::PreviousTab => self.tabs.cycle_active_tab(false),
            #[cfg(feature = "profiling")]
            Command::Profiler => self.profiler_visible = !self.profiler_visible,
        }
    }

    pub(crate) fn render_browser(&mut self, root: &mut egui::Ui) {
        let ctx = &root.ctx().clone();
        self.settings.appearance.apply(ctx);
        if self.display_modal.is_some() {
            root.disable();
        }
        // The file view takes keyboard input unless a dialog or overlay is
        // open; text fields are excluded where the keys are handled.
        let window = self.tabs.window();
        let takes_keys =
            self.display_modal.is_none() && !window.sidebar_overlay && !window.inspector_overlay;
        self.tabs.focused = takes_keys;
        self.tabs.reconcile_selection();
        if takes_keys {
            for command in keymap::pressed(ctx) {
                self.run_command(ctx, command);
            }
            for action in crate::plugins::PluginHost::pressed(ctx) {
                self.run_plugin_action(&action, crate::plugins::CallContext::default());
            }
        }
        self.top_panel(root);
        #[cfg(target_os = "macos")]
        if self.tabs.current_window() == WindowId::MAIN {
            self.disk_access_banner(root);
        }
        self.bottom_panel(root);
        self.plugin_bar(root);
        self.left_side_panel(root);
        self.central_panel(root);
        self.panel_overlays(ctx);
    }

    /// Draws the current window: its panels and tabs, plus the open dialog
    /// and toasts when they belong to it.
    fn render_window(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let id = self.tabs.current_window();
        let (focused, outer, content) = ctx.input(|i| {
            let viewport = i.viewport();
            (viewport.focused, viewport.outer_rect, i.content_rect())
        });
        if focused == Some(true) {
            self.focused_window = id;
        }
        let window = self.tabs.window_mut();
        window.outer_rect = outer;
        window.content_rect = Some(content);
        commands::in_window(id, || {
            if self.display_modal.is_none() {
                self.handle_external_drop(&ctx);
            }
            self.render_browser(ui);
            self.hand_off_external_drag(&ctx);
            self.claim_modal();
            if self.modal_window == Some(id) {
                self.render_modal(&ctx);
            }
        });
        if self.focused_window == id {
            Self::drag_badge(&ctx);
            show_toasts(&ctx);
        }
    }

    /// Draws every window but the main one, each in its own OS window.
    fn render_other_windows(&mut self, ctx: &egui::Context) {
        for id in self.tabs.window_ids() {
            if id == WindowId::MAIN {
                continue;
            }
            let Some(window) = self.tabs.window_by_id_mut(id) else {
                continue;
            };
            let mut builder = egui::ViewportBuilder::default()
                .with_app_id(crate::consts::APP_ID)
                .with_title(crate::consts::APP_NAME)
                .with_icon(std::sync::Arc::clone(&WINDOW_ICON))
                .with_min_inner_size([640.0, 420.0])
                .with_inner_size([1100.0, 720.0]);
            if let Some(rect) = window.open_at {
                builder = builder
                    .with_position(rect.min)
                    .with_inner_size(rect.size().max(egui::vec2(640.0, 420.0)));
            }
            ctx.show_viewport_immediate(id.viewport(), builder, |ui, _class| {
                self.tabs.set_current_window(id);
                self.render_window(ui);
                if ui.input(|i| i.viewport().close_requested()) {
                    self.tabs.close_window(id);
                }
            });
        }
        self.tabs.set_current_window(WindowId::MAIN);
    }
}

/// The app icon, for windows opened after the first.
static WINDOW_ICON: LazyLock<std::sync::Arc<egui::IconData>> = LazyLock::new(|| {
    std::sync::Arc::new(
        eframe::icon_data::from_png_bytes(crate::consts::WINDOW_ICON_PNG).unwrap_or_default(),
    )
});

const fn main_window() -> WindowId {
    WindowId::MAIN
}

impl eframe::App for App {
    /// Called by the frame work to save state before shutdown.
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.open_tabs = self.tabs.open_paths();
        self.windows = self.tabs.save();
        eframe::set_value(storage, eframe::APP_KEY, self);
    }

    #[allow(unused_variables)]
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        #[cfg(feature = "profiling")]
        {
            puffin::profile_function!("my_update");
            self.frame_tracker.begin_frame();
            if let Some(render_state) = frame.wgpu_render_state() {
                self.frame_tracker.report_gpu_status(&render_state.device);
            }
        }
        self.assets
            .set_selected_previews_enabled(self.settings.animate_selected_previews);
        self.assets.begin_frame();
        self.assets.poll_results(&ctx);
        self.drain_command_queue(&ctx);
        self.poll_drives(&ctx);
        self.poll_plugins();
        self.process_file_system_changes(&ctx);
        self.sync_open_history(&ctx);
        self.forget_stale_cut();
        self.commit_paused_typing(&ctx);
        self.tabs.set_current_window(self.focused_window);
        let active_directory = self.tabs.get_current_path();
        self.assets
            .set_active_directory(active_directory.as_deref());

        self.tabs.set_current_window(WindowId::MAIN);
        self.render_window(ui);
        // Closing the main window while others are open keeps the app
        // running: the main window takes over another window's tabs.
        if ctx.input(|i| i.viewport().close_requested()) && self.tabs.adopt_into_main() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        #[cfg(windows)]
        self.chrome.sync(&ctx, frame);

        #[cfg(feature = "profiling")]
        crate::profiler::profiler_window(&ctx, &mut self.profiler_visible);

        self.render_other_windows(&ctx);
        self.drain_command_queue(&ctx);
        for tab in self.tabs.take_retired() {
            tab.release(&mut self.watchers, &ctx);
        }
        // Defensive: keep the UI ticking while any tab is loading
        if self.tabs.has_loading() {
            #[cfg(feature = "profiling")]
            puffin::profile_scope!("lwa_fm::repaint::loading_tab");
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        self.assets.end_frame();
        #[cfg(feature = "profiling")]
        self.frame_tracker.end_frame();
    }
}

/// Tabs to open at startup: a folder given on the command line, otherwise
/// the windows and tabs that were open when the app last closed.
fn restore_tabs(
    windows: Vec<crate::app::dock::SavedWindow>,
    saved: &[PathBuf],
) -> crate::app::dock::Workspace {
    let from_args = std::env::args().nth(1).is_some();
    if !from_args && let Some(workspace) = crate::app::dock::Workspace::restore(windows) {
        return workspace;
    }
    let saved: Vec<&PathBuf> = saved.iter().filter(|path| path.is_dir()).collect();
    match saved.split_first() {
        Some((first, rest)) if !from_args => {
            let mut tabs = crate::app::dock::Workspace::new(first);
            for path in rest {
                tabs.open_in_new_tab(path);
            }
            tabs.select_tab(1);
            tabs
        }
        _ => crate::app::dock::Workspace::new(&get_starting_path()),
    }
}

fn get_starting_path() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        return PathBuf::from(args[1].clone());
    }
    let cwd = std::env::current_dir().expect("Could not get current_dir");
    // Apps started from Finder or the Dock run in `/`; start at home instead.
    if cwd.parent().is_none()
        && let Some(dirs) = directories::UserDirs::new()
    {
        return dirs.home_dir().to_path_buf();
    }
    cwd
}
