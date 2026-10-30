use std::{
    borrow::Cow,
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    },
};

use notify::RecursiveMode;

use icu_collator::CollatorBorrowed;
use rayon::{
    iter::{IntoParallelRefIterator, ParallelIterator},
    slice::{ParallelSlice, ParallelSliceMut},
};

use crate::{
    app::{
        Data, Search, Sort, database, open_history,
        directory_view_settings::{DirectoryShowHidden, DirectoryViewSettings},
        dock::{CurrentPath, build_collator},
    },
    data::files::{
        DirEntryMetaData, EntryType, Listing, ListingChange, is_hidden_entry, is_hidden_name, is_system_clutter,
    },
    helper::{DataHolder, canonical_form, normalize_path},
};
pub static COLLATER: std::sync::LazyLock<CollatorBorrowed<'static>> =
    std::sync::LazyLock::new(|| build_collator(false));

use super::dock::TabData;

/// Inputs that determine a tab's watcher specs, stored alongside the cached
/// specs so a recompute is triggered only when these change (not every frame).
#[derive(Clone, Debug, PartialEq, Eq)]
struct SpecsInputs {
    roots: Vec<PathBuf>,
    is_recursive: bool,
    depth: usize,
    linked: Vec<PathBuf>,
}

/// Symlinked directories that the last deep walk followed, together with the
/// roots and depth it walked, so they are only used for the same view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkedWatchRoots {
    roots: Vec<PathBuf>,
    depth: usize,
    dirs: Vec<PathBuf>,
}

/// Result of [`read_directory_streaming`].
pub struct WalkResult {
    pub listing: Arc<Listing>,
    /// Canonical targets of the directory symlinks the walk descended into.
    /// A recursive watch on the root does not follow them, so each needs its
    /// own watch. Empty for flat reads.
    pub linked_dirs: Vec<PathBuf>,
}

/// Memoisation store for `TabData::watcher_specs`, so the change-detection
/// path (`should_refresh_for_directories`) does not rebuild and re-normalise
/// the specs for every watcher batch.
#[derive(Debug, Default)]
pub struct WatcherSpecsCache {
    specs: Vec<(PathBuf, RecursiveMode)>,
    inputs: Option<SpecsInputs>,
}

fn resolve_path_setting<T, F, R>(path: &CurrentPath, data_source: &impl DataHolder, extract: F) -> R
where
    T: 'static
        + Clone
        + Default
        + std::any::Any
        + egui::util::id_type_map::SerializableAny
        + Send
        + Sync,
    F: Fn(crate::app::Data<T>) -> R,
    R: Default,
{
    match path {
        CurrentPath::One(single) => {
            extract(data_source.data_get_path_or_persisted::<T>(&CurrentPath::One(single.clone())))
        }
        CurrentPath::Multiple(paths) => {
            for p in paths {
                let data =
                    data_source.data_get_path_or_persisted::<T>(&CurrentPath::One(p.clone()));
                if data.is_local() {
                    return extract(data);
                }
            }
            extract(data_source.data_get_path_or_persisted::<T>(path))
        }
        CurrentPath::Trash => extract(data_source.data_get_path_or_persisted::<T>(path)),
        CurrentPath::None => extract(Data::default()),
    }
}

impl TabData {
    pub fn watcher_specs(&mut self) -> Vec<(PathBuf, RecursiveMode)> {
        // Symlinked roots come from the deep walk (`set_linked_watch_roots`),
        // so the specs are a pure function of the tab state and never walk
        // the filesystem here.
        self.ensure_watcher_specs_cached();
        self.watcher_specs_cache.specs.clone()
    }

    /// Record the directory symlinks the latest walk followed. Returns whether
    /// they changed, i.e. whether the tab's watchers need reconciling.
    pub fn set_linked_watch_roots(&mut self, dirs: Vec<PathBuf>) -> bool {
        let next = LinkedWatchRoots {
            roots: self.specs_inputs().roots,
            depth: self.search_depth(),
            dirs,
        };
        if next == self.linked_watch_roots {
            return false;
        }
        self.linked_watch_roots = next;
        true
    }

    /// Recompute the cached watcher specs only if the spec-relevant inputs
    /// (`current_path`, search depth, `extra_dirs`) have changed since the last
    /// compute. This is the cheap path used by the per-frame change handler and
    /// avoids re-normalising every root in `compute_watcher_specs`.
    fn ensure_watcher_specs_cached(&mut self) {
        let inputs = self.specs_inputs();
        let needs_recompute = self.watcher_specs_cache.inputs.as_ref() != Some(&inputs);
        if needs_recompute {
            self.watcher_specs_cache.specs = self.compute_watcher_specs();
            self.watcher_specs_cache.inputs = Some(inputs);
        }
    }

    fn specs_inputs(&self) -> SpecsInputs {
        let is_recursive = self.search.as_ref().is_some_and(|s| s.depth > 1);
        let mut roots = Vec::new();
        match &self.current_path {
            CurrentPath::None | CurrentPath::Trash => {}
            CurrentPath::One(p) => roots.push(p.clone()),
            CurrentPath::Multiple(ps) => roots.extend(ps.iter().cloned()),
        }
        if let Some(s) = &self.search {
            roots.extend(s.extra_dirs().cloned());
        }
        SpecsInputs {
            roots,
            is_recursive,
            depth: self.search_depth(),
            linked: self.linked_watch_roots.dirs.clone(),
        }
    }

    fn compute_watcher_specs(&self) -> Vec<(PathBuf, RecursiveMode)> {
        // Gather every root this view covers: the current path(s) plus any extra
        // search directories.
        let mut roots: Vec<PathBuf> = self.current_path.get_paths();
        if let Some(search) = &self.search {
            roots.extend(search.extra_dirs().cloned());
        }

        // A recursive watch does not follow directory symlinks, so each one the
        // deep walk descended into gets its own watch (see `WalkResult`).
        //
        // Recursive watching is only used for a SINGLE root. Multi-root views (favorites, multi-dir searches)
        // watch each root non-recursively instead: a change directly inside a
        // root still triggers a refresh, while deeper changes require a manual
        // re-search — an acceptable trade-off for not stalling the UI.
        //
        // The listing depth itself is NOT affected: `read_directory` still
        // walks every root to the full requested depth; only the live-update
        // watch is bounded.
        let single_root = roots.len() <= 1;
        let depth = self.search_depth();
        let recursive = single_root && depth > 1;
        let mode = if recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };

        let mut specs = BTreeSet::new();
        for root in &roots {
            specs.insert((normalize_path(root), mode));
        }
        let linked = &self.linked_watch_roots;
        if recursive && linked.depth == depth && linked.roots == roots {
            for dir in &linked.dirs {
                specs.insert((dir.clone(), mode));
            }
        }
        specs.into_iter().collect()
    }

    /// How deep to scan. Subfolders are only walked while there is a query,
    /// so an empty search box shows the folder itself, not its whole tree.
    pub fn search_depth(&self) -> usize {
        if !self.is_searching() {
            return 1;
        }
        self.search.as_ref().map_or(1, |search| search.depth.max(1))
    }

    pub fn should_refresh_for_directories(
        &mut self,
        changed_directories: &BTreeSet<PathBuf>,
    ) -> bool {
        self.ensure_watcher_specs_cached();
        // Paths in the cached specs were normalised at compute time, and
        // `changed_directories` arrive already normalised from the watcher, so
        // this hot path performs no `canonicalize` syscalls.
        self.watcher_specs_cache.specs.iter().any(|(root, mode)| {
            changed_directories.iter().any(|changed| match mode {
                RecursiveMode::Recursive => changed.starts_with(root),
                RecursiveMode::NonRecursive => changed == root,
            })
        })
    }

    pub fn set_path(&mut self, path: impl Into<CurrentPath>) -> &CurrentPath {
        let path = path.into();
        // Back / Forward already saved where you were and set what to restore.
        let restoring = self.view_restoring();
        if !restoring && path != self.current_path {
            let place = self.current_place();
            self.history.save_place(place);
            // Going up (or to any folder above), select the folder you came
            // from, as Explorer does.
            if let (Some(old), CurrentPath::One(new)) = (self.current_path.single_path(), &path)
                && let Ok(rest) = old.strip_prefix(new)
                && let Some(child) = rest.components().next()
            {
                self.pending_select = Some(crate::app::file_ops::PendingSelect {
                    paths: vec![new.join(child)],
                    rename: false,
                    since: std::time::Instant::now(),
                });
            }
        }
        self.history.visit(crate::app::history::HistoryEntry::new(
            path.clone(),
            self.search.clone(),
        ));
        self.current_path = path;
        if let Some(path) = self.current_path.get_path() {
            self.top_display_path.build(&path);
        } else if self.current_path.is_trash() {
            self.top_display_path = crate::app::top_bottom::TopDisplayPath::default();
        }
        &self.current_path
    }

    pub fn update_settings(&mut self, data_source: &impl DataHolder) {
        self.show_hidden = resolve_path_setting::<DirectoryShowHidden, _, _>(
            &self.current_path,
            data_source,
            |d: Data<DirectoryShowHidden>| d.data.0,
        );
        self.display_type = resolve_path_setting::<DirectoryViewSettings, _, _>(
            &self.current_path,
            data_source,
            |d: Data<DirectoryViewSettings>| d.data.display_type,
        );
        if let Some(path) = self.current_path.get_path() {
            self.top_display_path.build(&path);
        }
    }

    pub fn update_visible_entries(&mut self) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!(
            "dir_handling::update_visible_entries",
            self.total_entry_count().to_string()
        );
        let key = FilterKey::new(
            &self.listing,
            self.order_generation,
            self.show_hidden,
            self.search.as_ref(),
        );
        // Typing extends the query: names matching the longer query are a
        // subset of the previous matches, so only those need re-checking.
        let narrowing = key
            .as_ref()
            .zip(self.filter_key.as_ref())
            .is_some_and(|(next, previous)| next.narrows(previous));
        let visible = if narrowing {
            filter_visible(
                &self.listing,
                &self.visible_entries,
                self.show_hidden,
                self.search.as_ref(),
            )
        } else {
            filter_visible(
                &self.listing,
                &self.order,
                self.show_hidden,
                self.search.as_ref(),
            )
        };
        self.visible_entries = visible;
        self.filter_key = key;
    }

    pub fn sort_entries(&mut self, sort_settings: &DirectoryViewSettings) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::sort_entries");
        self.display_type = sort_settings.display_type;
        self.order = sort_order(&self.listing, sort_settings);
        self.order_generation = self.order_generation.wrapping_add(1);
    }

    /// Install a freshly loaded listing together with its precomputed sort
    /// order and visible rows.
    pub fn set_listing(&mut self, listing: Arc<Listing>, order: Vec<u32>, visible: Vec<u32>) {
        self.listing = listing;
        self.order = order;
        self.visible_entries = visible;
        self.order_generation = self.order_generation.wrapping_add(1);
        self.filter_key = None;
    }

    /// Show `entries` in the given order, unfiltered (tests and fixtures).
    #[cfg(test)]
    pub fn set_entries(&mut self, entries: &[crate::data::files::DirEntry]) {
        let listing = Arc::new(Listing::from_entries(entries));
        let order: Vec<u32> = (0..listing.len() as u32).collect();
        let visible = order.clone();
        self.set_listing(listing, order, visible);
    }

    #[cfg(test)]
    pub fn update_file_metadata(
        &mut self,
        path: &Path,
        sort_settings: &DirectoryViewSettings,
    ) -> bool {
        self.apply_listing_changes(
            &[crate::watcher::ResolvedChange::from_disk(
                path.to_path_buf(),
                false,
            )],
            sort_settings,
        ) == Some(true)
    }

    /// Apply a coalesced watcher batch to a single-directory listing. Returns
    /// `false` when the batch cannot be applied surgically and the caller
    /// should fall back to a structural refresh.
    pub fn apply_file_changes(
        &mut self,
        changes: &[crate::watcher::ResolvedChange],
        sort_settings: &DirectoryViewSettings,
    ) -> bool {
        self.apply_listing_changes(changes, sort_settings).is_some()
    }

    /// `None`: not applicable (refresh instead). `Some(changed)` otherwise.
    /// Metadata was read on the watcher thread, so this never touches disk.
    fn apply_listing_changes(
        &mut self,
        changes: &[crate::watcher::ResolvedChange],
        sort_settings: &DirectoryViewSettings,
    ) -> Option<bool> {
        let listing_dir = canonical_form(Path::new(self.listing.single_dir()?.as_ref()));
        let mut listing_changes: Vec<ListingChange<'_>> = Vec::with_capacity(changes.len());
        for change in changes {
            if change.path.parent().map(canonical_form).as_ref() != Some(&listing_dir) {
                return None;
            }
            listing_changes.push(change.listing_change()?);
        }
        let Some(next) = self.listing.with_changes(&listing_changes) else {
            return Some(false);
        };
        let structural = changes
            .iter()
            .any(|change| !matches!(change.kind, crate::watcher::ChangeKind::Update(_)));
        self.listing = Arc::new(next);
        // Metadata-only edits keep entry indices stable, so a name (or random)
        // order and the filter results remain valid as they are.
        if structural || !matches!(sort_settings.sorting, Sort::Name | Sort::Random) {
            self.sort_entries(sort_settings);
            self.update_visible_entries();
        }
        Some(true)
    }

    #[cfg(test)]
    pub fn insert_file_entry(
        &mut self,
        path: &Path,
        sort_settings: &DirectoryViewSettings,
    ) -> bool {
        self.apply_listing_changes(
            &[crate::watcher::ResolvedChange::from_disk(
                path.to_path_buf(),
                true,
            )],
            sort_settings,
        ) == Some(true)
    }

    #[cfg(test)]
    pub fn remove_file_entry(
        &mut self,
        path: &Path,
        sort_settings: &DirectoryViewSettings,
    ) -> bool {
        self.apply_listing_changes(
            &[crate::watcher::ResolvedChange {
                path: path.to_path_buf(),
                kind: crate::watcher::ChangeKind::Remove,
            }],
            sort_settings,
        ) == Some(true)
    }

    pub fn deep_or_multiple_paths(&self) -> bool {
        // The Recycle Bin shows where each item was deleted from.
        self.current_path.multiple_paths()
            || self.current_path.is_trash()
            || self.search.as_ref().map_or(1, |search| search.depth) > 1
            || self
                .search
                .as_ref()
                .is_some_and(|s| s.extra_dirs().next().is_some())
    }
}

/// How often a deep walk publishes partial results to the UI.
const PARTIAL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(300);

/// Shared state of one parallel recursive walk.
struct Walk<'a> {
    max_depth: usize,
    show_hidden: bool,
    cancel: &'a AtomicBool,
    /// One listing per visited directory; concatenated at the end.
    parts: std::sync::Mutex<Vec<Listing>>,
    entries: std::sync::atomic::AtomicUsize,
    /// Canonical targets of followed directory symlinks (loop protection).
    followed: std::sync::Mutex<foldhash::HashSet<PathBuf>>,
    last_partial: std::sync::Mutex<std::time::Instant>,
    on_partial: &'a (dyn Fn(Arc<Listing>) + Sync),
}

impl Walk<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.load(AtomicOrdering::Relaxed)
    }

    /// List `dir` (whose entries sit at `depth`) and recurse into its
    /// sub-directories in parallel. Entry metadata, attributes and link-ness
    /// all come from the bulk directory read; the only extra syscall is the
    /// `canonicalize` that guards against symlink loops.
    fn visit<'scope>(&'scope self, scope: &rayon::Scope<'scope>, dir: &Path, depth: usize) {
        if self.cancelled() {
            return;
        }
        let dir_name: Arc<str> = Arc::from(crate::helper::canonical_string(dir));
        let mut builder = crate::data::files::ListingBuilder::with_capacity(64);
        let dir_index = builder.intern_dir(&dir_name);
        let pushed = std::cell::Cell::new(0usize);
        let mut sink = |entry: &crate::fs_enum::RawEntry<'_>| {
            if !self.show_hidden
                && (entry.is_hidden_attr()
                    || entry.name.starts_with_ascii(b'.')
                    || (cfg!(windows) && entry.name.starts_with_ascii(b'$')))
            {
                return;
            }
            let meta = entry.metadata();
            if meta.entry_type == EntryType::Directory && depth < self.max_depth {
                let child = dir.join(entry.name.to_os_string());
                if self.should_descend(&child, entry.is_symlink()) {
                    scope.spawn(move |scope| self.visit(scope, &child, depth + 1));
                }
            }
            builder.push_raw(dir_index, &entry.name, meta);
            pushed.set(pushed.get() + 1);
        };
        // Fall back to `read_dir` only when the bulk read produced nothing,
        // so a mid-way failure never lists a directory twice.
        if crate::fs_enum::enumerate(dir, &mut sink).is_err()
            && pushed.get() == 0
            && crate::fs_enum::enumerate_std(dir, &mut sink).is_err()
        {
            return;
        }
        drop(sink);
        let listing = builder.finish();
        if listing.is_empty() {
            return;
        }
        self.entries
            .fetch_add(listing.len(), AtomicOrdering::Relaxed);
        if let Ok(mut parts) = self.parts.lock() {
            parts.push(listing);
        }
        self.maybe_publish();
    }

    fn should_descend(&self, path: &Path, is_symlink: bool) -> bool {
        if !is_symlink {
            return true;
        }
        let Ok(target) = std::fs::canonicalize(path) else {
            return false;
        };
        self.followed
            .lock()
            .is_ok_and(|mut followed| followed.insert(target))
    }

    fn maybe_publish(&self) {
        let Ok(mut last) = self.last_partial.try_lock() else {
            return;
        };
        if last.elapsed() < PARTIAL_INTERVAL {
            return;
        }
        *last = std::time::Instant::now();
        drop(last);
        let snapshot = self.snapshot();
        (self.on_partial)(Arc::new(snapshot));
    }

    fn snapshot(&self) -> Listing {
        self.parts
            .lock()
            .map(|parts| Listing::concat(parts.iter()))
            .unwrap_or_default()
    }
}

/// Read every root (one level for `depth <= 1`, recursively otherwise) into a
/// single listing. A single flat root returns the cached listing itself, with
/// no copying.
#[cfg(test)]
pub fn read_directory(
    paths: &[PathBuf],
    depth: usize,
    show_hidden: bool,
    cancel: &AtomicBool,
) -> Arc<Listing> {
    read_directory_streaming(paths, depth, show_hidden, cancel, &|_| {}).listing
}

/// [`read_directory`] that also hands partial results of deep walks to
/// `on_partial` every [`PARTIAL_INTERVAL`], so large searches fill in
/// progressively.
pub fn read_directory_streaming(
    paths: &[PathBuf],
    depth: usize,
    show_hidden: bool,
    cancel: &AtomicBool,
    on_partial: &(dyn Fn(Arc<Listing>) + Sync),
) -> WalkResult {
    #[cfg(feature = "profiling")]
    puffin::profile_scope!("lwa_fm::dir_handling::read_directory");

    // Normalise each root once; entry paths below it are then derived
    // lexically instead of paying a `canonicalize` per entry.
    let mut roots: Vec<PathBuf> = paths.iter().map(|path| normalize_path(path)).collect();
    roots.sort();
    roots.dedup();

    if depth <= 1 {
        let mut parts: Vec<Arc<Listing>> = roots
            .par_iter()
            .filter_map(|root| database::read_dir(root))
            .collect();
        let listing = match parts.len() {
            0 => Arc::default(),
            1 => parts.pop().unwrap_or_default(),
            _ => Arc::new(Listing::concat(parts.iter().map(AsRef::as_ref))),
        };
        return WalkResult {
            listing,
            linked_dirs: Vec::new(),
        };
    }

    // A root nested inside another root is already covered by its walk.
    let roots: Vec<PathBuf> = roots
        .iter()
        .filter(|root| {
            !roots
                .iter()
                .any(|other| other != *root && root.starts_with(other))
        })
        .cloned()
        .collect();

    let walk = Walk {
        // `depth` counts levels below the root, matching the former
        // `walkdir` `max_depth(depth + 1)`.
        max_depth: depth + 1,
        show_hidden,
        cancel,
        parts: std::sync::Mutex::new(Vec::new()),
        entries: std::sync::atomic::AtomicUsize::new(0),
        followed: std::sync::Mutex::new(foldhash::HashSet::default()),
        last_partial: std::sync::Mutex::new(std::time::Instant::now()),
        on_partial,
    };
    rayon::scope(|scope| {
        for root in &roots {
            let walk = &walk;
            let root = root.clone();
            scope.spawn(move |scope| walk.visit(scope, &root, 1));
        }
    });
    if walk.cancelled() {
        return WalkResult {
            listing: Arc::default(),
            linked_dirs: Vec::new(),
        };
    }
    let listing = Arc::new(walk.snapshot());
    let mut linked_dirs: Vec<PathBuf> = walk
        .followed
        .into_inner()
        .unwrap_or_default()
        .iter()
        .map(|target| canonical_form(target))
        .collect();
    linked_dirs.sort();
    linked_dirs.dedup();
    WalkResult {
        listing,
        linked_dirs,
    }
}

/// A key per entry of `listing` from its open history record (if any) and
/// metadata. History is looked up once per folder, then by name.
fn opening_keys(
    listing: &Listing,
    key: impl Fn(Option<open_history::OpenRecord>, &DirEntryMetaData) -> (u32, u32),
) -> Vec<(u32, u32)> {
    let by_dir: Vec<_> = if open_history::is_empty() {
        Vec::new()
    } else {
        listing
            .dirs()
            .iter()
            .map(|dir| open_history::records_in(Path::new(dir.as_ref())))
            .collect()
    };
    (0..listing.len())
        .map(|index| {
            let record = by_dir
                .get(listing.dir_index(index))
                .and_then(Option::as_ref)
                .and_then(|names| names.get(listing.name(index)).copied());
            key(record, listing.meta(index))
        })
        .collect()
}

/// Folders first in both directions, like the other sorts.
fn sort_by_opening(
    order: &mut [u32],
    keys: &[(u32, u32)],
    is_file: &(impl Fn(u32) -> bool + Sync),
    invert: bool,
) {
    use std::cmp::Reverse;
    if invert {
        order.par_sort_unstable_by_key(|&i| (is_file(i), Reverse(keys[i as usize])));
    } else {
        order.par_sort_unstable_by_key(|&i| (is_file(i), keys[i as usize]));
    }
}

/// Compute the display order (indices into `listing`) for `settings`.
pub fn sort_order(listing: &Listing, settings: &DirectoryViewSettings) -> Vec<u32> {
    use std::cmp::Reverse;
    let mut order: Vec<u32> = (0..listing.len() as u32).collect();
    let is_file = |index: u32| listing.is_file(index as usize);
    let meta = |index: u32| listing.meta(index as usize);
    match (settings.sorting, settings.invert_sort) {
        (Sort::Name, invert) => {
            let keys = listing.sort_keys();
            // Folders stay first in both directions; only names reverse.
            order.par_sort_unstable_by(|a, b| {
                let names = keys.get(*a as usize).cmp(keys.get(*b as usize));
                is_file(*a)
                    .cmp(&is_file(*b))
                    .then(if invert { names.reverse() } else { names })
            });
        }
        (Sort::Modified, false) => {
            order.par_sort_unstable_by_key(|&i| (is_file(i), meta(i).modified_at));
        }
        (Sort::Modified, true) => {
            order.par_sort_unstable_by_key(|&i| (is_file(i), Reverse(meta(i).modified_at)));
        }
        (Sort::Created, false) => {
            order.par_sort_unstable_by_key(|&i| (is_file(i), meta(i).created_at));
        }
        (Sort::Created, true) => {
            order.par_sort_unstable_by_key(|&i| (is_file(i), Reverse(meta(i).created_at)));
        }
        (Sort::Accessed, invert) => {
            let opened = opening_keys(listing, |record, meta| {
                (0, *open_history::last_opened(record, meta.accessed_at))
            });
            sort_by_opening(&mut order, &opened, &is_file, invert);
        }
        (Sort::MostOpened, invert) => {
            // Ties (most files: never opened here) fall back to recency.
            let opened = opening_keys(listing, |record, meta| {
                (
                    record.map_or(0, |record| record.count),
                    *open_history::last_opened(record, meta.accessed_at),
                )
            });
            sort_by_opening(&mut order, &opened, &is_file, invert);
        }
        (Sort::Size, false) => {
            order.par_sort_unstable_by_key(|&i| (is_file(i), meta(i).size));
        }
        (Sort::Size, true) => {
            order.par_sort_unstable_by_key(|&i| (is_file(i), Reverse(meta(i).size)));
        }
        (Sort::Kind, invert) => {
            // By extension, then name; folders first in both directions.
            let keys = listing.sort_keys();
            let kinds: Vec<String> = (0..listing.len())
                .map(|index| {
                    crate::app::dock::kind_label(
                        listing.name(index),
                        listing.meta(index).entry_type,
                    )
                })
                .collect();
            order.par_sort_unstable_by(|a, b| {
                let (a, b) = (*a as usize, *b as usize);
                let kind = kinds[a].cmp(&kinds[b]);
                listing.is_file(a).cmp(&listing.is_file(b)).then(
                    (if invert { kind.reverse() } else { kind })
                        .then_with(|| keys.get(a).cmp(keys.get(b))),
                )
            });
        }
        (Sort::Random, _) => {
            use rand::seq::SliceRandom;
            order.shuffle(&mut rand::thread_rng());
        }
    }
    order
}

/// Snapshot of the inputs of the last filter run, used to detect when a new
/// query only narrows the previous one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterKey {
    listing: usize,
    order_generation: u64,
    show_hidden: bool,
    case_sensitive: bool,
    /// Present only for a single plain term (the typing case).
    plain: Option<String>,
}

impl FilterKey {
    fn new(
        listing: &Arc<Listing>,
        order_generation: u64,
        show_hidden: bool,
        search: Option<&Search>,
    ) -> Option<Self> {
        let (case_sensitive, plain) = match search {
            None => (false, None),
            Some(s) if s.simple_text().is_some() || !s.has_query() => {
                let text = s.simple_text().unwrap_or_default();
                let value = if s.case_sensitive {
                    text.to_owned()
                } else {
                    text.to_lowercase()
                };
                (s.case_sensitive, Some(value))
            }
            Some(_) => return None,
        };
        Some(Self {
            listing: Arc::as_ptr(listing) as usize,
            order_generation,
            show_hidden,
            case_sensitive,
            plain,
        })
    }

    fn narrows(&self, previous: &Self) -> bool {
        self.listing == previous.listing
            && self.order_generation == previous.order_generation
            && self.show_hidden == previous.show_hidden
            && self.case_sensitive == previous.case_sensitive
            && match (&self.plain, &previous.plain) {
                (Some(next), Some(prev)) => next.len() > prev.len() && next.contains(prev.as_str()),
                _ => false,
            }
    }
}

const PARALLEL_FILTER_THRESHOLD: usize = 16 * 1024;

/// Indices from `candidates` (in their order) that pass the hidden-file and
/// search filters.
pub fn filter_visible(
    listing: &Listing,
    candidates: &[u32],
    show_hidden: bool,
    search: Option<&Search>,
) -> Vec<u32> {
    let compiled = crate::app::search::filter::CompiledQuery::new(search);
    let folded = compiled
        .as_ref()
        .filter(|query| query.needs_folded)
        .map(|_| listing.folded_names());
    let keep = |&index: &u32| -> bool {
        let index = index as usize;
        let name = listing.name(index);
        if is_system_clutter(name) || (!show_hidden && is_hidden_entry(name, listing.meta(index))) {
            return false;
        }
        compiled
            .as_ref()
            .is_none_or(|cs| cs.matches(listing, folded, index))
    };
    if candidates.len() < PARALLEL_FILTER_THRESHOLD {
        return candidates.iter().copied().filter(|i| keep(i)).collect();
    }
    let chunks: Vec<Vec<u32>> = candidates
        .par_chunks(4096)
        .map(|chunk| chunk.iter().copied().filter(|i| keep(i)).collect())
        .collect();
    chunks.concat()
}

pub fn get_directories(path: &Path, show_hidden: bool) -> BTreeSet<Cow<'static, str>> {
    // One level: going deeper reads every child folder at once, which on
    // macOS asks for Desktop, Documents and Downloads access together.
    get_directories_recursive(path, show_hidden, 1)
}

/// Immediate sub-directories of `path` as `(name, path)`, sorted by path.
/// Uses the directory listing's file type only (no per-entry `metadata`).
pub fn child_directories(path: &Path, show_hidden: bool) -> Vec<(String, PathBuf)> {
    type Reader = fn(&Path, &mut dyn FnMut(&crate::fs_enum::RawEntry<'_>)) -> std::io::Result<()>;
    let collect = |read: Reader| {
        let mut children: Vec<(String, PathBuf)> = Vec::new();
        read(path, &mut |entry| {
            if !entry.is_dir() || entry.is_package() {
                return;
            }
            let name = entry.name.to_string_lossy().into_owned();
            if show_hidden || !is_hidden_entry(&name, &entry.metadata()) {
                let child = path.join(&name);
                children.push((name, child));
            }
        })
        .map(|()| children)
    };
    let mut children = collect(crate::fs_enum::enumerate)
        .or_else(|_| collect(crate::fs_enum::enumerate_std))
        .unwrap_or_default();
    children.sort_by(|a, b| a.1.cmp(&b.1));
    children
}

pub fn get_directories_recursive(
    path: &Path,
    show_hidden: bool,
    depth: usize,
) -> BTreeSet<Cow<'static, str>> {
    walkdir::WalkDir::new(path)
        .max_depth(depth)
        .into_iter()
        // Packages are single items: neither suggested nor walked into.
        .filter_entry(|e| {
            e.depth() == 0 || !e.file_type().is_dir() || !crate::fs_enum::is_package(e.path())
        })
        .filter_map(Result::ok)
        .filter(|e| {
            if !e.file_type().is_dir() {
                return false;
            }
            if show_hidden {
                return true;
            }
            if is_hidden_name(&e.file_name().to_string_lossy()) {
                return false;
            }
            let mut current_path: Option<&Path> = e.path().parent();

            while let Some(parent) = current_path {
                if let Some(parent_name) = parent.file_name().and_then(|name| name.to_str())
                    && is_hidden_name(parent_name)
                    && !parent.eq(path)
                {
                    return false;
                }
                current_path = parent.parent(); // Move to the next parent
            }
            true
        })
        .map(|e| format!("{}", e.path().display()).into())
        .collect::<BTreeSet<_>>()
}

#[cfg(test)]
mod tests {
    use crate::app::dock::TabData;
    use crate::data::files::{DirEntry, Listing};

    use super::{Search, filter_visible, sort_order};

    fn identity(listing: &Listing) -> Vec<u32> {
        (0..listing.len() as u32).collect()
    }

    #[test]
    fn filter_visible_includes_all_entries_uniformly() {
        let listing = Listing::from_entries(&[
            DirEntry::test_new("/fav/file_report.txt"),
            DirEntry::test_new("/fav/subdir/report.txt"),
            DirEntry::test_new("/fav/notes.txt"),
            DirEntry::test_new("/fav/subdir/deep/report.txt"),
        ]);
        let all = identity(&listing);

        let visible = filter_visible(&listing, &all, true, None);
        assert_eq!(visible.len(), 4, "no search should include all entries");

        let search = Search {
            value: "report".to_string(),
            depth: 2,
            ..Default::default()
        };
        assert_eq!(
            filter_visible(&listing, &all, true, Some(&search)),
            [0, 1, 3]
        );

        let search = Search {
            value: "notes".to_string(),
            depth: 2,
            ..Default::default()
        };
        assert_eq!(filter_visible(&listing, &all, true, Some(&search)), [2]);
    }

    #[test]
    fn filter_visible_hides_hidden_files() {
        let listing = Listing::from_entries(&[
            DirEntry::test_new("/fav/.hidden.txt"),
            DirEntry::test_new("/fav/visible.txt"),
            DirEntry::test_new("/fav/subdir/.hidden.txt"),
        ]);
        assert_eq!(
            filter_visible(&listing, &identity(&listing), false, None),
            [1]
        );
    }

    #[test]
    fn filter_visible_hides_attribute_hidden_entries() {
        let mut hidden = DirEntry::test_new("/fav/desktop.ini");
        hidden.meta.flags = crate::data::files::EntryFlags::SYSTEM;
        let listing = Listing::from_entries(&[hidden, DirEntry::test_new("/fav/normal.txt")]);
        let all = identity(&listing);
        assert_eq!(filter_visible(&listing, &all, false, None), [1]);
        assert_eq!(filter_visible(&listing, &all, true, None), [0, 1]);
    }

    #[cfg(windows)]
    #[test]
    fn deep_walk_and_children_skip_attribute_hidden_dirs() {
        let root = unique_test_dir("attr_hidden");
        std::fs::create_dir_all(root.join("secret/inner")).expect("create tree");
        std::fs::create_dir_all(root.join("open")).expect("create open dir");
        std::fs::write(root.join("secret/inner/x.txt"), b"x").expect("write");
        let status = std::process::Command::new("attrib")
            .arg("+h")
            .arg(root.join("secret"))
            .status()
            .expect("attrib");
        assert!(status.success());
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let names = |hidden: bool| {
            let listing = super::read_directory(std::slice::from_ref(&root), 4, hidden, &cancel);
            let mut names: Vec<String> = listing
                .iter()
                .map(|entry| entry.file_name.to_owned())
                .collect();
            names.sort();
            names
        };
        assert_eq!(names(false), ["open"]);
        assert_eq!(names(true), ["inner", "open", "secret", "x.txt"]);
        let children: Vec<String> = super::child_directories(&root, false)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(children, ["open"]);
        assert_eq!(super::child_directories(&root, true).len(), 2);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn filter_is_case_insensitive_including_unicode() {
        let listing = Listing::from_entries(&[
            DirEntry::test_new("/fav/file_report.txt"),
            DirEntry::test_new("/fav/.hidden_report.txt"),
            DirEntry::test_new("/fav/notes.txt"),
            DirEntry::test_new("/fav/REPORT_final.txt"),
            DirEntry::test_new("/fav/ŻÓŁW.txt"),
        ]);
        let all = identity(&listing);
        let search = |value: &str| Search {
            value: value.to_string(),
            ..Default::default()
        };
        assert_eq!(
            filter_visible(&listing, &all, false, Some(&search("report"))),
            [0, 3]
        );
        assert_eq!(
            filter_visible(&listing, &all, false, Some(&search("żółw"))),
            [4]
        );
        let sensitive = Search {
            case_sensitive: true,
            ..search("REPORT")
        };
        assert_eq!(filter_visible(&listing, &all, false, Some(&sensitive)), [3]);
    }

    #[test]
    fn filter_preserves_candidate_order_in_parallel_path() {
        let entries: Vec<DirEntry> = (0..40_000)
            .map(|index| DirEntry::test_new(&format!("/d/{}_{index}", index % 3)))
            .collect();
        let listing = Listing::from_entries(&entries);
        let reversed: Vec<u32> = identity(&listing).into_iter().rev().collect();
        let search = Search {
            value: "1_".to_string(),
            ..Default::default()
        };
        let visible = filter_visible(&listing, &reversed, false, Some(&search));
        assert_eq!(visible.len(), 13_333);
        assert!(visible.windows(2).all(|pair| pair[0] > pair[1]));
    }

    #[test]
    fn narrowing_search_reuses_previous_matches() {
        let mut tab = TabData::from_path(std::path::Path::new("/virtual"));
        tab.set_entries(&[
            DirEntry::test_new("/v/report_a.txt"),
            DirEntry::test_new("/v/notes.txt"),
            DirEntry::test_new("/v/report_b.txt"),
        ]);
        tab.search = Some(Search {
            value: "rep".to_string(),
            ..Default::default()
        });
        tab.update_visible_entries();
        assert_eq!(tab.visible_entries, [0, 2]);
        tab.search.as_mut().expect("search").value = "report_b".to_string();
        tab.update_visible_entries();
        assert_eq!(tab.visible_entries, [2]);
        // Widening must re-scan the full order, not the narrowed subset.
        tab.search.as_mut().expect("search").value = "o".to_string();
        tab.update_visible_entries();
        assert_eq!(tab.visible_entries, [0, 1, 2]);
    }

    fn unique_test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lwa_fm_{name}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time before unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        crate::helper::normalize_path(&dir)
    }

    #[test]
    fn update_file_metadata_updates_entry_without_refresh() {
        let dir = unique_test_dir("lazy_meta");
        let file = dir.join("hot.log");
        std::fs::write(&file, b"abcdef").expect("write temp file");

        let mut tab = TabData::from_path(&dir);
        tab.set_entries(&[DirEntry::test_new(&file.to_string_lossy())]);

        let settings = super::super::directory_view_settings::DirectoryViewSettings::default();
        assert!(tab.update_file_metadata(&file, &settings));
        assert_eq!(tab.listing.meta(0).size, 6);
        assert_eq!(tab.visible_entries, [0]);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn insert_file_entry_adds_new_entry() {
        let dir = unique_test_dir("insert_new");
        let existing = dir.join("a.txt");
        std::fs::write(&existing, b"a").expect("write existing file");
        let new_file = dir.join("b.txt");
        std::fs::write(&new_file, b"hello").expect("write new file");

        let mut tab = TabData::from_path(&dir);
        tab.set_entries(&[DirEntry::test_new(&existing.to_string_lossy())]);

        let settings = super::super::directory_view_settings::DirectoryViewSettings::default();
        assert!(tab.insert_file_entry(&new_file, &settings));

        assert_eq!(tab.listing.len(), 2, "entry should be inserted");
        let inserted = tab
            .listing
            .iter()
            .find(|e| e.file_name == "b.txt")
            .expect("inserted entry present");
        assert_eq!(inserted.meta.size, 5);
        assert_eq!(tab.visible_entries.len(), 2);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn insert_file_entry_replaces_same_name_entry() {
        let dir = unique_test_dir("insert_replace");
        let file = dir.join("hot.log");
        std::fs::write(&file, b"old").expect("write initial file");

        let mut tab = TabData::from_path(&dir);
        tab.set_entries(&[DirEntry::test_new(&file.to_string_lossy())]);
        std::fs::write(&file, b"new-contents").expect("rewrite file");

        let settings = super::super::directory_view_settings::DirectoryViewSettings::default();
        assert!(tab.insert_file_entry(&file, &settings));
        assert_eq!(tab.listing.len(), 1, "no duplicate created");
        assert_eq!(tab.listing.meta(0).size, 12, "metadata refreshed");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn insert_file_entry_returns_false_for_wrong_parent() {
        let dir = unique_test_dir("insert_wrong");
        let other = unique_test_dir("insert_other");
        let file = other.join("stray.txt");
        std::fs::write(&file, b"x").expect("write file");

        let mut tab = TabData::from_path(&dir);
        tab.set_entries(&[DirEntry::test_new(&dir.join("a.txt").to_string_lossy())]);

        let settings = super::super::directory_view_settings::DirectoryViewSettings::default();
        assert!(
            !tab.insert_file_entry(&file, &settings),
            "different parent should not apply"
        );

        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(other);
    }

    #[test]
    fn remove_file_entry_drops_entry() {
        let dir = unique_test_dir("remove_entry");
        let keep = dir.join("keep.txt");
        let gone = dir.join("gone.txt");

        let mut tab = TabData::from_path(&dir);
        tab.set_entries(&[
            DirEntry::test_new(&keep.to_string_lossy()),
            DirEntry::test_new(&gone.to_string_lossy()),
        ]);

        let settings = super::super::directory_view_settings::DirectoryViewSettings::default();
        assert!(tab.remove_file_entry(&gone, &settings));
        assert_eq!(tab.listing.len(), 1, "entry removed");
        assert_eq!(tab.listing.name(0), "keep.txt");
        assert_eq!(tab.visible_entries.len(), 1);

        assert!(
            !tab.remove_file_entry(&dir.join("nope.txt"), &settings),
            "absent entry should not apply"
        );
        assert_eq!(tab.listing.len(), 1, "listing unchanged");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn deep_walk_respects_depth_hidden_and_nested_roots() {
        let root = unique_test_dir("deep_walk");
        std::fs::create_dir_all(root.join("a/b/c")).expect("create tree");
        std::fs::create_dir_all(root.join(".hidden")).expect("create hidden dir");
        for file in [
            "top.txt",
            "a/one.txt",
            "a/b/two.txt",
            "a/b/c/three.txt",
            ".hidden/x.txt",
        ] {
            std::fs::write(root.join(file), b"x").expect("write tree file");
        }
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let names = |depth: usize, hidden: bool, roots: &[std::path::PathBuf]| {
            let listing = super::read_directory(roots, depth, hidden, &cancel);
            let mut names: Vec<String> = listing
                .iter()
                .map(|entry| entry.file_name.to_owned())
                .collect();
            names.sort();
            names
        };
        let roots = [root.clone()];
        // depth 2 lists three levels below the root (as `walkdir` did).
        assert_eq!(
            names(2, false, &roots),
            ["a", "b", "c", "one.txt", "top.txt", "two.txt"]
        );
        assert!(names(3, true, &roots).contains(&"x.txt".to_owned()));
        // A nested root adds nothing (and no duplicates).
        let nested = [root.clone(), root.join("a")];
        assert_eq!(names(2, false, &nested), names(2, false, &roots));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn linked_watch_roots_apply_only_to_the_view_that_produced_them() {
        let root = std::path::PathBuf::from("/virtual/root");
        let linked = std::path::PathBuf::from("/virtual/elsewhere");
        let mut tab = TabData::from_path(&root);
        tab.current_path = crate::app::dock::CurrentPath::One(root);
        tab.search = Some(Search {
            depth: 3,
            value: "query".to_owned(),
            ..Default::default()
        });
        assert!(tab.set_linked_watch_roots(vec![linked.clone()]));
        assert!(!tab.set_linked_watch_roots(vec![linked.clone()]));
        let specs = tab.watcher_specs();
        assert!(specs.contains(&(linked.clone(), notify::RecursiveMode::Recursive)));
        assert!(specs.iter().any(
            |(path, mode)| path.ends_with("root") && *mode == notify::RecursiveMode::Recursive
        ));
        // A different depth (or root) ignores links recorded for another view.
        tab.search = Some(Search {
            depth: 2,
            value: "query".to_owned(),
            ..Default::default()
        });
        assert!(!tab.watcher_specs().iter().any(|(path, _)| path == &linked));
    }

    #[cfg(windows)]
    #[test]
    fn deep_walk_reports_followed_junctions_once_and_survives_loops() {
        let root = unique_test_dir("junctions");
        let outside = unique_test_dir("junction_target");
        std::fs::write(outside.join("far.txt"), b"x").expect("write target file");
        std::fs::create_dir_all(root.join("sub")).expect("create sub");
        let mklink = |link: &std::path::Path, target: &std::path::Path| {
            let out = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .expect("mklink");
            assert!(out.status.success(), "{out:?}");
        };
        mklink(&root.join("to_outside"), &outside);
        mklink(&root.join("sub").join("again"), &outside);
        // A loop back to the root must terminate.
        mklink(&root.join("sub").join("loop"), &root);
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let result = super::read_directory_streaming(
            std::slice::from_ref(&root),
            6,
            false,
            &cancel,
            &|_| {},
        );
        let outside = crate::helper::normalize_path(&outside);
        let root_norm = crate::helper::normalize_path(&root);
        assert_eq!(
            result
                .linked_dirs
                .iter()
                .filter(|dir| **dir == outside)
                .count(),
            1,
            "{:?}",
            result.linked_dirs
        );
        assert!(result.linked_dirs.contains(&root_norm));
        assert!(
            result
                .listing
                .iter()
                .any(|entry| entry.file_name == "far.txt")
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn search_depth_clamps_to_one() {
        let mut tab = TabData::from_path(std::path::Path::new("/virtual"));
        assert_eq!(tab.search_depth(), 1, "no search should use depth 1");
        for (depth, expected) in [(0, 1), (1, 1), (3, 3)] {
            tab.search = Some(Search {
                depth,
                ..Default::default()
            });
            assert_eq!(
                tab.search_depth(),
                1,
                "an empty query only lists the folder"
            );
            tab.search = Some(Search {
                depth,
                value: "query".to_owned(),
                ..Default::default()
            });
            assert_eq!(tab.search_depth(), expected, "unexpected depth for {depth}");
        }
    }

    /// Build a file entry with explicit size and modification/creation times so
    /// the numeric sort keys can be exercised (`DirEntry::test_new` zeroes them).
    fn entry_with(name: &str, size: u64, modified: u32, created: u32) -> DirEntry {
        use crate::data::time::TimestampSeconds;
        let mut entry = DirEntry::test_new(&format!("/d/{name}"));
        entry.meta.size = size;
        let ts = |secs: u32| {
            TimestampSeconds::from(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(u64::from(secs)),
            )
        };
        entry.meta.modified_at = ts(modified);
        entry.meta.created_at = ts(created);
        entry
    }

    fn entry_accessed(name: &str, accessed: u32) -> DirEntry {
        let mut entry = entry_with(name, 0, 0, 0);
        entry.meta.accessed_at = crate::data::time::TimestampSeconds::from(
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(u64::from(accessed)),
        );
        entry
    }

    fn sorted_names(
        entries: &[DirEntry],
        sorting: super::super::Sort,
        invert: bool,
    ) -> Vec<String> {
        let listing = Listing::from_entries(entries);
        let settings = super::super::directory_view_settings::DirectoryViewSettings {
            sorting,
            display_type: super::super::DisplayType::default(),
            invert_sort: invert,
        };
        sort_order(&listing, &settings)
            .into_iter()
            .map(|index| listing.name(index as usize).to_owned())
            .collect()
    }

    #[test]
    fn sort_by_name_alphabetical_and_inverted() {
        let entries = [
            entry_with("c.txt", 0, 0, 0),
            entry_with("a.txt", 0, 0, 0),
            entry_with("b.txt", 0, 0, 0),
        ];
        assert_eq!(
            sorted_names(&entries, super::super::Sort::Name, false),
            ["a.txt", "b.txt", "c.txt"]
        );
        assert_eq!(
            sorted_names(&entries, super::super::Sort::Name, true),
            ["c.txt", "b.txt", "a.txt"]
        );
    }

    #[test]
    fn sort_keeps_directories_first() {
        let mut dir = entry_with("zeta", 0, 0, 0);
        dir.meta.entry_type = crate::data::files::EntryType::Directory;
        let entries = [entry_with("alpha", 9, 0, 0), dir];
        assert_eq!(
            sorted_names(&entries, super::super::Sort::Name, false),
            ["zeta", "alpha"]
        );
        assert_eq!(
            sorted_names(&entries, super::super::Sort::Size, true),
            ["zeta", "alpha"]
        );
    }

    #[test]
    fn sort_by_size_ascending_and_inverted() {
        let entries = [
            entry_with("big", 300, 0, 0),
            entry_with("small", 100, 0, 0),
            entry_with("mid", 200, 0, 0),
        ];
        assert_eq!(
            sorted_names(&entries, super::super::Sort::Size, false),
            ["small", "mid", "big"]
        );
        assert_eq!(
            sorted_names(&entries, super::super::Sort::Size, true),
            ["big", "mid", "small"]
        );
    }

    #[test]
    fn sort_by_modified_ascending_and_inverted() {
        let entries = [
            entry_with("newest", 0, 30, 0),
            entry_with("oldest", 0, 10, 0),
            entry_with("middle", 0, 20, 0),
        ];
        assert_eq!(
            sorted_names(&entries, super::super::Sort::Modified, false),
            ["oldest", "middle", "newest"]
        );
        assert_eq!(
            sorted_names(&entries, super::super::Sort::Modified, true),
            ["newest", "middle", "oldest"]
        );
    }

    #[test]
    fn opening_in_the_app_beats_an_older_access_time() {
        let mut entries = [
            entry_accessed("recent_atime", 30),
            entry_accessed("opened_here", 10),
        ];
        for entry in &mut entries {
            entry.dir = std::sync::Arc::from("/sort_history_date");
        }
        crate::app::open_history::record(std::path::Path::new("/sort_history_date/opened_here"));
        assert_eq!(
            sorted_names(&entries, super::super::Sort::Accessed, true),
            ["opened_here", "recent_atime"]
        );
    }

    #[test]
    fn times_opened_sorts_by_count_then_recency() {
        let mut entries = [
            entry_accessed("never", 50),
            entry_accessed("once", 10),
            entry_accessed("twice", 10),
            entry_accessed("never_older", 20),
        ];
        for entry in &mut entries {
            entry.dir = std::sync::Arc::from("/sort_history_count");
        }
        crate::app::open_history::record(std::path::Path::new("/sort_history_count/once"));
        crate::app::open_history::record(std::path::Path::new("/sort_history_count/twice"));
        crate::app::open_history::record(std::path::Path::new("/sort_history_count/twice"));
        assert_eq!(
            sorted_names(&entries, super::super::Sort::MostOpened, true),
            ["twice", "once", "never", "never_older"]
        );
    }

    #[test]
    fn sort_by_accessed_ascending_and_inverted() {
        let entries = [
            entry_accessed("recent", 30),
            entry_accessed("stale", 10),
            entry_accessed("middle", 20),
        ];
        assert_eq!(
            sorted_names(&entries, super::super::Sort::Accessed, false),
            ["stale", "middle", "recent"]
        );
        assert_eq!(
            sorted_names(&entries, super::super::Sort::Accessed, true),
            ["recent", "middle", "stale"]
        );
    }
}
