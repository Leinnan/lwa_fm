use std::{
    borrow::Cow,
    cmp::Ordering,
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering as AtomicOrdering},
};

use notify::RecursiveMode;

use icu::collator::CollatorBorrowed;
use rayon::{
    iter::{IntoParallelIterator, IntoParallelRefIterator, ParallelIterator},
    slice::ParallelSliceMut,
};

use crate::{
    app::{
        Data, MatchMode, Search, SearchTermType, Sort, database,
        directory_view_settings::{DirectoryShowHidden, DirectoryViewSettings},
        dock::{CurrentPath, build_collator},
    },
    data::files::{DirEntry, DirEntryData, DirEntryMetaData, DirList, EntryType, SortKey},
    helper::{DataHolder, normalize_path},
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
}

/// Memoisation store for `TabData::watcher_specs`. Keeps the hot per-frame
/// change-detection path (`should_refresh_for_directories`) from redoing the
/// `walkdir` symlink walk + `canonicalize` syscalls that `compute_watcher_specs`
/// performs.
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
        CurrentPath::None => extract(Data::default()),
    }
}

impl TabData {
    pub fn watcher_specs(&mut self) -> Vec<(PathBuf, RecursiveMode)> {
        // Always recompute fresh on this reconcile path (called from
        // `reconcile_tab_watchers`) so newly-created symlinked watch roots are
        // picked up for single-root recursive views. Multi-root views skip that
        // discovery (see `compute_watcher_specs`), so this stays cheap for them.
        // The result also warms the memoisation cache used by the hot
        // `should_refresh_for_directories` path.
        let specs = self.compute_watcher_specs();
        self.watcher_specs_cache.specs.clone_from(&specs);
        self.watcher_specs_cache.inputs = Some(self.specs_inputs());
        specs
    }

    /// Recompute the cached watcher specs only if the spec-relevant inputs
    /// (`current_path`, search depth, `extra_dirs`) have changed since the last
    /// compute. This is the cheap path used by the per-frame change handler and
    /// avoids the `walkdir` + `canonicalize` work done by `compute_watcher_specs`.
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
            CurrentPath::None => {}
            CurrentPath::One(p) => roots.push(p.clone()),
            CurrentPath::Multiple(ps) => roots.extend(ps.iter().cloned()),
        }
        if let Some(s) = &self.search {
            roots.extend(s.extra_dirs.iter().cloned());
        }
        SpecsInputs {
            roots,
            is_recursive,
        }
    }

    fn compute_watcher_specs(&self) -> Vec<(PathBuf, RecursiveMode)> {
        // Gather every root this view covers: the current path(s) plus any extra
        // search directories.
        let mut roots: Vec<PathBuf> = match &self.current_path {
            CurrentPath::None => vec![],
            CurrentPath::One(path_buf) => vec![path_buf.clone()],
            CurrentPath::Multiple(path_bufs) => path_bufs.clone(),
        };
        if let Some(search) = &self.search {
            roots.extend(search.extra_dirs.iter().cloned());
        }

        // Recursive watching requires discovering symlinked sub-directories,
        // which `linked_watch_roots` does via a `walkdir` sweep plus a
        // `canonicalize` syscall per symlink — work that scales with the number
        // of roots × the tree size at the search depth. Running that once per
        // favorite during a deep favorites search freezes the UI thread.
        //
        // We therefore only afford recursive watching (with symlink expansion)
        // for a SINGLE root. Multi-root views (favorites, multi-dir searches)
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
            let normalized = normalize_path(root);
            specs.insert((normalized.clone(), mode));
            if recursive {
                for linked_root in linked_watch_roots(&normalized, depth) {
                    specs.insert((linked_root, mode));
                }
            }
        }
        specs.into_iter().collect()
    }

    pub fn search_depth(&self) -> usize {
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
        self.current_path = path;
        if let Some(path) = self.current_path.get_path() {
            self.top_display_path.build(&path, self.show_hidden);
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
            self.top_display_path.build(&path, self.show_hidden);
        }
    }

    pub fn update_visible_entries(&mut self) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!(
            "dir_handling::update_visible_entries",
            self.total_entry_count().to_string()
        );
        self.visible_entries = if let Some(dir_list) = &self.dir_list {
            filter_visible_dir_list(dir_list, self.show_hidden, self.search.as_ref())
        } else {
            filter_visible_entries(&self.list, self.show_hidden, self.search.as_ref())
        };
    }

    pub fn sort_entries(&mut self, sort_settings: &DirectoryViewSettings) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::sort_entries");
        self.display_type = sort_settings.display_type;
        if let Some(dir_list) = &mut self.dir_list {
            sort_dir_entry_data_slice(
                std::sync::Arc::make_mut(&mut dir_list.entries),
                sort_settings,
            );
        } else {
            sort_entries_vec(&mut self.list, sort_settings);
        }
    }

    #[cfg(test)]
    pub fn update_file_metadata(
        &mut self,
        path: &Path,
        sort_settings: &DirectoryViewSettings,
    ) -> bool {
        let Ok(metadata) = std::fs::metadata(path) else {
            return false;
        };
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        let meta: DirEntryMetaData = metadata.into();
        let mut updated = false;

        if let Some(dir_list) = &mut self.dir_list {
            let Some(parent) = path.parent().map(normalize_path) else {
                return false;
            };
            if normalize_path(Path::new(dir_list.dir.as_ref())) != parent {
                return false;
            }
            let entries = std::sync::Arc::make_mut(&mut dir_list.entries);
            if let Some(entry) = entries
                .iter_mut()
                .find(|entry| entry.file_name == file_name)
            {
                entry.meta = meta;
                updated = true;
            }
        } else if let Some(entry) = self.list.iter_mut().find(|entry| {
            entry.file_name == file_name
                && path.parent().is_some_and(|parent| {
                    normalize_path(Path::new(entry.dir.as_ref())) == normalize_path(parent)
                })
        }) {
            entry.meta = meta;
            updated = true;
        }

        if !updated {
            return false;
        }

        if matches!(
            sort_settings.sorting,
            Sort::Modified | Sort::Created | Sort::Size
        ) {
            self.sort_entries(sort_settings);
        }
        self.update_visible_entries();
        true
    }

    /// Apply a coalesced watcher batch to a single-directory listing. The
    /// backing slice is cloned once and sorting/filtering run once regardless
    /// of how many files changed.
    pub fn apply_file_changes(
        &mut self,
        changes: &[crate::watcher::FileChange],
        sort_settings: &DirectoryViewSettings,
    ) -> bool {
        let Some(dir_list) = self.dir_list.as_mut() else {
            return false;
        };
        let listing_dir = normalize_path(Path::new(dir_list.dir.as_ref()));
        if changes.iter().any(|change| {
            let path = match change {
                crate::watcher::FileChange::Metadata(path)
                | crate::watcher::FileChange::Created(path)
                | crate::watcher::FileChange::Removed(path) => path,
            };
            path.parent().map(normalize_path).as_ref() != Some(&listing_dir)
        }) {
            return false;
        }

        let mut entries = dir_list.entries.iter().cloned().collect::<Vec<_>>();
        for change in changes {
            let path = match change {
                crate::watcher::FileChange::Metadata(path)
                | crate::watcher::FileChange::Created(path)
                | crate::watcher::FileChange::Removed(path) => path,
            };
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                return false;
            };
            match change {
                crate::watcher::FileChange::Metadata(_) => {
                    let Ok(metadata) = std::fs::metadata(path) else {
                        return false;
                    };
                    let Some(entry) = entries
                        .iter_mut()
                        .find(|entry| entry.file_name == file_name)
                    else {
                        return false;
                    };
                    entry.meta = metadata.into();
                }
                crate::watcher::FileChange::Created(_) => {
                    let Ok(metadata) = std::fs::metadata(path) else {
                        continue;
                    };
                    let meta: DirEntryMetaData = metadata.into();
                    let data = DirEntryData {
                        sort_key: SortKey::new_path(
                            file_name,
                            matches!(meta.entry_type, EntryType::File),
                        ),
                        file_name: file_name.to_owned(),
                        meta,
                    };
                    if let Some(existing) = entries
                        .iter_mut()
                        .find(|entry| entry.file_name == file_name)
                    {
                        *existing = data;
                    } else {
                        entries.push(data);
                    }
                }
                crate::watcher::FileChange::Removed(_) => {
                    entries.retain(|entry| entry.file_name != file_name);
                }
            }
        }

        dir_list.entries = std::sync::Arc::from(entries);
        self.sort_entries(sort_settings);
        self.update_visible_entries();
        true
    }

    /// Surgically insert a newly-created file entry without a full directory
    /// re-read. Only the lazy single-directory listing (`dir_list`) supports
    /// this; search / multi-directory / recursive views return `false` so the
    /// caller can fall back to a structural refresh. Returns `true` if applied.
    #[cfg(test)]
    pub fn insert_file_entry(
        &mut self,
        path: &Path,
        sort_settings: &DirectoryViewSettings,
    ) -> bool {
        let Some(dir_list) = self.dir_list.as_mut() else {
            return false;
        };
        let Some(parent) = path.parent().map(normalize_path) else {
            return false;
        };
        if normalize_path(Path::new(dir_list.dir.as_ref())) != parent {
            return false;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        let Ok(metadata) = std::fs::metadata(path) else {
            return false;
        };
        let meta: DirEntryMetaData = metadata.into();
        let is_file = matches!(meta.entry_type, EntryType::File);
        let data = DirEntryData {
            sort_key: SortKey::new_path(file_name, is_file),
            file_name: file_name.to_string(),
            meta,
        };
        // `entries` is an `Arc<[DirEntryData]>` (an immutable-sized slice), so a
        // length change requires rebuilding it via a `Vec`. This stays far
        // cheaper than a full directory re-scan (one metadata syscall + an
        // in-memory clone, no per-entry `fs` reads).
        let mut entries: Vec<DirEntryData> = dir_list.entries.iter().cloned().collect();
        // Replace an existing entry with the same name rather than duplicating
        // (a re-create after a fast delete, or a same-name rename target).
        if let Some(existing) = entries
            .iter_mut()
            .find(|entry| entry.file_name == data.file_name)
        {
            *existing = data;
        } else {
            entries.push(data);
        }
        dir_list.entries = std::sync::Arc::from(entries);
        self.sort_entries(sort_settings);
        self.update_visible_entries();
        true
    }

    /// Surgically remove a deleted file entry without a full directory re-read.
    /// Same scoping rules as [`insert_file_entry`]; returns `true` if removed.
    #[cfg(test)]
    pub fn remove_file_entry(
        &mut self,
        path: &Path,
        sort_settings: &DirectoryViewSettings,
    ) -> bool {
        let Some(dir_list) = self.dir_list.as_mut() else {
            return false;
        };
        let Some(parent) = path.parent().map(normalize_path) else {
            return false;
        };
        if normalize_path(Path::new(dir_list.dir.as_ref())) != parent {
            return false;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        let mut entries: Vec<DirEntryData> = dir_list.entries.iter().cloned().collect();
        let before = entries.len();
        entries.retain(|entry| entry.file_name != file_name);
        if entries.len() == before {
            return false;
        }
        dir_list.entries = std::sync::Arc::from(entries);
        self.sort_entries(sort_settings);
        self.update_visible_entries();
        true
    }

    pub fn deep_or_multiple_paths(&self) -> bool {
        self.current_path.multiple_paths()
            || self.search.as_ref().map_or(1, |search| search.depth) > 1
            || self
                .search
                .as_ref()
                .is_some_and(|s| !s.extra_dirs.is_empty())
    }
}

/// Walk a single root path and return its entries.
/// Shared helper used by [`read_directory`] for each path in the parallelized
/// per-root walk.
fn walk_single_root(
    root: &PathBuf,
    depth: usize,
    show_hidden: bool,
    cancel: &AtomicBool,
) -> Vec<DirEntry> {
    if cancel.load(AtomicOrdering::SeqCst) {
        return Vec::new();
    }
    if depth > 1 {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!(
            "lwa_fm::dir_handling::walk_single_root::walkdir",
            root.to_string_lossy().as_ref()
        );
        let walk_entries: Vec<walkdir::DirEntry> = walkdir::WalkDir::new(root)
            .follow_links(true)
            .min_depth(1)
            .max_depth(depth + 1)
            .into_iter()
            .filter_entry(|e| {
                if e.depth() == 0 {
                    return true;
                }
                if show_hidden {
                    return true;
                }
                let name = e.file_name().to_string_lossy();
                !name.starts_with('.') && !name.starts_with('$')
            })
            .filter_map(Result::ok)
            .take_while(|_| !cancel.load(AtomicOrdering::SeqCst))
            .collect();

        if cancel.load(AtomicOrdering::SeqCst) {
            return Vec::new();
        }

        #[cfg(feature = "profiling")]
        puffin::profile_scope!(
            "lwa_fm::dir_handling::walk_single_root::parallel_convert",
            walk_entries.len().to_string().as_str()
        );
        walk_entries
            .into_par_iter()
            .filter_map(|e| e.try_into().ok())
            .collect()
    } else {
        let mut entries = Vec::new();
        database::read_dir(root, &mut entries);
        entries
    }
}

pub fn read_directory(
    paths: &[PathBuf],
    depth: usize,
    show_hidden: bool,
    cancel: &AtomicBool,
) -> Vec<DirEntry> {
    #[cfg(feature = "profiling")]
    puffin::profile_scope!("lwa_fm::dir_handling::read_directory");

    // Parallel per-root walk for multiple paths.
    // For a single path the overhead of rayon dispatch is negligible.
    let lists: Vec<Vec<DirEntry>> = paths
        .par_iter()
        .map(|d| walk_single_root(d, depth, show_hidden, cancel))
        .collect();

    let mut list: Vec<DirEntry> = lists.into_iter().flatten().collect();

    // Dedup is only needed with multiple roots or recursive walks (symlinks may cause overlap)
    if paths.len() > 1 || depth > 1 {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!(
            "lwa_fm::dir_handling::read_directory::dedup",
            list.len().to_string().as_str()
        );
        let mut seen: std::collections::HashSet<String> =
            std::collections::HashSet::with_capacity(list.len());
        list.retain(|e| seen.insert(e.full_path_string()));
    }
    list
}

fn sort_dir_entry_data_slice(entries: &mut [DirEntryData], settings: &DirectoryViewSettings) {
    match settings.sorting {
        Sort::Modified => {
            if settings.invert_sort {
                entries.par_sort_unstable_by(|a, b| {
                    let file_type_cmp = (a.meta.entry_type == crate::data::files::EntryType::File)
                        .cmp(&(b.meta.entry_type == crate::data::files::EntryType::File));
                    file_type_cmp.then(b.meta.modified_at.cmp(&a.meta.modified_at))
                });
            } else {
                entries.par_sort_unstable_by(|a, b| {
                    let file_type_cmp = (a.meta.entry_type == crate::data::files::EntryType::File)
                        .cmp(&(b.meta.entry_type == crate::data::files::EntryType::File));
                    file_type_cmp.then(a.meta.modified_at.cmp(&b.meta.modified_at))
                });
            }
        }
        Sort::Name => {
            if settings.invert_sort {
                entries.par_sort_unstable_by(|a, b| b.sort_key.compare(&a.sort_key));
            } else {
                entries.par_sort_unstable_by(|a, b| a.sort_key.compare(&b.sort_key));
            }
        }
        Sort::Created => {
            if settings.invert_sort {
                entries.par_sort_unstable_by(|a, b| {
                    let file_type_cmp = (a.meta.entry_type == crate::data::files::EntryType::File)
                        .cmp(&(b.meta.entry_type == crate::data::files::EntryType::File));
                    file_type_cmp.then(b.meta.created_at.cmp(&a.meta.created_at))
                });
            } else {
                entries.par_sort_unstable_by(|a, b| {
                    let file_type_cmp = (a.meta.entry_type == crate::data::files::EntryType::File)
                        .cmp(&(b.meta.entry_type == crate::data::files::EntryType::File));
                    file_type_cmp.then(a.meta.created_at.cmp(&b.meta.created_at))
                });
            }
        }
        Sort::Size => {
            if settings.invert_sort {
                entries.par_sort_unstable_by(|a, b| {
                    let file_type_cmp = (a.meta.entry_type == crate::data::files::EntryType::File)
                        .cmp(&(b.meta.entry_type == crate::data::files::EntryType::File));
                    file_type_cmp.then(b.meta.size.cmp(&a.meta.size))
                });
            } else {
                entries.par_sort_unstable_by(|a, b| {
                    let file_type_cmp = (a.meta.entry_type == crate::data::files::EntryType::File)
                        .cmp(&(b.meta.entry_type == crate::data::files::EntryType::File));
                    file_type_cmp.then(a.meta.size.cmp(&b.meta.size))
                });
            }
        }
        Sort::Random => {
            use rand::seq::SliceRandom;
            use rand::thread_rng;
            let mut rng = thread_rng();
            entries.shuffle(&mut rng);
        }
    }
}

pub fn sort_entries_vec(entries: &mut [DirEntry], settings: &DirectoryViewSettings) {
    match settings.sorting {
        Sort::Modified => {
            if settings.invert_sort {
                entries.par_sort_unstable_by(|a, b| {
                    let file_type_cmp = a.is_file().cmp(&b.is_file());
                    file_type_cmp.then(b.meta.modified_at.cmp(&a.meta.modified_at))
                });
            } else {
                entries.par_sort_unstable_by(|a, b| {
                    let file_type_cmp = a.is_file().cmp(&b.is_file());
                    file_type_cmp.then(a.meta.modified_at.cmp(&b.meta.modified_at))
                });
            }
        }
        Sort::Name => {
            if settings.invert_sort {
                entries.par_sort_unstable_by(|a, b| b.sort_key.compare(&a.sort_key));
            } else {
                entries.par_sort_unstable_by(|a, b| a.sort_key.compare(&b.sort_key));
            }
        }
        Sort::Created => {
            if settings.invert_sort {
                entries.par_sort_unstable_by(|a, b| {
                    let file_type_cmp = a.is_file().cmp(&b.is_file());
                    file_type_cmp.then(b.meta.created_at.cmp(&a.meta.created_at))
                });
            } else {
                entries.par_sort_unstable_by(|a, b| {
                    let file_type_cmp = a.is_file().cmp(&b.is_file());
                    file_type_cmp.then(a.meta.created_at.cmp(&b.meta.created_at))
                });
            }
        }
        Sort::Size => {
            if settings.invert_sort {
                entries.par_sort_unstable_by(|a, b| {
                    let file_type_cmp = a.is_file().cmp(&b.is_file());
                    file_type_cmp.then(b.meta.size.cmp(&a.meta.size))
                });
            } else {
                entries.par_sort_unstable_by(|a, b| {
                    let file_type_cmp = a.is_file().cmp(&b.is_file());
                    file_type_cmp.then(a.meta.size.cmp(&b.meta.size))
                });
            }
        }
        Sort::Random => {
            use rand::seq::SliceRandom;
            use rand::thread_rng;
            let mut rng = thread_rng();
            entries.shuffle(&mut rng);
        }
    }
}

enum CompiledTerm {
    Plain(String),
    Glob(glob::Pattern),
    Regex(regex::Regex),
}

fn compile_term(
    pattern: &str,
    term_type: SearchTermType,
    case_sensitive: bool,
) -> Option<CompiledTerm> {
    match term_type {
        SearchTermType::Plain => {
            if case_sensitive {
                Some(CompiledTerm::Plain(pattern.to_string()))
            } else {
                Some(CompiledTerm::Plain(pattern.to_lowercase()))
            }
        }
        SearchTermType::Glob => glob::Pattern::new(pattern).ok().map(CompiledTerm::Glob),
        SearchTermType::Regex => regex::Regex::new(pattern).ok().map(CompiledTerm::Regex),
    }
}

struct CompiledSearch {
    terms: Vec<CompiledTerm>,
    mode: MatchMode,
    has_plain: bool,
}

impl CompiledSearch {
    fn matches(
        &self,
        name: &str,
        case_sensitive: bool,
        collator: Option<&CollatorBorrowed<'_>>,
    ) -> bool {
        let matches_one = |term: &CompiledTerm| -> bool {
            match term {
                CompiledTerm::Plain(pattern) => {
                    plain_matches(name, pattern, case_sensitive, collator)
                }
                CompiledTerm::Glob(glob) => glob.matches(name),
                CompiledTerm::Regex(re) => re.is_match(name),
            }
        };
        match self.mode {
            MatchMode::All => self.terms.iter().all(matches_one),
            MatchMode::Any => self.terms.iter().any(matches_one),
        }
    }
}

fn contains_ignore_ascii_case(name: &str, pattern: &str) -> bool {
    let needle = pattern.as_bytes();
    if needle.is_empty() {
        return true;
    }
    if needle.len() > name.len() {
        return false;
    }
    name.as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

fn plain_matches(
    name: &str,
    pattern: &str,
    case_sensitive: bool,
    collator: Option<&CollatorBorrowed<'_>>,
) -> bool {
    if pattern.is_empty() {
        return true;
    }
    if case_sensitive {
        return name.contains(pattern);
    }
    // Very common fast path for large ASCII-heavy directories. This avoids the
    // per-character ICU collation loop while keeping the collator fallback for
    // non-ASCII names where locale-aware matching matters.
    if name.is_ascii() && pattern.is_ascii() {
        return contains_ignore_ascii_case(name, pattern);
    }

    let search_len = pattern.len();
    if search_len > name.len() {
        return false;
    }
    let chars = name.as_bytes();
    if let Some(collator) = collator {
        for (j, _) in name.char_indices() {
            if j + search_len > name.len() {
                break;
            }
            if !name.is_char_boundary(j + search_len) {
                continue;
            }
            if collator.compare_utf8(pattern.as_bytes(), &chars[j..j + search_len])
                == Ordering::Equal
            {
                return true;
            }
        }
    }
    name.to_lowercase().contains(pattern)
}

fn compile_search(search: Option<&Search>) -> (bool, Option<CompiledSearch>) {
    let case_sensitive = search.is_some_and(|s| s.case_sensitive);
    let compiled_search = search.and_then(|s| {
        let terms: Vec<CompiledTerm> = if !s.terms.is_empty() {
            s.terms
                .iter()
                .filter_map(|st| compile_term(&st.pattern, st.term_type, case_sensitive))
                .collect()
        } else if !s.value.is_empty() {
            compile_term(&s.value, s.term_type, case_sensitive)
                .into_iter()
                .collect()
        } else {
            return None;
        };
        if terms.is_empty() {
            return None;
        }
        let has_plain = s
            .terms
            .iter()
            .any(|st| st.term_type == SearchTermType::Plain)
            || (s.terms.is_empty() && s.term_type == SearchTermType::Plain);
        Some(CompiledSearch {
            terms,
            mode: s.match_mode,
            has_plain,
        })
    });
    (case_sensitive, compiled_search)
}

pub fn filter_visible_entries(
    entries: &[DirEntry],
    show_hidden: bool,
    search: Option<&Search>,
) -> Vec<usize> {
    let mut visible = Vec::new();
    let (case_sensitive, compiled_search) = compile_search(search);
    let collator = if compiled_search.as_ref().is_some_and(|cs| cs.has_plain) {
        Some(build_collator(case_sensitive))
    } else {
        None
    };
    for (i, entry) in entries.iter().enumerate() {
        let name = entry.get_splitted_path().1;
        if !show_hidden && (name.starts_with('.') || name.starts_with('$')) {
            continue;
        }
        if let Some(ref cs) = compiled_search
            && !cs.matches(name, case_sensitive, collator.as_ref())
        {
            continue;
        }
        visible.push(i);
    }
    visible
}

pub fn filter_visible_dir_list(
    dir_list: &DirList,
    show_hidden: bool,
    search: Option<&Search>,
) -> Vec<usize> {
    let mut visible = Vec::new();
    let (case_sensitive, compiled_search) = compile_search(search);
    let collator = if compiled_search.as_ref().is_some_and(|cs| cs.has_plain) {
        Some(build_collator(case_sensitive))
    } else {
        None
    };
    for (i, entry) in dir_list.entries.iter().enumerate() {
        let name = entry.file_name.as_str();
        if !show_hidden && (name.starts_with('.') || name.starts_with('$')) {
            continue;
        }
        if let Some(ref cs) = compiled_search
            && !cs.matches(name, case_sensitive, collator.as_ref())
        {
            continue;
        }
        visible.push(i);
    }
    visible
}

fn linked_watch_roots(root: &Path, depth: usize) -> BTreeSet<PathBuf> {
    let mut linked_roots = BTreeSet::new();
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .min_depth(1)
        .max_depth(depth)
        .into_iter()
        .flatten()
    {
        if !entry.path_is_symlink() {
            continue;
        }
        let Ok(target) = std::fs::canonicalize(entry.path()) else {
            continue;
        };
        if target.is_dir() {
            linked_roots.insert(normalize_path(&target));
        }
    }
    linked_roots
}

pub fn get_directories(path: &Path, show_hidden: bool) -> BTreeSet<Cow<'static, str>> {
    get_directories_recursive(path, show_hidden, 2)
}

pub fn has_subdirectories(path: &Path, show_hidden: bool) -> bool {
    walkdir::WalkDir::new(path)
        .max_depth(1)
        .into_iter()
        .filter_map(Result::ok)
        .any(|e| {
            if !e.file_type().is_dir() {
                return false;
            }
            if !show_hidden {
                let file_name = e.file_name().to_string_lossy();
                if file_name.starts_with('.') || file_name.starts_with('$') {
                    return false;
                }
            }
            true
        })
}

pub fn get_directories_recursive(
    path: &Path,
    show_hidden: bool,
    depth: usize,
) -> BTreeSet<Cow<'static, str>> {
    walkdir::WalkDir::new(path)
        .max_depth(depth)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| {
            if !e.file_type().is_dir() {
                return false;
            }
            if show_hidden {
                return true;
            }
            let s = e.file_name().to_string_lossy();
            if s.starts_with('.') || s.starts_with('$') {
                return false;
            }
            let mut current_path: Option<&Path> = e.path().parent();

            while let Some(parent) = current_path {
                if let Some(parent_name) = parent.file_name().and_then(|name| name.to_str())
                    && (parent_name.starts_with('.') || parent_name.starts_with('$'))
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
    use rayon::slice::ParallelSliceMut;

    use crate::app::dock::TabData;
    use crate::data::files::{DirEntry, DirList};

    use super::{Search, filter_visible_dir_list, filter_visible_entries};

    #[test]
    fn filter_visible_entries_includes_all_entries_uniformly() {
        let entries = vec![
            DirEntry::test_new("/fav/file_report.txt"),
            DirEntry::test_new("/fav/subdir/report.txt"),
            DirEntry::test_new("/fav/notes.txt"),
            DirEntry::test_new("/fav/subdir/deep/report.txt"),
        ];
        let show_hidden = true;

        // No search: all entries pass
        let visible = filter_visible_entries(&entries, show_hidden, None);
        assert_eq!(visible.len(), 4, "no search should include all entries");

        // Search for "report": root-level + subdirectory entries should match
        let search = Search {
            value: "report".to_string(),
            depth: 2,
            case_sensitive: false,
            ..Default::default()
        };
        let visible = filter_visible_entries(&entries, show_hidden, Some(&search));
        assert!(
            visible.contains(&0),
            "root-level file_report.txt should match"
        );
        assert!(visible.contains(&1), "subdir/report.txt should match");
        assert!(visible.contains(&3), "subdir/deep/report.txt should match");
        assert_eq!(visible.len(), 3);

        // Search for "notes": only root-level
        let search = Search {
            value: "notes".to_string(),
            depth: 2,
            case_sensitive: false,
            ..Default::default()
        };
        let visible = filter_visible_entries(&entries, show_hidden, Some(&search));
        assert_eq!(
            visible.len(),
            1,
            "search 'notes' should match only root-level notes.txt"
        );
        assert!(visible.contains(&2), "root-level notes.txt should match");
    }

    #[test]
    fn filter_visible_entries_hides_hidden_files() {
        let entries = vec![
            DirEntry::test_new("/fav/.hidden.txt"),
            DirEntry::test_new("/fav/visible.txt"),
            DirEntry::test_new("/fav/subdir/.hidden.txt"),
        ];
        let show_hidden = false;
        let visible = filter_visible_entries(&entries, show_hidden, None);
        assert_eq!(
            visible.len(),
            1,
            "only visible.txt should pass with show_hidden=false"
        );
        assert!(visible.contains(&1));
    }

    #[test]
    fn filter_visible_dir_list_matches_eager_entries() {
        let entries = vec![
            DirEntry::test_new("/fav/file_report.txt"),
            DirEntry::test_new("/fav/.hidden_report.txt"),
            DirEntry::test_new("/fav/notes.txt"),
            DirEntry::test_new("/fav/REPORT_final.txt"),
        ];
        let search = Search {
            value: "report".to_string(),
            case_sensitive: false,
            ..Default::default()
        };
        let eager = filter_visible_entries(&entries, false, Some(&search));
        let lazy = DirList::from_owned_list(entries).expect("non-empty dir list");
        let lazy_visible = filter_visible_dir_list(&lazy, false, Some(&search));

        assert_eq!(
            lazy_visible, eager,
            "lazy DirList filtering should preserve eager search behavior"
        );
        assert_eq!(lazy_visible, vec![0, 3]);
    }

    #[test]
    fn dedup_removes_all_duplicates_regardless_of_order() {
        let entries = [
            DirEntry::test_new("/fav/subdir/file.txt"),
            DirEntry::test_new("/fav/file.txt"),
            DirEntry::test_new("/fav/subdir/file.txt"),
            DirEntry::test_new("/fav/file.txt"),
        ];
        let mut list: Vec<DirEntry> = entries.to_vec();
        list.par_sort_unstable_by(|a, b| {
            a.dir
                .as_ref()
                .cmp(b.dir.as_ref())
                .then(a.file_name.cmp(&b.file_name))
        });
        list.dedup_by(|a, b| a.dir == b.dir && a.file_name == b.file_name);
        assert_eq!(list.len(), 2, "should have 2 unique entries");
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
        dir
    }

    #[test]
    fn update_file_metadata_updates_eager_entry_without_refresh() {
        let dir = unique_test_dir("eager_meta");
        let file = dir.join("hot.log");
        std::fs::write(&file, b"abc").expect("write temp file");

        let mut tab = TabData::from_path(&dir);
        tab.list = vec![DirEntry::test_new(&file.to_string_lossy())];
        tab.visible_entries = vec![0];

        let settings = super::super::directory_view_settings::DirectoryViewSettings::default();
        assert!(tab.update_file_metadata(&file, &settings));
        assert_eq!(tab.list[0].meta.size, 3);
        assert_eq!(tab.visible_entries, vec![0]);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn update_file_metadata_updates_lazy_dir_list_without_refresh() {
        let dir = unique_test_dir("lazy_meta");
        let file = dir.join("hot.log");
        std::fs::write(&file, b"abcdef").expect("write temp file");

        let mut tab = TabData::from_path(&dir);
        let entry = DirEntry::test_new(&file.to_string_lossy());
        tab.dir_list = DirList::from_owned_list(vec![entry]);
        tab.visible_entries = vec![0];

        let settings = super::super::directory_view_settings::DirectoryViewSettings::default();
        assert!(tab.update_file_metadata(&file, &settings));
        let dir_list = tab.dir_list.as_ref().expect("lazy dir list");
        assert_eq!(dir_list.entries[0].meta.size, 6);
        assert_eq!(tab.visible_entries, vec![0]);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn insert_file_entry_adds_new_entry_to_lazy_dir_list() {
        let dir = unique_test_dir("insert_new");
        let existing = dir.join("a.txt");
        std::fs::write(&existing, b"a").expect("write existing file");
        let new_file = dir.join("b.txt");
        std::fs::write(&new_file, b"hello").expect("write new file");

        let mut tab = TabData::from_path(&dir);
        tab.dir_list =
            DirList::from_owned_list(vec![DirEntry::test_new(&existing.to_string_lossy())]);
        tab.visible_entries = vec![0];

        let settings = super::super::directory_view_settings::DirectoryViewSettings::default();
        assert!(tab.insert_file_entry(&new_file, &settings));

        let dir_list = tab.dir_list.as_ref().expect("lazy dir list");
        assert_eq!(dir_list.entries.len(), 2, "entry should be inserted");
        let inserted = dir_list
            .entries
            .iter()
            .find(|e| e.file_name == "b.txt")
            .expect("inserted entry present");
        assert_eq!(inserted.meta.size, 5);
        assert_eq!(
            tab.visible_entries.len(),
            2,
            "visible indices should cover both entries"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn insert_file_entry_replaces_same_name_entry() {
        let dir = unique_test_dir("insert_replace");
        let file = dir.join("hot.log");
        std::fs::write(&file, b"old").expect("write initial file");

        let mut tab = TabData::from_path(&dir);
        tab.dir_list = DirList::from_owned_list(vec![DirEntry::test_new(&file.to_string_lossy())]);
        tab.visible_entries = vec![0];

        // Simulate a rewrite of the same name: rewrite disk content first.
        std::fs::write(&file, b"new-contents").expect("rewrite file");

        let settings = super::super::directory_view_settings::DirectoryViewSettings::default();
        assert!(tab.insert_file_entry(&file, &settings));

        let dir_list = tab.dir_list.as_ref().expect("lazy dir list");
        assert_eq!(dir_list.entries.len(), 1, "no duplicate created");
        assert_eq!(dir_list.entries[0].meta.size, 12, "metadata refreshed");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn insert_file_entry_returns_false_for_wrong_parent() {
        let dir = unique_test_dir("insert_wrong");
        let other = unique_test_dir("insert_other");
        let file = other.join("stray.txt");
        std::fs::write(&file, b"x").expect("write file");

        let mut tab = TabData::from_path(&dir);
        tab.dir_list = DirList::from_owned_list(vec![DirEntry::test_new(
            &dir.join("a.txt").to_string_lossy(),
        )]);
        tab.visible_entries = vec![0];

        let settings = super::super::directory_view_settings::DirectoryViewSettings::default();
        assert!(
            !tab.insert_file_entry(&file, &settings),
            "different parent should not apply"
        );

        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(other);
    }

    #[test]
    fn remove_file_entry_drops_entry_from_lazy_dir_list() {
        let dir = unique_test_dir("remove_entry");
        let keep = dir.join("keep.txt");
        let gone = dir.join("gone.txt");
        std::fs::write(&keep, b"k").expect("write keep");
        std::fs::write(&gone, b"g").expect("write gone");

        let mut tab = TabData::from_path(&dir);
        tab.dir_list = DirList::from_owned_list(vec![
            DirEntry::test_new(&keep.to_string_lossy()),
            DirEntry::test_new(&gone.to_string_lossy()),
        ]);
        tab.visible_entries = vec![0, 1];

        let settings = super::super::directory_view_settings::DirectoryViewSettings::default();
        assert!(tab.remove_file_entry(&gone, &settings));

        let dir_list = tab.dir_list.as_ref().expect("lazy dir list");
        assert_eq!(dir_list.entries.len(), 1, "entry removed");
        assert_eq!(dir_list.entries[0].file_name, "keep.txt");
        assert_eq!(tab.visible_entries.len(), 1);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn remove_file_entry_returns_false_when_not_found() {
        let dir = unique_test_dir("remove_missing");
        std::fs::write(dir.join("a.txt"), b"a").expect("write a");
        let missing = dir.join("nope.txt");

        let mut tab = TabData::from_path(&dir);
        tab.dir_list = DirList::from_owned_list(vec![DirEntry::test_new(
            &dir.join("a.txt").to_string_lossy(),
        )]);
        tab.visible_entries = vec![0];

        let settings = super::super::directory_view_settings::DirectoryViewSettings::default();
        assert!(
            !tab.remove_file_entry(&missing, &settings),
            "absent entry should not apply"
        );
        let dir_list = tab.dir_list.as_ref().expect("lazy dir list");
        assert_eq!(dir_list.entries.len(), 1, "listing unchanged");

        let _ = std::fs::remove_dir_all(dir);
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

    fn names_of(entries: &[DirEntry]) -> Vec<String> {
        entries.iter().map(|e| e.file_name.clone()).collect()
    }

    fn settings(
        sorting: super::super::Sort,
        invert: bool,
    ) -> super::super::directory_view_settings::DirectoryViewSettings {
        super::super::directory_view_settings::DirectoryViewSettings {
            sorting,
            display_type: super::super::DisplayType::default(),
            invert_sort: invert,
        }
    }

    #[test]
    fn sort_by_name_alphabetical_and_inverted() {
        let mut entries = vec![
            entry_with("c.txt", 0, 0, 0),
            entry_with("a.txt", 0, 0, 0),
            entry_with("b.txt", 0, 0, 0),
        ];
        super::sort_entries_vec(&mut entries, &settings(super::super::Sort::Name, false));
        assert_eq!(
            names_of(&entries),
            vec!["a.txt", "b.txt", "c.txt"],
            "name sort should be alphabetical"
        );

        super::sort_entries_vec(&mut entries, &settings(super::super::Sort::Name, true));
        assert_eq!(
            names_of(&entries),
            vec!["c.txt", "b.txt", "a.txt"],
            "inverted name sort should reverse"
        );
    }

    #[test]
    fn sort_by_size_ascending_and_inverted() {
        let mut entries = vec![
            entry_with("big", 300, 0, 0),
            entry_with("small", 100, 0, 0),
            entry_with("mid", 200, 0, 0),
        ];
        super::sort_entries_vec(&mut entries, &settings(super::super::Sort::Size, false));
        assert_eq!(
            names_of(&entries),
            vec!["small", "mid", "big"],
            "size sort should be ascending"
        );

        super::sort_entries_vec(&mut entries, &settings(super::super::Sort::Size, true));
        assert_eq!(
            names_of(&entries),
            vec!["big", "mid", "small"],
            "inverted size sort should be descending"
        );
    }

    #[test]
    fn sort_by_modified_ascending_and_inverted() {
        let mut entries = vec![
            entry_with("newest", 0, 30, 0),
            entry_with("oldest", 0, 10, 0),
            entry_with("middle", 0, 20, 0),
        ];
        super::sort_entries_vec(&mut entries, &settings(super::super::Sort::Modified, false));
        assert_eq!(
            names_of(&entries),
            vec!["oldest", "middle", "newest"],
            "modified sort should be ascending by mtime"
        );

        super::sort_entries_vec(&mut entries, &settings(super::super::Sort::Modified, true));
        assert_eq!(
            names_of(&entries),
            vec!["newest", "middle", "oldest"],
            "inverted modified sort should be descending"
        );
    }
}
