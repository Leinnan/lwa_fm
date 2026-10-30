use egui::text::{LayoutJob, TextWrapping};
use egui::{Color32, Context, FontId, FontSelection, Galley, Id, TextFormat, Ui, WidgetText};
use egui_dock::TabViewer;
use icu_collator::options::{AlternateHandling, CollatorOptions, Strength};
use icu_collator::{Collator, CollatorBorrowed};
use icu_locale_core::Locale;
use lru::LruCache;
use serde::{Deserialize, Serialize};
use std::num::NonZeroUsize;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};

use super::assets::{AssetManager, PreviewIntent};
use super::commands::ActionToPerform;
use crate::app::commands::TabAction;
use crate::app::dir_handling::WatcherSpecsCache;
use crate::app::history::History;
use crate::app::selection::Selection;
use crate::app::top_bottom::TopDisplayPath;
use crate::app::{DisplayType, Search};
use crate::data::files::{EntryRef, Listing};
use crate::data::time::ElapsedTime;
use crate::helper::DataHolder;
use crate::locations::Locations;
use std::cell::RefCell;

mod columns;
mod entry_actions;
mod grid_view;
mod interaction;
mod list_view;
mod quick_look;
mod text_fit;
mod workspace;

pub use columns::kind_label;
#[cfg(test)]
pub use columns::{CELL_PADDING, Column, ListColumns};
pub use workspace::{SavedWindow, WindowId, Workspace};

thread_local! {
    pub static TIME_POOL: RefCell<LruCache<ElapsedTime, Arc<Galley>>> =
        RefCell::new(LruCache::new(NonZeroUsize::new(2048).expect("capacity must be > 0")));
}

pub fn populate_time_pool(components: impl Iterator<Item = ElapsedTime>, ui: &Context) {
    TIME_POOL.with_borrow_mut(|pool| {
        for component in components {
            if pool.peek(&component).is_some() {
                continue;
            }
            let galley = WidgetText::LayoutJob(Arc::new(LayoutJob::simple_singleline(
                component.to_string(),
                FontId::default(),
                ui.global_style().visuals.weak_text_color(),
            )))
            .into_galley_impl(
                ui,
                &ui.global_style(),
                TextWrapping::default(),
                FontSelection::Default,
                egui::Align::Center,
            );
            pool.put(component, galley);
        }
    });
}

thread_local! {
    pub static SIZES_POOL: RefCell<LruCache<u64, Arc<Galley>>> =
        RefCell::new(LruCache::new(NonZeroUsize::new(2048).expect("capacity must be > 0")));
}
thread_local! {
    /// Pre-rendered galleys for file entry names, keyed by the file name string.
    /// Keying by `String` (rather than `(String, bool)`) lets the per-row lookup
    /// borrow the entry's `&str` name via `Borrow<str>` without allocating a
    /// throwaway `String` key every frame. Directories use a separate cache
    /// because the galley colour is baked in and differs between files/folders.
    pub static FILE_NAME_POOL: RefCell<LruCache<String, Arc<Galley>>> =
        RefCell::new(LruCache::new(NonZeroUsize::new(2048).expect("capacity must be > 0")));
    pub static FOLDER_NAME_POOL: RefCell<LruCache<String, Arc<Galley>>> =
        RefCell::new(LruCache::new(NonZeroUsize::new(2048).expect("capacity must be > 0")));
    static TEXT_CACHE_STYLE: RefCell<Option<TextCacheStyle>> = const { RefCell::new(None) };
}

#[cfg(test)]
static FILE_NAME_LAYOUT_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[derive(PartialEq)]
struct TextCacheStyle {
    context_id: u64,
    pixels_per_point: u32,
    font: FontId,
    text: Color32,
    secondary: Color32,
    options: egui::epaint::text::TextOptions,
}

fn ensure_text_cache_style(ui: &Ui) {
    static NEXT_CONTEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let context_id = ui.ctx().data_mut(|data| {
        *data.get_temp_mut_or_insert_with(egui::Id::new("text_cache_context"), || {
            NEXT_CONTEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        })
    });
    // Appearance changes rebuild egui's font atlas on the following pass.
    // Include the active atlas options, rather than only the requested theme.
    let key = TextCacheStyle {
        context_id,
        pixels_per_point: ui.ctx().pixels_per_point().to_bits(),
        font: egui::TextStyle::Body.resolve(ui.style()),
        text: ui.visuals().text_color(),
        secondary: ui.visuals().weak_text_color(),
        options: ui.fonts(|fonts| *fonts.options()),
    };
    let changed = TEXT_CACHE_STYLE.with_borrow_mut(|current| {
        if current.as_ref() == Some(&key) {
            false
        } else {
            *current = Some(key);
            true
        }
    });
    if changed {
        FILE_NAME_POOL.with_borrow_mut(LruCache::clear);
        FOLDER_NAME_POOL.with_borrow_mut(LruCache::clear);
        TIME_POOL.with_borrow_mut(LruCache::clear);
        SIZES_POOL.with_borrow_mut(LruCache::clear);
        text_fit::clear_pool();
    }
}

/// A file name with the searched words highlighted; `None` when nothing in
/// the name matches (then the cached plain galley is used).
fn highlighted_name_galley(
    ui: &Ui,
    name: &str,
    highlights: &crate::app::search::filter::Highlights,
) -> Option<Arc<Galley>> {
    let ranges = highlights.ranges(name);
    if ranges.is_empty() {
        return None;
    }
    let plain = TextFormat::simple(FontId::default(), ui.visuals().text_color());
    let marked = TextFormat {
        background: crate::app::ui::Palette::of(ui).accent.gamma_multiply(0.45),
        ..plain.clone()
    };
    let mut job = LayoutJob::default();
    let mut at = 0;
    for range in ranges {
        job.append(&name[at..range.start], 0.0, plain.clone());
        job.append(&name[range.clone()], 0.0, marked.clone());
        at = range.end;
    }
    job.append(&name[at..], 0.0, plain);
    Some(ui.fonts_mut(|fonts| fonts.layout_job(job)))
}

fn file_name_galley(ui: &Ui, name: &str, is_dir: bool) -> Arc<Galley> {
    let color = ui.visuals().text_color();
    let make = || {
        #[cfg(test)]
        FILE_NAME_LAYOUT_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        WidgetText::LayoutJob(Arc::new(LayoutJob::simple_singleline(
            name.to_owned(),
            FontId::default(),
            color,
        )))
        .into_galley_impl(
            ui.ctx(),
            ui.style(),
            TextWrapping::default(),
            FontSelection::Default,
            egui::Align::Center,
        )
    };
    if is_dir {
        FOLDER_NAME_POOL.with_borrow_mut(|pool| {
            if let Some(galley) = pool.get(name) {
                return galley.clone();
            }
            let galley = make();
            pool.put(name.to_owned(), galley.clone());
            galley
        })
    } else {
        FILE_NAME_POOL.with_borrow_mut(|pool| {
            if let Some(galley) = pool.get(name) {
                return galley.clone();
            }
            let galley = make();
            pool.put(name.to_owned(), galley.clone());
            galley
        })
    }
}

pub fn populate_sizes_pool(components: impl Iterator<Item = u64>, ui: &Context) {
    SIZES_POOL.with_borrow_mut(|pool| {
        for component in components {
            if pool.peek(&component).is_some() {
                continue;
            }
            let galley = WidgetText::LayoutJob(Arc::new(LayoutJob::simple_singleline(
                crate::helper::format_bytes_simple(component),
                FontId::default(),
                ui.global_style().visuals.weak_text_color(),
            )))
            .into_galley_impl(
                ui,
                &ui.global_style(),
                TextWrapping::default(),
                FontSelection::Default,
                egui::Align::Center,
            );
            pool.put(component, galley);
        }
    });
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Default, Hash)]
pub enum CurrentPath {
    #[default]
    None,
    One(PathBuf),
    Multiple(Vec<PathBuf>),
    /// The Recycle Bin / Trash, listed by `fileops::trash_bin`.
    Trash,
}

impl CurrentPath {
    /// Every folder shown: one, several (favorites search) or none.
    pub fn get_paths(&self) -> Vec<PathBuf> {
        match self {
            Self::None | Self::Trash => Vec::new(),
            Self::One(path) => vec![path.clone()],
            Self::Multiple(paths) => paths.clone(),
        }
    }

    pub fn get_path(&self) -> Option<PathBuf> {
        match self {
            Self::None | Self::Trash => None,
            Self::One(path) => Some(path.clone()),
            Self::Multiple(paths) => {
                let mut common_path = paths.first()?.clone();
                for path in paths.iter().skip(1) {
                    let mut common_components = Vec::new();
                    let common_iter = common_path.components().zip(path.components());

                    for (a, b) in common_iter {
                        if a == b {
                            common_components.push(a);
                        } else {
                            break;
                        }
                    }

                    common_path = common_components.into_iter().collect();
                }
                Some(common_path)
            }
        }
    }
}

impl From<PathBuf> for CurrentPath {
    fn from(path: PathBuf) -> Self {
        if path.is_dir() {
            Self::One(path)
        } else {
            Self::None
        }
    }
}

impl CurrentPath {
    pub fn get_name_from_path(&self) -> String {
        match &self {
            Self::None => "Empty".into(),
            Self::One(path) => {
                #[cfg(not(windows))]
                {
                    path.iter()
                        .next_back()
                        .expect("FAILED")
                        .to_string_lossy()
                        .into_owned()
                }
                #[cfg(windows)]
                {
                    let mut result = path
                        .iter()
                        .next_back()
                        .expect("FAILED")
                        .to_string_lossy()
                        .into_owned();
                    if result.len() == 1 {
                        result = path.display().to_string();
                    }
                    result
                }
            }
            Self::Multiple(_) => "Multiple".into(),
            Self::Trash => crate::fileops::trash_bin::LABEL.into(),
        }
    }
    pub fn parent(&self) -> Option<PathBuf> {
        match self {
            Self::One(path) => path.parent().map(PathBuf::from),
            _ => None,
        }
    }
    pub fn single_path(&self) -> Option<PathBuf> {
        match self {
            Self::One(path) => Some(path.clone()),
            _ => None,
        }
    }

    pub const fn multiple_paths(&self) -> bool {
        matches!(self, Self::Multiple(_))
    }

    /// Whether this is the Recycle Bin view, whose rows are not real files.
    pub const fn is_trash(&self) -> bool {
        matches!(self, Self::Trash)
    }
}

#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct TabData {
    /// Shared, immutable listing (also held by the directory cache).
    pub listing: Arc<Listing>,
    /// Display order: indices into `listing`.
    pub order: Vec<u32>,
    /// Rows that pass the current filter, in display order.
    pub visible_entries: Vec<u32>,
    /// Bumped whenever `order` changes, so filter narrowing can detect it.
    pub(crate) order_generation: u64,
    pub(crate) filter_key: Option<crate::app::dir_handling::FilterKey>,
    pub active_watch_specs: Vec<(PathBuf, notify::RecursiveMode)>,
    pub current_path: CurrentPath,
    pub show_hidden: bool,
    pub display_type: DisplayType,
    pub search: Option<Search>,
    /// The search options kept while the search bar is closed.
    pub(crate) closed_search: Option<Search>,
    /// The address bar is being edited: its text and suggestions.
    pub(crate) path_edit: Option<crate::app::directory_path_info::DirectoryPathInfo>,
    pub load: LoadState,
    pub(crate) refresh_generation: Arc<AtomicU64>,
    pub(crate) cancel_token: Arc<AtomicBool>,
    pub(crate) history: History,
    pub id: TabId,
    pub top_display_path: TopDisplayPath,
    /// Memoised `(path, RecursiveMode)` watcher specs plus the input snapshot
    /// that produced them. Recomputed only when `current_path` / search depth /
    /// `extra_dirs` change, so the per-frame `should_refresh_for_directories`
    /// path avoids `walkdir` + `canonicalize` syscalls during change bursts.
    pub(crate) watcher_specs_cache: WatcherSpecsCache,
    /// Directory symlinks followed by the last deep walk (extra watch roots).
    pub(crate) linked_watch_roots: crate::app::dir_handling::LinkedWatchRoots,
    /// Selected rows (positions in `visible_entries`), anchor and cursor.
    pub selection: Selection,
    /// What the selection referred to when last drawn; see [`SelectionSnapshot`].
    selection_snapshot: Option<SelectionSnapshot>,
    /// Items to select once a reload shows them, e.g. after "New folder".
    pub(crate) pending_select: Option<crate::app::file_ops::PendingSelect>,
    /// The search as last recorded for undo and history.
    pub(crate) search_committed: Option<Search>,
    /// When the search text last changed without being recorded yet.
    pub(crate) search_dirty_since: Option<std::time::Instant>,
    /// The next path change restores a history entry, so it keeps the search.
    pub(crate) keep_search_once: bool,
    /// Quick Look is showing a large preview of the focused item.
    pub(crate) quick_look: bool,
    /// Scroll and pointer state of the files view.
    pub(crate) view: ViewState,
    /// Restored but not read yet: the folder is read when the tab is first
    /// shown, so tabs left in the background never touch the disk (nor make
    /// macOS ask for folder access) at launch.
    pub(crate) unloaded: bool,
}

/// Whether a tab's listing is being read in the background.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum LoadState {
    #[default]
    Idle,
    Loading {
        /// What the reader is doing, once it has said.
        progress: Option<String>,
        /// Another refresh was asked for meanwhile: run it when this ends.
        rerun: bool,
    },
}

impl LoadState {
    pub const fn loading(progress: Option<String>) -> Self {
        Self::Loading {
            progress,
            rerun: false,
        }
    }
    pub const fn is_loading(&self) -> bool {
        matches!(self, Self::Loading { .. })
    }
    pub fn progress(&self) -> Option<&str> {
        match self {
            Self::Loading { progress, .. } => progress.as_deref(),
            Self::Idle => None,
        }
    }
    pub fn set_progress(&mut self, text: String) {
        if let Self::Loading { progress, .. } = self {
            *progress = Some(text);
        }
    }
    /// Asks for a refresh once the running load ends. False when idle,
    /// so the caller refreshes now.
    pub const fn defer_refresh(&mut self) -> bool {
        match self {
            Self::Loading { rerun, .. } => {
                *rerun = true;
                true
            }
            Self::Idle => false,
        }
    }
    /// Drops a refresh deferred by [`Self::defer_refresh`].
    pub const fn cancel_rerun(&mut self) {
        if let Self::Loading { rerun, .. } = self {
            *rerun = false;
        }
    }
    /// Ends the load. Returns whether a deferred refresh is waiting.
    pub fn finish(&mut self) -> bool {
        matches!(std::mem::take(self), Self::Loading { rerun: true, .. })
    }
}

/// Scroll and pointer state of a tab's files view, measured while drawing.
#[derive(Debug, Default)]
pub struct ViewState {
    /// Full rows that fit on screen, for Page Up and Page Down.
    pub page_rows: usize,
    /// The scroll offset when last drawn; saved in the history on leaving.
    pub scroll_y: f32,
    /// Height of the list's sticky header row when last drawn.
    pub header_height: f32,
    /// Distance between the tops of two rows when last drawn.
    pub row_pitch: f32,
    /// Empty space between two rows (part of `row_pitch`): 0 in the list.
    pub row_gap: f32,
    /// Height of the area rows scroll in, below any header.
    pub viewport_height: f32,
    /// Items per row: 1 in the list.
    pub columns: usize,
    /// Screen area the rows scroll in (below any header), when last drawn.
    pub rows_area: Option<egui::Rect>,
    /// Grid tiles when last drawn: left edge of the first column, tile
    /// size and the gaps between tiles. For hit-testing drag selection.
    pub tiles: Option<GridGeometry>,
    /// The folder shown when last drawn: a new one starts at the top.
    shown_path: Option<CurrentPath>,
    /// Where to scroll once the listing has loaded (Back / Forward).
    restore_scroll: Option<f32>,
}

/// Where the grid's tiles were drawn, in screen points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridGeometry {
    pub left: f32,
    pub tile: egui::Vec2,
    pub gap: egui::Vec2,
}

/// Where a view should scroll this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScrollRequest {
    /// Show this row, scrolling as little as possible.
    Cursor(usize),
    /// Scroll to this offset from the top.
    Offset(f32),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
struct PopupOpened(pub Id, pub usize);

impl Default for PopupOpened {
    fn default() -> Self {
        Self(Id::new(0), usize::MAX)
    }
}

/// Where egui memory keeps the tab's open row menu.
fn popup_memory_id(tab: TabId) -> Id {
    Id::new(("popup_opened", tab))
}

impl ViewState {
    /// Whether the whole of row `index` was on screen when last drawn.
    #[cfg(test)]
    pub fn shows(&self, index: usize) -> bool {
        let top = (index / self.columns.max(1)) as f32 * self.row_pitch;
        top >= self.scroll_y - 0.5
            && top + self.row_pitch - self.row_gap <= self.scroll_y + self.viewport_height + 0.5
    }
}

impl TabData {
    /// Selects the items an operation created once the listing shows them.
    fn apply_pending_select(&mut self) {
        let Some(pending) = &self.pending_select else {
            return;
        };
        if pending.since.elapsed() > std::time::Duration::from_secs(10) {
            self.pending_select = None;
            return;
        }
        let rows: Vec<usize> = self
            .visible_entries
            .iter()
            .enumerate()
            .filter(|&(_, &index)| {
                let entry = self.listing.entry(index as usize);
                let path = Path::new(entry.dir.as_ref()).join(entry.file_name);
                pending.paths.contains(&path)
            })
            .map(|(row, _)| row)
            .collect();
        let Some(&first) = rows.first() else {
            return;
        };
        let rename = pending.rename;
        self.pending_select = None;
        self.selection.select_only(first);
        self.selection.set_rows(rows);
        self.selection.just_changed = true;
        if rename && let Some(entry) = self.entry_at(first) {
            ActionToPerform::BeginRename(entry.get_path()).schedule();
        }
    }

    fn reconcile_selection(&mut self) {
        self.apply_pending_select();
        let Some(previous) = &self.selection_snapshot else {
            return;
        };
        if previous.directory != self.current_path {
            self.selection.clear();
            return;
        }
        let same_listing = Arc::ptr_eq(&previous.listing, &self.listing);
        if same_listing && previous.visible_hash == slice_hash(&self.visible_entries) {
            return;
        }
        if self.selection.is_empty() && previous.selected.is_empty() {
            return;
        }
        let rows: Vec<usize> = if same_listing {
            // Sorted or filtered: the same listing indices, new rows.
            let mut wanted = fixedbitset::FixedBitSet::with_capacity(self.listing.len());
            for &index in &previous.selected {
                wanted.insert(index as usize);
            }
            self.visible_entries
                .iter()
                .enumerate()
                .filter(|&(_, &index)| wanted.contains(index as usize))
                .map(|(row, _)| row)
                .collect()
        } else {
            // Reloaded: match the previously selected files by path.
            let wanted: foldhash::HashSet<(&str, &str)> = previous
                .selected
                .iter()
                .filter(|&&index| (index as usize) < previous.listing.len())
                .map(|&index| {
                    let entry = previous.listing.entry(index as usize);
                    (entry.dir.as_ref(), entry.file_name)
                })
                .collect();
            self.visible_entries
                .iter()
                .enumerate()
                .filter(|&(_, &index)| {
                    let entry = self.listing.entry(index as usize);
                    wanted.contains(&(entry.dir.as_ref(), entry.file_name))
                })
                .map(|(row, _)| row)
                .collect()
        };
        // The cursor follows its file too, even when it is not selected.
        let cursor_row = previous
            .cursor
            .filter(|_| same_listing)
            .and_then(|index| self.visible_entries.iter().position(|&i| i == index));
        self.selection.set_rows(rows);
        if let Some(row) = cursor_row {
            self.selection.set_cursor(row);
        }
        self.selection.truncate(self.visible_entries.len());
    }

    fn remember_selection(&mut self) {
        let visible_hash = slice_hash(&self.visible_entries);
        let selection_hash = {
            use std::hash::{BuildHasher, Hash, Hasher};
            let mut hasher = foldhash::fast::FixedState::default().build_hasher();
            for row in self.selection.rows() {
                row.hash(&mut hasher);
            }
            self.selection.cursor().hash(&mut hasher);
            hasher.finish()
        };
        if let Some(previous) = &self.selection_snapshot
            && previous.directory == self.current_path
            && Arc::ptr_eq(&previous.listing, &self.listing)
            && previous.visible_hash == visible_hash
            && previous.selection_hash == selection_hash
        {
            return;
        }
        let selected = self
            .selection
            .rows()
            .filter_map(|row| self.visible_entries.get(row).copied())
            .collect();
        let cursor = self
            .selection
            .cursor()
            .and_then(|row| self.visible_entries.get(row).copied());
        self.selection_snapshot = Some(SelectionSnapshot {
            directory: self.current_path.clone(),
            listing: Arc::clone(&self.listing),
            visible_hash,
            selection_hash,
            selected,
            cursor,
        });
    }

    /// Where you are in this folder: the scroll position and selection.
    pub(crate) fn current_place(&self) -> crate::app::history::Place {
        crate::app::history::Place {
            scroll_y: self.view.scroll_y,
            selected: self
                .selection
                .rows()
                .filter_map(|row| self.entry_at(row).map(|entry| entry.get_path()))
                .collect(),
        }
    }

    /// Takes where the view should scroll this frame: the top of a newly
    /// shown folder, a position restored by Back / Forward once loaded, or
    /// the cursor after it moved (`cursor_moved`).
    pub(crate) fn take_scroll_request(&mut self, cursor_moved: bool) -> Option<ScrollRequest> {
        if self.view.shown_path.as_ref() != Some(&self.current_path) {
            self.view.shown_path = Some(self.current_path.clone());
            if self.view.restore_scroll.is_none() {
                return Some(ScrollRequest::Offset(0.0));
            }
        }
        if !self.load.is_loading()
            && let Some(offset) = self.view.restore_scroll.take()
        {
            return Some(ScrollRequest::Offset(offset));
        }
        cursor_moved
            .then(|| self.selection.cursor())
            .flatten()
            .map(ScrollRequest::Cursor)
    }

    /// Whether Back / Forward is bringing back a place, which the next path
    /// change must not overwrite.
    pub(crate) const fn view_restoring(&self) -> bool {
        self.view.restore_scroll.is_some()
    }

    /// Brings back where you were in the entry Back / Forward lands on.
    fn restore_place(&mut self) {
        let place = self.history.place().cloned().unwrap_or_default();
        self.view.restore_scroll = Some(place.scroll_y);
        self.pending_select =
            (!place.selected.is_empty()).then(|| crate::app::file_ops::PendingSelect {
                paths: place.selected,
                rename: false,
                since: std::time::Instant::now(),
            });
    }

    pub const fn can_go_back(&self) -> bool {
        self.history.can_go_back()
    }
    pub const fn can_go_forward(&self) -> bool {
        self.history.can_go_forward()
    }
    /// Steps back in this tab's history and returns the navigation to run.
    pub fn go_back(&mut self) -> Option<ActionToPerform> {
        let place = self.current_place();
        self.history.save_place(place);
        let entry = self.history.back();
        if entry.is_some() {
            self.restore_place();
        }
        entry.map(|entry| {
            ActionToPerform::TabAction(
                super::commands::TabTarget::TabWithId(self.id),
                TabAction::Restore(entry),
            )
        })
    }
    /// Steps forward in this tab's history and returns the navigation to run.
    pub fn go_forward(&mut self) -> Option<ActionToPerform> {
        let place = self.current_place();
        self.history.save_place(place);
        let entry = self.history.forward();
        if entry.is_some() {
            self.restore_place();
        }
        entry.map(|entry| {
            ActionToPerform::TabAction(
                super::commands::TabTarget::TabWithId(self.id),
                TabAction::Restore(entry),
            )
        })
    }
    /// Closes the search bar without navigating, keeping its options for
    /// the next time it opens.
    pub fn end_search(&mut self) {
        if let Some(search) = self.search.take() {
            self.closed_search = Some(Search {
                value: String::new(),
                chips: Vec::new(),
                saved_from: None,
                ..search
            });
        }
    }

    pub fn toggle_search(&mut self) {
        if let Some(search) = self.search.take() {
            let was_deep = search.depth > 1;
            self.closed_search = Some(search);
            if self.current_path.multiple_paths() {
                let Some(mut entry) = self.history.back() else {
                    return;
                };
                // Leaving favorites closes the search, whatever was there.
                entry.search = None;
                TabAction::Restore(entry).schedule_tab(self.id);
            } else if was_deep {
                TabAction::RequestFilesRefresh.schedule_tab(self.id);
            }
        } else {
            let search = self.closed_search.take().unwrap_or_default();
            let restored_depth = search.depth;
            let is_multiple = self.current_path.multiple_paths();
            self.search = Some(search);
            if is_multiple || restored_depth > 1 {
                TabAction::RequestFilesRefresh.schedule_tab(self.id);
            }
        }
    }
}

pub fn build_collator(case_sensitive: bool) -> CollatorBorrowed<'static> {
    let mut options = CollatorOptions::default();
    options.strength = if case_sensitive {
        Some(Strength::Tertiary)
    } else {
        Some(Strength::Primary)
    };
    options.alternate_handling = Some(AlternateHandling::Shifted);
    let lang = bevy_device_lang::get_lang().unwrap_or_else(|| "en".to_string());
    let prefs = Locale::try_from_str(&lang)
        .unwrap_or_else(|_| Locale::try_from_str("en").expect("Failed to create default locale"));
    Collator::try_new(prefs.into(), options).expect("Failed to create collator")
}

/// Names one tab for its whole life, whichever pane or window it is in.
/// Ids are never reused, so a stale one finds nothing instead of another tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TabId(std::num::NonZeroU32);

impl TabId {
    /// The id as a number, for plugins.
    pub const fn number(self) -> u32 {
        self.0.get()
    }

    fn next() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(1);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        Self(std::num::NonZeroU32::new(id).expect("tab ids start at 1"))
    }
}

impl std::fmt::Display for TabId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl TabData {
    /// Stops what a closed tab still runs, its load and its watchers, and
    /// forgets the per-tab state kept in egui memory.
    pub fn release(self, watchers: &mut crate::watcher::DirectoryWatchers, ctx: &Context) {
        use std::sync::atomic::Ordering;
        self.cancel_token.store(true, Ordering::SeqCst);
        self.refresh_generation.fetch_add(1, Ordering::SeqCst);
        watchers.stop_many(self.active_watch_specs.into_iter().map(|(path, _)| path));
        ctx.data_mut(|data| data.remove::<PopupOpened>(popup_memory_id(self.id)));
    }

    /// The search has an `opened:` filter, whose results change with the
    /// open history.
    pub fn filters_by_opening(&self) -> bool {
        self.search.as_ref().is_some_and(|search| {
            search
                .effective_chips()
                .iter()
                .any(|chip| matches!(chip, crate::app::search::Chip::Opened(_)))
        })
    }

    pub fn is_searching(&self) -> bool {
        self.search.as_ref().is_some_and(Search::has_query)
    }

    /// Borrow the entry visible at `visible_row` (0-based in visible order).
    /// No allocation: the row borrows the shared listing.
    pub fn entry_at(&self, visible_row: usize) -> Option<EntryRef<'_>> {
        let index = *self.visible_entries.get(visible_row)? as usize;
        Some(self.listing.entry(index))
    }

    pub fn total_entry_count(&self) -> usize {
        self.listing.len()
    }
    pub fn from_path(path: &Path) -> Self {
        let new = Self::blank(path);
        TabAction::ChangePaths(CurrentPath::One(path.into())).schedule_tab(new.id);
        new
    }

    /// A tab showing `path` that reads it only once it is first drawn.
    pub fn unloaded(path: &Path) -> Self {
        let mut tab = Self::blank(path);
        tab.current_path = CurrentPath::One(path.to_path_buf());
        tab.unloaded = true;
        tab
    }

    fn blank(path: &Path) -> Self {
        let mut top_display_path = TopDisplayPath::default();
        top_display_path.build(path);
        Self {
            id: TabId::next(),
            listing: Arc::default(),
            order: vec![],
            visible_entries: vec![],
            order_generation: 0,
            filter_key: None,
            active_watch_specs: vec![],
            current_path: CurrentPath::None,
            show_hidden: false,
            display_type: DisplayType::default(),
            search: None,
            closed_search: None,
            path_edit: None,
            load: LoadState::Idle,
            refresh_generation: Arc::new(AtomicU64::new(0)),
            cancel_token: Arc::new(AtomicBool::new(false)),
            history: History::default(),
            top_display_path,
            watcher_specs_cache: WatcherSpecsCache::default(),
            linked_watch_roots: crate::app::dir_handling::LinkedWatchRoots::default(),
            selection: Selection::default(),
            selection_snapshot: None,
            pending_select: None,
            search_committed: None,
            search_dirty_since: None,
            keep_search_once: false,
            quick_look: false,
            view: ViewState::default(),
            unloaded: false,
        }
    }
}

#[allow(clippy::struct_excessive_bools)]
pub struct MyTabViewer<'a> {
    tabs: &'a mut TabStore,
    closeable: bool,
    /// The window has more than one tab, so one can move out.
    has_other_tabs: bool,
    tab_paths: Vec<PathBuf>,
    active_tab: TabId,
    focused: bool,
    assets: &'a mut AssetManager,
    cut: Arc<std::collections::HashSet<PathBuf>>,
    /// Tab-bar actions collected while drawing, applied after the dock is shown.
    tab_requests: TabRequests,
    /// Grid tooltips show full paths (search results can share names).
    show_full_paths: bool,
}

/// Tab-bar actions that change the dock layout, which can't be done while
/// the dock is being drawn.
#[derive(Default)]
struct TabRequests {
    /// Panes whose "+" (or "New tab") was used: open a tab there.
    add: Vec<(egui_dock::NodePath, Option<PathBuf>)>,
    /// Tabs to close.
    close: Vec<TabId>,
    /// Close every tab except this one.
    close_others: Option<TabId>,
    /// Move this tab into a window of its own.
    to_new_window: Option<TabId>,
    /// Tabs the dock already took out of the layout while drawing.
    closed: Vec<TabId>,
}

const GRID_TILE_PADDING: f32 = 10.0;
const GRID_VIEW_PADDING: f32 = 8.0;

const fn tile_preview_intent(
    eligible: bool,
    hovered: bool,
    selected: bool,
    selected_budget: &mut u32,
) -> Option<PreviewIntent> {
    if !eligible {
        return None;
    }
    if hovered {
        return Some(PreviewIntent::Hovered);
    }
    if selected && *selected_budget > 0 {
        *selected_budget = selected_budget.saturating_sub(1);
        return Some(PreviewIntent::Selected);
    }
    None
}

/// Captured row interaction result collected while the taffy tui borrow is active,
/// so they can be processed afterwards (when `tab` can be borrowed again).
struct RowResult {
    row_index: usize,
    response: egui::Response,
}

/// Shared per-frame setup extracted from [`MyTabViewer::grid_view`] and
/// [`MyTabViewer::list_view`]: keyboard shortcuts (toggle hidden, go to
/// parent, undoer feed) handled identically by both views, plus the per-tab
/// interaction state (open popup, favorites) loaded once instead of in each
/// view. Returns `None` when the caller should early-return (parent
/// navigation was scheduled, or the tab is still loading).
struct ViewHeader {
    tab_id: TabId,
    tab_popup_id: Id,
    opened_popup: usize,
    /// Where to scroll this frame: the moved cursor, or a folder's start or
    /// restored position.
    scroll: Option<ScrollRequest>,
    favorites: Locations,
    /// Click target behind the rows: clicking empty space clears the
    /// selection, dragging selects the items in a rectangle and
    /// right-clicking it opens the folder's menu.
    background: egui::Response,
}

/// Draws the keyboard cursor's ring: a thin inset outline, so the cursor
/// shows inside a selected row as well as on its own.
/// Tells assistive technology what a row or tile is.
fn describe_entry(response: &egui::Response, entry: &EntryRef<'_>, selected: bool) {
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            true,
            selected,
            entry.file_name,
        )
    });
}

fn draw_cursor_ring(ui: &Ui, rect: egui::Rect, radius: f32) {
    ui.painter().rect_stroke(
        rect.shrink(2.0),
        radius,
        egui::Stroke::new(1.0, ui.visuals().text_color().gamma_multiply(0.6)),
        egui::StrokeKind::Inside,
    );
}

impl MyTabViewer<'_> {
    /// The row whose cursor ring to draw: the cursor, while this view has
    /// keyboard focus and the cursor isn't simply the one selected row.
    fn cursor_ring_row(&self, tab: &TabData) -> Option<usize> {
        let cursor = tab.selection.cursor()?;
        let only_selected = tab.selection.len() == 1 && tab.selection.contains(cursor);
        (self.focused && self.active_tab == tab.id && !only_selected).then_some(cursor)
    }

    /// Selection color: the accent while this view has keyboard focus,
    /// faded otherwise, so the focused pane of a split stands out.
    fn selection_fill(&self, ui: &Ui, tab: &TabData) -> Color32 {
        let fill = ui.visuals().selection.bg_fill;
        if self.focused && self.active_tab == tab.id {
            fill
        } else {
            ui.visuals().gray_out(fill)
        }
    }

    /// The message shown in place of an empty folder.
    fn empty_folder_message(ui: &mut Ui, tab: &TabData) {
        let area = ui.available_rect_before_wrap();
        let rect = egui::Rect::from_center_size(area.center(), egui::vec2(area.width(), 80.0));
        let mut message = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::Center)),
        );
        if tab.current_path.is_trash() {
            message.weak(format!("The {} is empty", crate::fileops::trash_bin::LABEL));
            return;
        }
        if Self::access_denied(ui, tab) {
            message.weak("DirFleet doesn't have permission to open this folder");
            #[cfg(target_os = "macos")]
            if message.link("Open Privacy settings").clicked() {
                crate::shell::open_full_disk_access_settings();
            }
            return;
        }
        message.weak("This folder is empty");
        if !tab.show_hidden {
            message.weak("Hidden items aren't shown.");
            if message.link("Show hidden items").clicked() {
                ActionToPerform::RunCommand(crate::app::keymap::Command::ToggleHidden).schedule();
            }
        }
    }

    /// Whether the folder came back empty because it can't be read. Asked
    /// once per loaded listing, not every frame.
    fn access_denied(ui: &Ui, tab: &TabData) -> bool {
        let Some(path) = tab.current_path.single_path() else {
            return false;
        };
        let id = Id::new(("access_denied", tab.id, Arc::as_ptr(&tab.listing)));
        ui.data_mut(|data| {
            *data.get_temp_mut_or_insert_with(id, || {
                std::fs::read_dir(&path)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
            })
        })
    }

    /// Whether the entry was cut to the clipboard and should look dimmed.
    fn is_cut(&self, entry: &EntryRef<'_>) -> bool {
        !self.cut.is_empty() && self.cut.contains(&entry.get_path())
    }

    /// Shared header for both views. Runs the keyboard shortcuts that
    /// `grid_view` and `list_view` previously duplicated verbatim, draws the
    /// loading spinner, and loads the per-tab interaction state into a
    /// [`ViewHeader`]. Returning `None` signals the caller must early-return.
    fn begin_view(ui: &mut Ui, tab: &mut TabData) -> Option<ViewHeader> {
        if tab.load.is_loading() && tab.visible_entries.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.spinner();
                ui.label(tab.load.progress().unwrap_or("Loading..."));
            });
            return None;
        }

        let tab_id = tab.id;
        let opened_popup = ui
            .ctx()
            .data(|data| data.get_temp::<PopupOpened>(popup_memory_id(tab_id)))
            .and_then(|d| {
                if egui::Popup::is_id_open(ui.ctx(), d.0) {
                    Some(d.1)
                } else {
                    None
                }
            })
            .unwrap_or(usize::MAX);
        let just_changed = std::mem::take(&mut tab.selection.just_changed);
        let scroll = tab.take_scroll_request(just_changed);
        // Registered before the rows, so rows (added later) win the hit test.
        let background = ui.interact(
            ui.available_rect_before_wrap(),
            Id::new("view_background").with(tab_id),
            egui::Sense::click_and_drag(),
        );
        if !tab.load.is_loading() && tab.visible_entries.is_empty() && !tab.is_searching() {
            Self::empty_folder_message(ui, tab);
        }
        Some(ViewHeader {
            background,
            tab_id,
            tab_popup_id: Id::new(tab_id).with(1500),
            opened_popup,
            scroll,
            favorites: ui.data_get_persisted::<Locations>().unwrap_or_default(),
        })
    }
}

impl TabViewer for MyTabViewer<'_> {
    // The dock holds only the layout; tab data lives in `Workspace::tabs`.
    type Tab = TabId;

    // A tab dragged out of the dock becomes an OS window of its own (see
    // `Workspace::take_torn_off_tabs`). The main window keeps its last tab.
    fn allowed_in_windows(&self, _tab: &mut Self::Tab) -> bool {
        self.closeable
    }

    fn closeable(&mut self, _: &mut Self::Tab) -> bool {
        self.closeable
    }

    fn on_add(&mut self, path: egui_dock::NodePath) {
        self.tab_requests.add.push((path, None));
    }

    // Middle-click and the close button are handled by egui_dock itself,
    // which calls `on_close` once per tab. Closing from the context menu is
    // applied after the dock is drawn (see `Workspace::apply_tab_requests`):
    // queuing those through `force_close` as well removed the same tab twice
    // and, once a split pane emptied, indexed a node that no longer existed.
    fn on_close(&mut self, tab: &mut Self::Tab) -> egui_dock::widgets::tab_viewer::OnCloseResponse {
        self.tab_requests.closed.push(*tab);
        egui_dock::widgets::tab_viewer::OnCloseResponse::Close
    }

    fn context_menu(&mut self, ui: &mut Ui, tab: &mut Self::Tab, path: egui_dock::NodePath) {
        let id = *tab;
        if ui.button("New tab").clicked() {
            self.tab_requests.add.push((path, None));
            ui.close();
        }
        if ui.button("Duplicate tab").clicked() {
            let folder = self
                .tabs
                .get(&id)
                .and_then(|tab| tab.current_path.single_path());
            self.tab_requests.add.push((path, folder));
            ui.close();
        }
        if ui
            .add_enabled(self.has_other_tabs, egui::Button::new("Move to new window"))
            .clicked()
        {
            self.tab_requests.to_new_window = Some(id);
            ui.close();
        }
        ui.separator();
        ui.add_enabled_ui(self.closeable, |ui| {
            if ui.button("Close tab").clicked() {
                self.tab_requests.close.push(id);
                ui.close();
            }
            if ui.button("Close other tabs").clicked() {
                self.tab_requests.close_others = Some(id);
                ui.close();
            }
        });
        if ui.button("Reopen closed tab").clicked() {
            crate::app::commands::ActionToPerform::RunCommand(
                crate::app::keymap::Command::ReopenClosedTab,
            )
            .schedule();
            ui.close();
        }
    }

    fn id(&mut self, tab: &mut Self::Tab) -> Id {
        Id::new(*tab)
    }

    // Returns the current `tab`'s title.
    fn title(&mut self, tab: &mut Self::Tab) -> egui_dock::egui::WidgetText {
        let Some(tab) = self.tabs.get(tab) else {
            return WidgetText::default();
        };
        if tab.is_searching() {
            tab.search
                .as_ref()
                .map(|s| format!("Searching: {}", s.value))
                .unwrap_or_default()
                .into()
        } else {
            let path = tab.current_path.get_name_from_path();
            path.into()
        }
    }
    fn scroll_bars(&self, _tab: &Self::Tab) -> [bool; 2] {
        [false, false]
    }

    /// Defines the contents of a given `tab`.
    #[allow(clippy::too_many_lines)]
    fn ui(&mut self, ui: &mut Ui, tab: &mut Self::Tab) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::MyTabViewer::ui");
        // Taken out while drawn, so the views can borrow the viewer mutably.
        let Some(mut data) = self.tabs.remove(tab) else {
            return;
        };
        if std::mem::take(&mut data.unloaded) {
            TabAction::ChangePaths(data.current_path.clone()).schedule_tab(data.id);
        }
        match data.display_type {
            DisplayType::List => self.list_view(ui, &mut data),
            DisplayType::Icons => self.grid_view(ui, &mut data),
        }
        self.tabs.insert(*tab, data);
    }
}

/// Every open tab, by id. The dock layout only refers to them.
pub type TabStore = foldhash::HashMap<TabId, TabData>;

/// What the selection referred to when last drawn, so it can follow the same
/// files after the view is re-sorted, filtered or reloaded. Identities are
/// listing indices plus the listing itself, which is cheap to record every
/// frame (no per-row path allocation).
#[derive(Debug, Clone, Default)]
struct SelectionSnapshot {
    directory: CurrentPath,
    listing: Arc<Listing>,
    visible_hash: u64,
    selection_hash: u64,
    /// Listing indices of the selected rows.
    selected: Vec<u32>,
    /// Listing index of the cursor row.
    cursor: Option<u32>,
}

fn slice_hash<T: std::hash::Hash>(values: &[T]) -> u64 {
    use std::hash::BuildHasher;
    foldhash::fast::FixedState::default().hash_one(values)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::data::files::{DirEntry, EntryType};

    use image::{Rgba, RgbaImage};
    use std::cell::RefCell;
    use std::sync::OnceLock;

    pub static SNAPSHOT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn tile_preview_policy_keeps_hover_and_caps_selected_entries() {
        let mut budget = 4;
        for _ in 0..4 {
            assert_eq!(
                tile_preview_intent(true, false, true, &mut budget),
                Some(PreviewIntent::Selected)
            );
        }
        assert_eq!(budget, 0);
        assert_eq!(tile_preview_intent(true, false, true, &mut budget), None);
        assert_eq!(
            tile_preview_intent(true, true, true, &mut budget),
            Some(PreviewIntent::Hovered)
        );
        assert_eq!(tile_preview_intent(false, true, true, &mut budget), None);
    }

    /// Build a list of [`DirEntry`] values from the project's `src/` directory.
    ///
    /// Using real filesystem entries means paths, timestamps, and sizes are all
    /// realistic, which exercises the galley-pool paths in the renderer.
    /// A fixed listing, so snapshots don't change when the source tree does.
    fn make_entries() -> Vec<DirEntry> {
        let folders = ["app", "data", "widgets"];
        let files = [
            "consts.rs",
            "helper.rs",
            "locations.rs",
            "main.rs",
            "watcher.rs",
            "windows_tools.rs",
        ];
        folders
            .iter()
            .map(|name| {
                let mut entry = DirEntry::test_new(&format!("src/{name}"));
                entry.meta.entry_type = crate::data::files::EntryType::Directory;
                entry
            })
            .chain(
                files
                    .iter()
                    .map(|name| DirEntry::test_new(&format!("src/{name}"))),
            )
            .collect()
    }

    fn fixture_root() -> &'static std::path::Path {
        static ROOT: OnceLock<std::path::PathBuf> = OnceLock::new();
        ROOT.get_or_init(|| {
            let root = std::env::temp_dir().join("lwa_fm_kittest_grid");
            std::fs::create_dir_all(&root).expect("grid fixture root must be created");

            for dir_name in ["Applications", "Projects", "Pictures"] {
                std::fs::create_dir_all(root.join(dir_name))
                    .expect("grid fixture directory must be created");
            }

            for (name, width, height, base) in [
                ("Zrzut ekranu 2026-03-16 o 12.45.04.png", 160, 90, [52, 152, 219, 255]),
                ("Zrzut ekranu 2026-03-09 o 13.14.58.png", 90, 160, [231, 76, 60, 255]),
                (
                    "Vacation panorama with a very descriptive title.png",
                    240,
                    72,
                    [46, 204, 113, 255],
                ),
                ("Square album art.png", 128, 128, [241, 196, 15, 255]),
            ] {
                write_png_fixture(&root.join(name), width, height, base);
            }

            for name in [
                "Old World.app",
                "Factorio save data.zip",
                "Meeting notes.txt",
                "Quarterly financial summary.pdf",
                "README.md",
                "Recording archive.rs",
                "super-extra-long filename that should truncate gracefully in the icon grid view.txt",
            ] {
                std::fs::write(root.join(name), b"fixture").expect("text fixture must be written");
            }

            root
        })
        .as_path()
    }

    fn write_png_fixture(path: &std::path::Path, width: u32, height: u32, base: [u8; 4]) {
        let mut image = RgbaImage::new(width, height);
        let max_x = width.saturating_sub(1).max(1);
        let max_y = height.saturating_sub(1).max(1);

        for (x, y, pixel) in image.enumerate_pixels_mut() {
            let red = base[0].saturating_add(((x * 40) / max_x) as u8);
            let green = base[1].saturating_add(((y * 40) / max_y) as u8);
            let blue = base[2].saturating_sub(((x * 30) / max_x) as u8);
            *pixel = Rgba([red, green, blue, base[3]]);
        }

        image
            .save(path)
            .expect("png fixture must be generated successfully");
    }

    fn grid_entries() -> Vec<DirEntry> {
        let mut entries: Vec<_> = std::fs::read_dir(fixture_root())
            .expect("grid fixture root must exist")
            .filter_map(std::result::Result::ok)
            .filter_map(|entry| DirEntry::try_from(entry).ok())
            .collect();
        entries.sort_by(|left, right| left.get_splitted_path().1.cmp(right.get_splitted_path().1));
        entries
    }

    fn long_name_entries() -> Vec<DirEntry> {
        let mut entries = grid_entries();
        entries.sort_by(|left, right| {
            right
                .get_splitted_path()
                .1
                .len()
                .cmp(&left.get_splitted_path().1.len())
        });
        entries.truncate(9);
        entries
    }

    fn mixed_thumbnail_entries() -> Vec<DirEntry> {
        let mut entries = grid_entries();
        entries.sort_by_key(|entry| {
            let name = entry.get_splitted_path().1;
            match name {
                "Vacation panorama with a very descriptive title.png" => 0,
                "Zrzut ekranu 2026-03-09 o 13.14.58.png" => 1,
                "Square album art.png" => 2,
                "Zrzut ekranu 2026-03-16 o 12.45.04.png" => 3,
                "Applications" => 4,
                "README.md" => 5,
                _ => 6,
            }
        });
        entries.truncate(8);
        entries
    }

    fn populated_tabs_from_entries(entries: &[DirEntry], display_type: DisplayType) -> Workspace {
        let path = fixture_root();
        let mut tab = TabData::from_path(path);
        tab.display_type = display_type;
        tab.set_entries(entries);
        {
            let mut tabs = Workspace::from_tabs(vec![tab]);
            tabs.focused = true;
            tabs
        }
    }

    /// Build a [`Workspace`] whose active tab is populated from `src/`.
    fn populated_tabs() -> Workspace {
        let path = std::path::Path::new("src");
        let mut tab = TabData::from_path(path);
        tab.set_entries(&make_entries());
        {
            let mut tabs = Workspace::from_tabs(vec![tab]);
            tabs.focused = true;
            tabs
        }
    }

    /// Draw one frame of the dock view, seeding galley pools and draining commands.
    fn draw_frame(
        ui: &mut egui::Ui,
        my_tabs: &mut Workspace,
        assets: &RefCell<crate::app::assets::AssetManager>,
    ) {
        draw_frame_with_selection(ui, my_tabs, None, assets);
    }

    fn draw_frame_with_selection(
        ui: &mut egui::Ui,
        my_tabs: &mut Workspace,
        selected_fields: Option<&[usize]>,
        assets: &RefCell<crate::app::assets::AssetManager>,
    ) {
        let ctx = ui.ctx().clone();
        crate::app::ui::configure_test(&ctx, false);
        ctx.set_theme(egui::Theme::Dark);
        // The UI harness adds an outer margin; paint it like the full-window panel.
        ui.painter().with_clip_rect(ctx.content_rect()).rect_filled(
            ctx.content_rect(),
            0.0,
            ctx.global_style().visuals.panel_fill,
        );
        FILE_NAME_POOL.with_borrow_mut(LruCache::clear);
        FOLDER_NAME_POOL.with_borrow_mut(LruCache::clear);
        text_fit::clear_pool();
        TIME_POOL.with_borrow_mut(LruCache::clear);
        SIZES_POOL.with_borrow_mut(LruCache::clear);
        TEXT_CACHE_STYLE.with_borrow_mut(|style| *style = None);
        let mut assets = assets.borrow_mut();
        assets.poll_results(&ctx);
        {
            if let Some(indices) = selected_fields {
                my_tabs
                    .get_current_tab()
                    .expect("missing active tab")
                    .selection
                    .set_rows(indices.iter().copied());
            }
            my_tabs.ui(ui, &mut assets);
        }
        assets.wait_for_idle(&ctx);

        // Drain the command queue so it does not fill up across frames.
        while crate::app::commands::COMMANDS_QUEUE.pop().is_some() {}
        // Asset decoding is intentionally asynchronous. Give workers a small,
        // deterministic opportunity to finish between synthetic frames so
        // snapshots do not depend on host scheduling speed.
    }

    #[test]
    fn large_list_only_lays_out_virtualized_file_names() {
        let _guard = SNAPSHOT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        FILE_NAME_LAYOUT_COUNT.store(0, std::sync::atomic::Ordering::Relaxed);
        let entries: Vec<DirEntry> = (0..10_000)
            .map(|index| DirEntry::test_new(&format!("/virtual/file_{index:05}.txt")))
            .collect();
        let assets = RefCell::new(crate::app::assets::AssetManager::default());
        let mut harness = egui_kittest::Harness::builder()
            .with_options(crate::app::ui::snapshot_options())
            .with_size(egui::Vec2::new(900.0, 500.0))
            .build_ui_state(
                |ui, my_tabs: &mut Workspace| draw_frame(ui, my_tabs, &assets),
                populated_tabs_from_entries(&entries, DisplayType::List),
            );

        harness.run_steps(1);

        let layouts = FILE_NAME_LAYOUT_COUNT.load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            layouts < 256,
            "virtualized list laid out {layouts} names for 10,000 entries"
        );
        FILE_NAME_POOL.with_borrow(|pool| assert!(pool.len() < 64));
    }

    /// Frame-time baseline for large listings. Run with
    /// `cargo test --release perf_ -- --ignored --nocapture`.
    /// `LWA_PERF_N` overrides the entry count (default 100k).
    fn perf_frame_time(display_type: DisplayType, select_all: bool) {
        let _guard = SNAPSHOT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = crate::app::perf_tests::perf_count(100_000);
        let entries: Vec<DirEntry> = (0..count)
            .map(|index| DirEntry::test_new(&format!("/virtual/file_{index:06}.txt")))
            .collect();
        let assets = RefCell::new(crate::app::assets::AssetManager::default());
        let selection: Option<Vec<usize>> = select_all.then(|| (0..count).collect());
        let mut harness = egui_kittest::Harness::builder()
            .with_options(crate::app::ui::snapshot_options())
            .with_size(egui::Vec2::new(1100.0, 720.0))
            .build_ui_state(
                |ui, my_tabs: &mut Workspace| {
                    let ctx = ui.ctx().clone();
                    crate::app::ui::configure_test(&ctx, false);
                    if let Some(indices) = &selection {
                        let tab = my_tabs.get_current_tab().expect("missing active tab");
                        if tab.selection.is_empty() {
                            tab.selection.set_rows(indices.iter().copied());
                        }
                    }
                    let mut assets = assets.borrow_mut();
                    assets.begin_frame();
                    assets.poll_results(&ctx);
                    my_tabs.ui(ui, &mut assets);
                    assets.end_frame();
                    while crate::app::commands::COMMANDS_QUEUE.pop().is_some() {}
                },
                populated_tabs_from_entries(&entries, display_type),
            );
        // Warm galley pools and icon requests before measuring.
        harness.run_steps(5);
        let frames = 50;
        let started = std::time::Instant::now();
        for _ in 0..frames {
            harness.run_steps(1);
        }
        let per_frame = started.elapsed() / frames;
        eprintln!(
            "perf frame {display_type:?} entries={count} select_all={select_all}: {per_frame:?}/frame"
        );
    }

    #[test]
    #[ignore = "performance baseline; run explicitly"]
    fn perf_list_view_frame() {
        perf_frame_time(DisplayType::List, false);
    }

    #[test]
    #[ignore = "performance baseline; run explicitly"]
    fn perf_list_view_frame_all_selected() {
        perf_frame_time(DisplayType::List, true);
    }

    #[test]
    #[ignore = "performance baseline; run explicitly"]
    fn perf_grid_view_frame_all_selected() {
        perf_frame_time(DisplayType::Icons, true);
    }

    // ── Snapshot: dock view at a standard desktop width ──────────────────────

    #[test]
    fn test_dock_view_wide() {
        let _guard = SNAPSHOT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let assets = RefCell::new(crate::app::assets::AssetManager::default());
        let mut harness = egui_kittest::Harness::builder()
            .with_options(crate::app::ui::snapshot_options())
            .with_size(egui::Vec2::new(900.0, 500.0))
            .build_ui_state(
                |ui, my_tabs: &mut Workspace| draw_frame(ui, my_tabs, &assets),
                populated_tabs(),
            );

        harness.run_steps(12);
        harness.snapshot("dock_view_wide");
    }

    // ── Snapshot: dock view at a narrow width (split-pane scenario) ───────────
    //
    // Before the fix the name column had `min_size.width = 260 px` and the grid
    // had `overflow.x = Visible`, which forced cells to bleed outside the panel
    // at this width.  The snapshot now shows text clipped with "…" instead.

    #[test]
    fn test_dock_view_narrow() {
        let _guard = SNAPSHOT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let assets = RefCell::new(crate::app::assets::AssetManager::default());
        let mut harness = egui_kittest::Harness::builder()
            .with_options(crate::app::ui::snapshot_options())
            .with_size(egui::Vec2::new(350.0, 500.0))
            .build_ui_state(
                |ui, my_tabs: &mut Workspace| draw_frame(ui, my_tabs, &assets),
                populated_tabs(),
            );

        harness.run_steps(12);
        harness.snapshot("dock_view_narrow");
    }

    // ── Snapshot: dock view with long / duplicated names (ellipsis stress) ────

    #[test]
    fn test_dock_view_long_names() {
        let _guard = SNAPSHOT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Duplicate entries so we get more rows and more truncation to render.
        let entries = make_entries();
        let all: Vec<DirEntry> = entries
            .iter()
            .chain(entries.iter())
            .take(20)
            .cloned()
            .collect();

        let path = std::path::Path::new("src");
        let mut tab = TabData::from_path(path);
        tab.set_entries(&all);
        let my_tabs = {
            let mut tabs = Workspace::from_tabs(vec![tab]);
            tabs.focused = false;
            tabs
        };

        let assets = RefCell::new(crate::app::assets::AssetManager::default());
        let mut harness = egui_kittest::Harness::builder()
            .with_options(crate::app::ui::snapshot_options())
            .with_size(egui::Vec2::new(400.0, 600.0))
            .build_ui_state(
                |ui, my_tabs: &mut Workspace| draw_frame(ui, my_tabs, &assets),
                my_tabs,
            );

        harness.run_steps(12);
        harness.snapshot("dock_view_long_names");
    }

    // ── Snapshot: empty tab (no entries) ─────────────────────────────────────

    #[test]
    fn test_dock_view_empty_tab() {
        let _guard = SNAPSHOT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tab = TabData::from_path(std::path::Path::new("src"));
        let my_tabs = {
            let mut tabs = Workspace::from_tabs(vec![tab]);
            tabs.focused = false;
            tabs
        };

        let assets = RefCell::new(crate::app::assets::AssetManager::default());
        let mut harness = egui_kittest::Harness::builder()
            .with_options(crate::app::ui::snapshot_options())
            .with_size(egui::Vec2::new(700.0, 400.0))
            .build_ui_state(
                |ui, my_tabs: &mut Workspace| draw_frame(ui, my_tabs, &assets),
                my_tabs,
            );

        harness.run_steps(12);
        harness.snapshot("dock_view_empty_tab");
    }

    #[test]
    fn test_dock_view_icons_wide() {
        let _guard = SNAPSHOT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let assets = RefCell::new(crate::app::assets::AssetManager::default());
        let mut harness = egui_kittest::Harness::builder()
            .with_options(crate::app::ui::snapshot_options())
            .with_size(egui::Vec2::new(960.0, 620.0))
            .build_ui_state(
                |ui, my_tabs: &mut Workspace| draw_frame(ui, my_tabs, &assets),
                populated_tabs_from_entries(&grid_entries(), DisplayType::Icons),
            );

        harness.run_steps(12);
        harness.snapshot("dock_view_icons_wide");
    }

    #[test]
    fn test_dock_view_icons_narrow() {
        let _guard = SNAPSHOT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let assets = RefCell::new(crate::app::assets::AssetManager::default());
        let mut harness = egui_kittest::Harness::builder()
            .with_options(crate::app::ui::snapshot_options())
            .with_size(egui::Vec2::new(420.0, 620.0))
            .build_ui_state(
                |ui, my_tabs: &mut Workspace| draw_frame(ui, my_tabs, &assets),
                populated_tabs_from_entries(&grid_entries(), DisplayType::Icons),
            );

        harness.run_steps(12);
        harness.snapshot("dock_view_icons_narrow");
    }

    #[test]
    fn test_dock_view_icons_long_names() {
        let _guard = SNAPSHOT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let assets = RefCell::new(crate::app::assets::AssetManager::default());
        let mut harness = egui_kittest::Harness::builder()
            .with_options(crate::app::ui::snapshot_options())
            .with_size(egui::Vec2::new(620.0, 700.0))
            .build_ui_state(
                |ui, my_tabs: &mut Workspace| draw_frame(ui, my_tabs, &assets),
                populated_tabs_from_entries(&long_name_entries(), DisplayType::Icons),
            );

        harness.run_steps(24);
        harness.snapshot("dock_view_icons_long_names");
    }

    #[test]
    fn test_dock_view_icons_mixed_thumbnails() {
        let _guard = SNAPSHOT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let assets = RefCell::new(crate::app::assets::AssetManager::default());
        let mut harness = egui_kittest::Harness::builder()
            .with_options(crate::app::ui::snapshot_options())
            .with_size(egui::Vec2::new(820.0, 520.0))
            .build_ui_state(
                |ui, my_tabs: &mut Workspace| draw_frame(ui, my_tabs, &assets),
                populated_tabs_from_entries(&mixed_thumbnail_entries(), DisplayType::Icons),
            );

        harness.run_steps(24);
        harness.snapshot("dock_view_icons_mixed_thumbnails");
    }

    #[test]
    fn test_dock_view_icons_empty_tab() {
        let _guard = SNAPSHOT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut tab = TabData::from_path(fixture_root());
        tab.display_type = DisplayType::Icons;
        let my_tabs = {
            let mut tabs = Workspace::from_tabs(vec![tab]);
            tabs.focused = false;
            tabs
        };

        let assets = RefCell::new(crate::app::assets::AssetManager::default());
        let mut harness = egui_kittest::Harness::builder()
            .with_options(crate::app::ui::snapshot_options())
            .with_size(egui::Vec2::new(720.0, 420.0))
            .build_ui_state(
                |ui, my_tabs: &mut Workspace| draw_frame(ui, my_tabs, &assets),
                my_tabs,
            );

        harness.run_steps(12);
        harness.snapshot("dock_view_icons_empty_tab");
    }

    #[test]
    fn test_dock_view_icons_selected() {
        let _guard = SNAPSHOT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let assets = RefCell::new(crate::app::assets::AssetManager::default());
        let mut harness = egui_kittest::Harness::builder()
            .with_options(crate::app::ui::snapshot_options())
            .with_size(egui::Vec2::new(960.0, 620.0))
            .build_ui_state(
                |ui, my_tabs: &mut Workspace| {
                    draw_frame_with_selection(ui, my_tabs, Some(&[2]), &assets);
                },
                populated_tabs_from_entries(&grid_entries(), DisplayType::Icons),
            );

        harness.run_steps(24);
        harness.snapshot("dock_view_icons_selected");
    }

    /// Two panes: `[a, b]` on the left, `[c]` alone on the right, as after
    /// dragging a tab to the right edge.
    fn split_tabs() -> (Workspace, [TabId; 3]) {
        let [a, b, c] = ["src", "src/app", "src/data"].map(|folder| {
            let mut tab = TabData::from_path(Path::new(folder));
            tab.current_path = CurrentPath::One(folder.into());
            tab
        });
        let ids = [a.id, b.id, c.id];
        let mut tabs = Workspace::from_tabs(vec![a, b]);
        tabs.focused = true;
        tabs.split_right(c);
        while crate::app::commands::COMMANDS_QUEUE.pop().is_some() {}
        (tabs, ids)
    }

    #[test]
    fn closing_lone_tab_of_split_pane_keeps_the_rest() {
        let _guard = SNAPSHOT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mut tabs, [a, b, c]) = split_tabs();
        tabs.apply_tab_requests(TabRequests {
            close: vec![c, c],
            ..TabRequests::default()
        });
        assert_eq!(tabs.get_tab_ids(), vec![a, b]);
        assert_eq!(tabs.closed.len(), 1);
    }

    /// Middle-clicking the lone tab of a split pane used to queue its removal
    /// twice and panic once the pane was gone.
    #[test]
    fn middle_click_closes_lone_tab_of_split_pane() {
        let _guard = SNAPSHOT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (tabs, [a, b, _]) = split_tabs();
        let assets = RefCell::new(crate::app::assets::AssetManager::default());
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(900.0, 500.0))
            .build_ui_state(
                |ui, my_tabs: &mut Workspace| draw_frame(ui, my_tabs, &assets),
                tabs,
            );
        harness.run_steps(2);
        // Tab buttons carry no accessibility label: aim at the start of the
        // right pane's tab bar, where its only tab is.
        let pane = harness
            .state_mut()
            .dock_mut()
            .iter_leaves()
            .map(|(_, leaf)| leaf.rect)
            .max_by(|l, r| l.left().total_cmp(&r.left()))
            .expect("two panes");
        let pos = pane.left_top() + egui::vec2(20.0, 12.0);
        harness.event(egui::Event::PointerMoved(pos));
        for pressed in [true, false] {
            harness.event(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Middle,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
            harness.run_steps(1);
        }
        harness.run_steps(2);
        assert_eq!(harness.state_mut().get_tab_ids(), vec![a, b]);
    }

    #[test]
    fn closed_tab_releases_its_watchers_and_load() {
        let _guard = SNAPSHOT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mut tabs, [_, _, c]) = split_tabs();
        let folder = std::env::temp_dir();
        let mut watchers = crate::watcher::DirectoryWatchers::default();
        watchers.start(&folder, notify::RecursiveMode::NonRecursive);
        let tab = tabs.get_tab_by_id(c).expect("tab");
        tab.active_watch_specs = vec![(folder, notify::RecursiveMode::NonRecursive)];
        let cancel = Arc::clone(&tab.cancel_token);
        tabs.close_tab(c);
        assert!(tabs.get_tab_by_id(c).is_none());
        let ctx = egui::Context::default();
        for tab in tabs.take_retired() {
            tab.release(&mut watchers, &ctx);
        }
        assert_eq!(watchers.watched_roots(), 0);
        assert!(cancel.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn close_others_across_split_panes() {
        let _guard = SNAPSHOT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mut tabs, [a, b, c]) = split_tabs();
        tabs.apply_tab_requests(TabRequests {
            close_others: Some(b),
            ..TabRequests::default()
        });
        assert_eq!(tabs.get_tab_ids(), vec![b]);
        assert!(tabs.get_tab_by_id(a).is_none() && tabs.get_tab_by_id(c).is_none());
        // The last tab stays open.
        tabs.close_tab(b);
        assert_eq!(tabs.get_tab_ids(), vec![b]);
    }

    // ── Unit: DirEntry constructed from real fs has correct entry type ────────

    #[test]
    fn test_dir_entry_entry_type() {
        let entries = make_entries();
        assert!(!entries.is_empty(), "src/ must contain at least one entry");
        for entry in &entries {
            let path = entry.get_path();
            if path.is_file() {
                assert!(
                    entry.is_file(),
                    "Expected file entry for {}",
                    path.display()
                );
            } else {
                assert!(
                    !entry.is_file(),
                    "Expected directory entry for {}",
                    path.display()
                );
                assert_eq!(
                    entry.meta.entry_type,
                    EntryType::Directory,
                    "Entry type mismatch for {}",
                    path.display()
                );
            }
        }
    }
}
