use crate::app::assets::AssetManager;
use crate::app::commands::{COMMANDS_QUEUE, TabAction, TabTarget};
use crate::app::directory_path_info::DirectoryPathInfo;
use crate::app::directory_view_settings::DirectoryViewSettings;
use crate::app::dock::CurrentPath;
use crate::data::files::{DirEntry, DirList};
use crate::helper::{DataHolder, KeyWithCommandPressed};
use crate::locations::Locations;
use crate::watcher::DirectoryWatchers;
use crate::{app::settings::ApplicationSettings, locations::Location};
use command_palette::CommandPalette;
use commands::{ActionToPerform, ModalWindow};
use egui::TextBuffer;
use mlua::Lua;
use rayon::ThreadPoolBuilder;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use std::fs;

pub mod assets;
mod central_panel;
pub mod command_palette;
pub mod commands;
pub mod database;
pub mod dir_handling;
pub mod directory_path_info;
mod directory_view_settings;
pub mod dock;
mod settings;
mod side_panel;
mod top_bottom;

/// Dedicated thread pool for filesystem reads. Limited to 2 threads to bound
/// concurrent disk I/O while keeping the UI responsive.
static BG_POOL: LazyLock<rayon::ThreadPool> = LazyLock::new(|| {
    ThreadPoolBuilder::new()
        .num_threads(2)
        .thread_name(|i| format!("lwa_fm_bg_{i}"))
        .build()
        .expect("Failed to create background thread pool")
});

thread_local! {
    pub static LUA_INSTANCE: RefCell<Lua> = RefCell::new({
        Lua::new()
    });
}

pub static TOASTS: std::sync::LazyLock<egui::mutex::RwLock<egui_notify::Toasts>> =
    std::sync::LazyLock::new(|| {
        egui::mutex::RwLock::new(
            egui_notify::Toasts::new().with_anchor(egui_notify::Anchor::TopRight),
        )
    });
#[macro_export]
macro_rules! toast{
        (Basic, $($format:expr),+) => {
            $crate::app::TOASTS.write().basic(format!($($format),+));
        };
        (Info, $($format:expr),+) => {
            $crate::app::TOASTS.write().info(format!($($format),+));
        };
        (Warning, $($format:expr),+) => {
            $crate::app::TOASTS.write().warning(format!($($format),+));
        };
        (Error, $($format:expr),+) => {
            $crate::app::TOASTS.write().error(format!($($format),+));
        };
        (Success, $($format:expr),+) => {
            $crate::app::TOASTS.write().success(format!($($format),+));
        };
    }

#[derive(Deserialize, Serialize)]
#[serde(default)] // if we add new fields, give them default values when deserializing old state
pub struct App {
    user_locations: Locations,
    #[cfg(not(target_os = "macos"))]
    drives_locations: Locations,
    #[serde(skip)]
    tabs: crate::app::dock::MyTabs,
    pub settings: ApplicationSettings,
    #[serde(skip)]
    display_modal: Option<ModalWindow>,
    #[serde(skip)]
    command_palette: CommandPalette,
    #[serde(skip, default)]
    pub watchers: DirectoryWatchers,
    #[serde(skip, default)]
    assets: AssetManager,
    #[serde(skip, default)]
    pending_modified_files: BTreeMap<PathBuf, Instant>,
    #[cfg(feature = "profiling")]
    #[serde(skip)]
    profiler_visible: bool,
    #[cfg(feature = "profiling")]
    #[serde(skip)]
    frame_tracker: crate::profiler::FrameTracker,
}

impl App {
    fn reconcile_tab_watchers(&mut self, tab_id: u32) {
        let (old_watcher_specs, new_watcher_specs) = {
            let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                return;
            };
            let old_watcher_specs = tab.active_watch_specs.clone();
            let new_watcher_specs = tab.watcher_specs();
            tab.active_watch_specs = new_watcher_specs.clone();
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
        self.watchers.check_for_new_watchers();
        const MODIFIED_FILE_COALESCE: Duration = Duration::from_millis(150);

        let changes = self.watchers.check_for_file_system_events();
        let now = Instant::now();
        for file in changes.modified_files {
            self.pending_modified_files.entry(file).or_insert(now);
        }
        let ready_modified_files = self
            .pending_modified_files
            .iter()
            .filter_map(|(path, first_seen)| {
                (now.duration_since(*first_seen) >= MODIFIED_FILE_COALESCE).then(|| path.clone())
            })
            .collect::<Vec<_>>();
        for file in &ready_modified_files {
            self.pending_modified_files.remove(file);
        }

        if changes.structural_dirs.is_empty() && ready_modified_files.is_empty() {
            return;
        }

        self.assets.invalidate_files(ready_modified_files.iter().cloned());
        for file in &ready_modified_files {
            crate::app::database::update_file_metadata(file);
        }
        self.assets
            .invalidate_directories(changes.structural_dirs.iter().cloned());
        crate::app::database::invalidate_dirs(changes.structural_dirs.iter().cloned());

        let tab_ids = self.tabs.get_tab_ids();
        let affected_tabs = tab_ids
            .iter()
            .copied()
            .filter(|tab_id| {
                self.tabs
                    .get_tab_by_id(*tab_id)
                    .is_some_and(|tab| tab.should_refresh_for_directories(&changes.structural_dirs))
            })
            .collect::<Vec<_>>();

        for tab_id in &tab_ids {
            if affected_tabs.contains(tab_id) {
                continue;
            }
            let Some(tab) = self.tabs.get_tab_by_id(*tab_id) else {
                continue;
            };
            let settings = ctx.data_get_path_or_persisted::<DirectoryViewSettings>(&tab.current_path);
            for file in &ready_modified_files {
                tab.update_file_metadata(file, &settings.data);
            }
        }

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

    fn drain_command_queue(&mut self, ctx: &egui::Context) {
        while let Some(action) = COMMANDS_QUEUE.pop() {
            self.handle_action(ctx, action);
        }
    }
}

#[derive(Deserialize, Serialize, Default, PartialEq, Eq, Debug, Clone, Copy)]
pub enum Sort {
    #[default]
    Name,
    Modified,
    Created,
    Size,
    Random,
}

#[derive(Deserialize, Serialize, Default, PartialEq, Eq, Debug, Clone, Copy)]
pub enum DisplayType {
    #[default]
    List,
    Icons,
}

impl DisplayType {
    /// returns if it is a list
    pub const fn is_list(&self) -> bool {
        matches!(self, Self::List)
    }
}

#[derive(Deserialize, Serialize, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchTermType {
    #[default]
    Plain,
    Glob,
    Regex,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchMode {
    #[default]
    All,
    Any,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct SearchTerm {
    pub pattern: String,
    pub term_type: SearchTermType,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct SavedSearch {
    pub name: String,
    pub search: Search,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
pub struct SavedSearches {
    pub searches: Vec<SavedSearch>,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
pub struct Search {
    pub value: String,
    pub depth: usize,
    pub case_sensitive: bool,
    #[serde(default)]
    pub term_type: SearchTermType,
    #[serde(default)]
    pub extra_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub terms: Vec<SearchTerm>,
    #[serde(default)]
    pub match_mode: MatchMode,
    #[serde(skip)]
    pub new_dir_input: String,
    #[serde(skip)]
    pub save_name_input: String,
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
        #[cfg(not(target_os = "macos"))]
        let drives_locations = Locations::get_drives();

        let command_palette = CommandPalette::default();
        Self {
            #[cfg(not(target_os = "macos"))]
            drives_locations,
            user_locations: Locations::get_user_dirs(),
            tabs: crate::app::dock::MyTabs::new(&get_starting_path()),
            settings: ApplicationSettings::default(),
            display_modal: None,
            command_palette,
            watchers: DirectoryWatchers::default(),
            assets: AssetManager::default(),
            pending_modified_files: BTreeMap::new(),
            #[cfg(feature = "profiling")]
            profiler_visible: true,
            #[cfg(feature = "profiling")]
            frame_tracker: crate::profiler::FrameTracker::new(),
        }
    }
}

impl App {
    fn load_locations(&mut self) {
        #[cfg(not(target_os = "macos"))]
        {
            self.drives_locations = Locations::get_drives();
        }
        self.user_locations = Locations::get_user_dirs();
    }
    /// Called once before the first frame.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_custom_fonts(&cc.egui_ctx);

        #[cfg(debug_assertions)]
        cc.egui_ctx.style_mut(|style| {
            style.debug.show_unaligned = false;
        });

        // Load previous app state (if any).
        // Note that you must enable the `persistence` feature for this to work.
        if let Some(storage) = cc.storage {
            let mut value: Self = eframe::get_value(storage, eframe::APP_KEY).unwrap_or_default();

            value.load_locations();
            value.tabs = crate::app::dock::MyTabs::new(&get_starting_path());
            value.assets = AssetManager::default();
            value.assets.set_icon_size(value.settings.icon_size);
            #[cfg(feature = "profiling")]
            {
                value.profiler_visible = true;
            }
            return value;
        }

        Self::default()
    }

    #[allow(clippy::too_many_lines)]
    fn handle_action(&mut self, ctx: &egui::Context, action: ActionToPerform) {
        #[cfg(feature = "profiling")]
        puffin::profile_function!("lwa_fm::handle_action");

        match action {
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
                        let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                            return;
                        };
                        path.print_from_lua();
                        tab.set_path(path);
                        tab.refresh_generation
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if let Some(data) = ctx.data_get_tab::<DirectoryPathInfo>(tab.id) {
                            let new_data = match tab.current_path.single_path() {
                                Some(p) => {
                                    if Path::new(&data.text_input).eq(p.as_path()) {
                                        Some(DirectoryPathInfo::build(p.as_path(), false))
                                    } else {
                                        None
                                    }
                                }
                                None => None,
                            };
                            match new_data {
                                Some(s) => ctx.data_set_tab(tab.id, s),
                                None => ctx.data_remove_tab::<DirectoryPathInfo>(tab.id),
                            }
                        }
                        self.handle_action(
                            ctx,
                            ActionToPerform::TabAction(
                                TabTarget::TabWithId(tab_id),
                                TabAction::RequestFilesRefresh,
                            ),
                        );
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
                            puffin::profile_scope!("lwa_fm::handle_action::ForceRefresh::collect_dirs");
                            let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                                return;
                            };
                            let mut dirs: Vec<PathBuf> = match &tab.current_path {
                                CurrentPath::None => vec![],
                                CurrentPath::One(p) => vec![p.clone()],
                                CurrentPath::Multiple(ps) => ps.clone(),
                            };
                            if let Some(search) = &tab.search {
                                dirs.extend(search.extra_dirs.iter().cloned());
                                dirs.sort();
                                dirs.dedup();
                            }
                            dirs
                        };
                        crate::app::database::invalidate_dirs(force_dirs.into_iter());
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
                        self.reconcile_tab_watchers(tab_id);
                        let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                            return;
                        };
                        tab.refresh_generation
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        tab.cancel_token
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                        if tab.loading {
                            tab.pending_refresh = true;
                            return;
                        }
                        tab.update_settings(ctx);

                        let mut directories: Vec<PathBuf> = match &tab.current_path {
                            CurrentPath::None => vec![],
                            CurrentPath::One(path_buf) => vec![path_buf.clone()],
                            CurrentPath::Multiple(path_bufs) => path_bufs.clone(),
                        };
                        if let Some(search) = &tab.search {
                            directories.extend(search.extra_dirs.iter().cloned());
                            directories.sort();
                            directories.dedup();
                        }
                        let depth = tab.search_depth();
                        let show_hidden = tab.show_hidden;
                        let current_path = tab.current_path.clone();
                        let search = tab.search.clone();

                        let settings =
                            ctx.data_get_path_or_persisted::<DirectoryViewSettings>(&current_path);

                        tab.loading = true;
                        tab.loading_progress = None;
                        tab.cancel_token
                            .store(false, std::sync::atomic::Ordering::SeqCst);
                        let refresh_gen = std::sync::Arc::clone(&tab.refresh_generation);
                        let generation = refresh_gen.load(std::sync::atomic::Ordering::SeqCst);
                        let cancel = std::sync::Arc::clone(&tab.cancel_token);

                        BG_POOL.spawn(move || {
                            #[cfg(feature = "profiling")]
                            puffin::profile_scope!("lwa_fm::handle_action::RefreshFiles::bg_thread");
                            if refresh_gen.load(std::sync::atomic::Ordering::SeqCst) != generation {
                                return;
                            }
                            if cancel.load(std::sync::atomic::Ordering::SeqCst) {
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

                            let mut list = crate::app::dir_handling::read_directory(
                                &directories,
                                depth,
                                show_hidden,
                                &cancel,
                            );
                            if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                                return;
                            }

                            progress(&format!("Sorting {} entries…", list.len()));

                            crate::app::dir_handling::sort_entries_vec(&mut list, &settings.data);
                            #[cfg(feature = "profiling")]
                            puffin::profile_scope!("lwa_fm::handle_action::RefreshFiles::filter_visible");
                            let visible = crate::app::dir_handling::filter_visible_entries(
                                &list,
                                show_hidden,
                                search.as_ref(),
                            );
                            // For single-directory reads, move `list` into a lazily-materialised
                            // DirList so the tab's `list` can stay empty (full DirEntry values
                            // are then built only for visible rows, roughly halving memory for
                            // large folders). Multi-dir / search reads keep `list` populated.
                            let (list, dir_list) = if depth <= 1 && directories.len() == 1 {
                                (Vec::new(), crate::data::files::DirList::from_owned_list(list))
                            } else {
                                (list, None)
                            };
                            COMMANDS_QUEUE.push(ActionToPerform::TabAction(
                                TabTarget::TabWithId(tab_id),
                                TabAction::FilesLoaded {
                                    list,
                                    generation,
                                    visible,
                                    dir_list,
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
                        list,
                        generation,
                        visible,
                        dir_list,
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
                                tab.list = list;
                                tab.visible_entries = visible;
                                tab.dir_list = dir_list;
                            }
                            tab.loading = false;
                            tab.loading_progress = None;
                            if tab.pending_refresh {
                                tab.pending_refresh = false;
                                TabAction::RequestFilesRefresh.schedule_tab(tab_id);
                            }
                            return;
                        }
                        tab.update_settings(ctx);
                        tab.list = list;
                        tab.visible_entries = visible;
                        tab.dir_list = dir_list;
                        tab.loading = false;
                        tab.loading_progress = None;
                        if tab.pending_refresh {
                            tab.pending_refresh = false;
                            TabAction::RequestFilesRefresh.schedule_tab(tab_id);
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
                        tab.loading_progress = Some(progress);
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
                            if !tab.can_undo() {
                                return;
                            }

                            let Some(previous_path_action) = tab.undo() else {
                                return;
                            };
                            self.handle_action(ctx, previous_path_action);
                        }
                    }
                    commands::TabAction::AddSearchDir(path) => {
                        let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                            return;
                        };
                        if let Some(search) = &mut tab.search
                            && path.is_dir()
                            && !search.extra_dirs.contains(&path)
                        {
                            search.extra_dirs.push(path);
                        }
                        tab.update_visible_entries();
                        TabAction::RequestFilesRefresh.schedule_tab(tab_id);
                    }
                    commands::TabAction::RemoveSearchDir(index) => {
                        let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                            return;
                        };
                        if let Some(search) = &mut tab.search
                            && index < search.extra_dirs.len()
                        {
                            search.extra_dirs.remove(index);
                        }
                        tab.update_visible_entries();
                        TabAction::RequestFilesRefresh.schedule_tab(tab_id);
                    }
                    commands::TabAction::AddSearchTerm(term) => {
                        let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                            return;
                        };
                        if let Some(search) = &mut tab.search {
                            search.terms.push(term);
                        }
                        TabAction::FilterChanged.schedule_tab(tab_id);
                    }
                    commands::TabAction::RemoveSearchTerm(index) => {
                        let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                            return;
                        };
                        if let Some(search) = &mut tab.search
                            && index < search.terms.len()
                        {
                            search.terms.remove(index);
                        }
                        TabAction::FilterChanged.schedule_tab(tab_id);
                    }
                    commands::TabAction::SetMatchMode(mode) => {
                        let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                            return;
                        };
                        if let Some(search) = &mut tab.search {
                            search.match_mode = mode;
                        }
                        TabAction::FilterChanged.schedule_tab(tab_id);
                    }
                    commands::TabAction::SaveSearch(name) => {
                        let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                            return;
                        };
                        if let Some(search) = &tab.search {
                            let mut saved = ctx
                                .data_get_persisted::<SavedSearches>()
                                .unwrap_or_default();
                            saved.searches.retain(|s| s.name != name);
                            saved.searches.push(SavedSearch {
                                name: name.clone(),
                                search: search.clone(),
                            });
                            ctx.data_set_persisted(saved);
                            toast!(Success, "Saved search: {name}");
                        }
                    }
                    commands::TabAction::LoadSavedSearch(name) => {
                        let saved = ctx
                            .data_get_persisted::<SavedSearches>()
                            .unwrap_or_default();
                        if let Some(ss) = saved.searches.iter().find(|s| s.name == name) {
                            let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
                                return;
                            };
                            tab.search = Some(ss.search.clone());
                            tab.update_visible_entries();
                            TabAction::RequestFilesRefresh.schedule_tab(tab_id);
                        }
                    }
                    commands::TabAction::DeleteSavedSearch(name) => {
                        let mut saved = ctx
                            .data_get_persisted::<SavedSearches>()
                            .unwrap_or_default();
                        saved.searches.retain(|s| s.name != name);
                        ctx.data_set_persisted(saved);
                    }
                }
            }
            ActionToPerform::NewTab(path) => self.tabs.open_in_new_tab(&path),
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
            ActionToPerform::ViewSettingsChanged(_) => {
                TabAction::FilesSort.schedule_active_tab();
                let Some(active_tab) = self.tabs.get_current_tab() else {
                    return;
                };
                let active_path = active_tab.current_path.clone();
                for tab_id in self.tabs.get_tab_ids() {
                    if let Some(tab) = self.tabs.get_tab_by_id(tab_id)
                        && tab.current_path == active_path
                    {
                        TabAction::FilesSort.schedule_tab(tab_id);
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
                let current_path = self.tabs.get_current_path();
                let index = self.tabs.get_current_index().unwrap_or_default();

                match ctx.data_get_tab::<DirectoryPathInfo>(index) {
                    Some(_) => ctx.data_remove_tab::<DirectoryPathInfo>(index),
                    None => {
                        if let Some(path) = current_path {
                            ctx.data_set_tab(
                                index,
                                DirectoryPathInfo::build(path.as_path(), false),
                            );
                        }
                    }
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
                let _ = open::that_detached(cow.as_str());
            }
        }
    }
}

impl eframe::App for App {
    /// Called by the frame work to save state before shutdown.
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
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
        self.assets.poll_results(&ctx);
        self.drain_command_queue(&ctx);
        self.process_file_system_changes(&ctx);
        let active_directory = self.tabs.get_current_path();
        self.assets
            .set_active_directory(active_directory.as_deref());
        self.top_panel(&ctx);
        self.bottom_panel(&ctx);
        self.left_side_panel(&ctx);
        self.central_panel(&ctx);

        if ctx.key_with_command_pressed(egui::Key::P) {
            ActionToPerform::ToggleModalWindow(ModalWindow::Settings).schedule();
        }

        if ctx.key_with_command_pressed(egui::Key::L) {
            ActionToPerform::ToggleTopEdit.schedule();
        }
        if let Some(current_path) = self.tabs.get_current_path() {
            let favorites = ctx.data_get_persisted::<Locations>().unwrap_or_default();
            if ctx.key_with_command_pressed(egui::Key::R) {
                ActionToPerform::ToggleModalWindow(ModalWindow::Commands).schedule();
                self.command_palette.build_for_path(
                    &CurrentPath::One(current_path.clone()),
                    &current_path,
                    &favorites,
                );
            }
        }
        // F5: Force refresh the active tab (bypass all caches)
        if ctx.input(|i| i.key_pressed(egui::Key::F5)) {
            toast!(Info, "Force refreshing...");
            TabAction::ForceRefresh.schedule_active_tab();
        }

        if let Some(modal) = &self.display_modal {
            match modal {
                ModalWindow::Settings => {
                    self.settings.display(&ctx);
                }
                ModalWindow::Commands => {
                    self.command_palette.ui(&ctx);
                } // ModalWindow::NewDirectory => todo!(),
                ModalWindow::Rename => {
                    let modal_response =
                        egui::Modal::new(egui::Id::new(ModalWindow::Rename)).show(&ctx, |ui| {
                            ui.label("Old name");
                            let (old, mut name) = ui.data_mut(|d| {
                                let old =
                                    d.get_temp::<DirEntry>(egui::Id::new(ModalWindow::Rename));
                                let new = d
                                    .get_temp::<String>(
                                        egui::Id::new(ModalWindow::Rename).with("new"),
                                    )
                                    .unwrap_or_else(|| {
                                        old.as_ref()
                                            .map(|d| d.get_splitted_path().1.to_string())
                                            .unwrap_or_default()
                                    });
                                (old, new)
                            });
                            let Some(old) = old else {
                                return;
                            };
                            let mut old_file_name = old.get_splitted_path().1.to_string();
                            ui.add_enabled(false, egui::TextEdit::singleline(&mut old_file_name));
                            ui.label("New name");
                            ui.text_edit_singleline(&mut name);
                            let valid = !Path::new(&name).try_exists().is_ok_and(|f| f);
                            if ui.add_enabled(valid, egui::Button::new("Rename")).clicked() {
                                if fs::rename(
                                    old.get_path(),
                                    Path::new(old.get_splitted_path().0).join(&name),
                                )
                                .is_ok()
                                {
                                    crate::app::database::invalidate_dir(Path::new(
                                        old.get_splitted_path().0,
                                    ));
                                    TabAction::RequestFilesRefresh.schedule_active_tab();
                                }
                                ui.data_mut(|w| {
                                    w.remove_temp::<String>(
                                        egui::Id::new(ModalWindow::Rename).with("new"),
                                    )
                                });
                                ui.close();
                            } else {
                                ui.data_mut(|w| {
                                    w.insert_temp(
                                        egui::Id::new(ModalWindow::Rename).with("new"),
                                        name.clone(),
                                    );
                                });
                            }
                        });

                    if modal_response.should_close() {
                        ActionToPerform::CloseActiveModalWindow.schedule();
                    }
                }
            }
        }

        #[cfg(feature = "profiling")]
        {
            if ctx.input(|i| i.key_pressed(egui::Key::F2)) {
                self.profiler_visible = !self.profiler_visible;
            }
            crate::profiler::profiler_window(&ctx, &mut self.profiler_visible);
        }

        TOASTS.write().show(&ctx);
        self.drain_command_queue(&ctx);
        if self.watchers.is_active() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        // Defensive: keep the UI ticking while any tab is loading
        if self.tabs.has_loading() {
            #[cfg(feature = "profiling")]
            puffin::profile_scope!("lwa_fm::repaint::loading_tab");
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        #[cfg(feature = "profiling")]
        self.frame_tracker.end_frame();
    }
}

fn setup_custom_fonts(ctx: &egui::Context) {
    // Start with the default fonts (we will be adding to them rather than replacing them).
    let mut fonts = egui::FontDefinitions::default();
    if let Ok((regular, semibold)) = get_fonts() {
        fonts.font_data.insert(
            "regular".to_owned(),
            egui::FontData::from_owned(regular).into(),
        );
        fonts.font_data.insert(
            "semibold".to_owned(),
            egui::FontData::from_owned(semibold).into(),
        );

        // Put my font first (highest priority) for proportional text:
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "regular".to_owned());
        fonts
            .families
            .entry(egui::FontFamily::Name("semibold".into()))
            .or_default()
            .insert(0, "semibold".to_owned());

        // Put my font as last fallback for monospace:
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .push("regular".to_owned());

        // Tell egui to use these fonts:
        ctx.set_fonts(fonts);
    }

    ctx.all_styles_mut(|style| {
        for font_id in style.text_styles.values_mut() {
            font_id.size *= 1.4;
        }
    });
}

#[cfg(not(windows))]
fn get_fonts() -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let font_path = std::path::Path::new("/System/Library/Fonts");

    let regular = fs::read(font_path.join("SFNSRounded.ttf"))?;
    let semibold = fs::read(font_path.join("SFCompact.ttf"))?;

    Ok((regular, semibold))
}

#[cfg(windows)]
fn get_fonts() -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let app_data = std::env::var("APPDATA")?;
    let font_path = std::path::Path::new(&app_data);

    let regular = fs::read(font_path.join("../Local/Microsoft/Windows/Fonts/aptos.ttf"))?;
    let semibold = fs::read(font_path.join("../Local/Microsoft/Windows/Fonts/aptos-semibold.ttf"))?;

    Ok((regular, semibold))
}

fn get_starting_path() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        return PathBuf::from(args[1].clone());
    }
    std::env::current_dir().expect("Could not get current_dir")
}
