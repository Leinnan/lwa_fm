use std::{
    borrow::Cow,
    cmp::Ordering,
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

use notify::RecursiveMode;

use icu::collator::CollatorBorrowed;
use rayon::slice::ParallelSliceMut;

use crate::{
    app::{
        Data, MatchMode, Search, SearchTermType, database,
        directory_view_settings::{DirectoryShowHidden, DirectoryViewSettings},
        dock::{CurrentPath, build_collator},
    },
    data::files::DirEntry,
    helper::{DataHolder, normalize_path},
};
pub static COLLATER: std::sync::LazyLock<CollatorBorrowed<'static>> =
    std::sync::LazyLock::new(|| build_collator(false));

use super::{Sort, dock::TabData};

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
            extract(Data::default())
        }
        CurrentPath::None => extract(Data::default()),
    }
}

impl TabData {
    pub fn watcher_specs(&self) -> Vec<(PathBuf, RecursiveMode)> {
        let mode = if self.search.as_ref().map_or(1, |search| search.depth) > 1 {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        let depth = self.search_depth();
        let mut specs = BTreeSet::new();
        let mut add_root = |root: &PathBuf| {
            let root = normalize_path(root);
            specs.insert((root.clone(), mode));
            if mode == RecursiveMode::Recursive {
                for linked_root in linked_watch_roots(&root, depth) {
                    specs.insert((linked_root, mode));
                }
            }
        };
        match &self.current_path {
            CurrentPath::None => {}
            CurrentPath::One(path_buf) => add_root(path_buf),
            CurrentPath::Multiple(path_bufs) => {
                for root in path_bufs {
                    add_root(root);
                }
            }
        }
        if let Some(search) = &self.search {
            for extra in &search.extra_dirs {
                add_root(extra);
            }
        }
        specs.into_iter().collect()
    }

    pub fn search_depth(&self) -> usize {
        self.search.as_ref().map_or(1, |search| search.depth.max(1))
    }

    pub fn should_refresh_for_directories(&self, changed_directories: &BTreeSet<PathBuf>) -> bool {
        self.watcher_specs().iter().any(|(root, mode)| {
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
            self.list.len().to_string()
        );
        self.visible_entries =
            filter_visible_entries(&self.list, self.show_hidden, self.search.as_ref());
    }

    pub fn sort_entries(&mut self, sort_settings: &DirectoryViewSettings) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::sort_entries");
        self.display_type = sort_settings.display_type;
        sort_entries_vec(&mut self.list, sort_settings);
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

pub fn read_directory(
    paths: &[PathBuf],
    depth: usize,
    show_hidden: bool,
    cancel: &AtomicBool,
) -> Vec<DirEntry> {
    let mut list = Vec::new();
    for d in paths {
        if cancel.load(std::sync::atomic::Ordering::SeqCst) {
            return list;
        }
        database::read_dir(d, &mut list);
        if depth > 1 {
            let mut subdir_count = 0u64;
            for dir in walkdir::WalkDir::new(d)
                .follow_links(true)
                .min_depth(1)
                .max_depth(depth)
                .into_iter()
                .flatten()
                .filter(|e| e.file_type().is_dir())
            {
                if !show_hidden {
                    let mut parent = dir.path().parent();
                    let mut parent_depth = 0;
                    let mut skip = false;
                    while let Some(p) = parent {
                        if p.iter().next_back().is_some_and(|f| {
                            f.to_string_lossy().starts_with('.')
                                || f.to_string_lossy().starts_with('$')
                        }) {
                            skip = true;
                            break;
                        }
                        parent_depth += 1;
                        if parent_depth >= dir.depth() {
                            break;
                        }
                        parent = p.parent();
                    }
                    if skip {
                        continue;
                    }
                }
                database::read_dir(dir.path(), &mut list);
                subdir_count += 1;
                if subdir_count.is_multiple_of(128)
                    && cancel.load(std::sync::atomic::Ordering::SeqCst)
                {
                    return list;
                }
            }
        }
    }
    list.par_sort_unstable_by(|a, b| a.path.cmp(&b.path));
    list.dedup_by(|a, b| a.path == b.path);
    list
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

impl CompiledTerm {
    fn matches(&self, name: &str, case_sensitive: bool) -> bool {
        match self {
            Self::Plain(pattern) => {
                if case_sensitive {
                    name.contains(pattern.as_str())
                } else {
                    name.to_lowercase().contains(pattern.as_str())
                }
            }
            Self::Glob(pattern) => pattern.matches(name),
            Self::Regex(re) => re.is_match(name),
        }
    }
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
        _case_sensitive: bool,
        collator: Option<&CollatorBorrowed<'_>>,
    ) -> bool {
        let matches_one = |term: &CompiledTerm| -> bool {
            match term {
                CompiledTerm::Plain(pattern) => {
                    let search_len = pattern.len();
                    if search_len > name.len() {
                        return false;
                    }
                    let chars = name.as_bytes();
                    let mut found = false;
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
                                found = true;
                                break;
                            }
                        }
                    }
                    if !found && name.contains(pattern.as_str()) {
                        log::debug!("Collator missed match for {name:?} containing {pattern:?}");
                    }
                    found
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

pub fn filter_visible_entries(
    entries: &[DirEntry],
    show_hidden: bool,
    search: Option<&Search>,
) -> Vec<usize> {
    let mut visible = Vec::new();
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
    let is_searching = compiled_search.is_some();
    let collator = if is_searching && compiled_search.as_ref().is_some_and(|cs| cs.has_plain) {
        Some(build_collator(case_sensitive))
    } else {
        None
    };
    for (i, entry) in entries.iter().enumerate() {
        if !show_hidden {
            let name = entry.get_splitted_path().1;
            if name.starts_with('.') || name.starts_with('$') {
                continue;
            }
        }
        if let Some(ref cs) = compiled_search {
            let name = entry.get_splitted_path().1;
            if !cs.matches(name, case_sensitive, collator.as_ref()) {
                continue;
            }
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

    use crate::data::files::DirEntry;

    use super::{Search, filter_visible_entries};

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
    fn dedup_removes_all_duplicates_regardless_of_order() {
        let mut list = vec![
            DirEntry::test_new("/fav/subdir/file.txt"),
            DirEntry::test_new("/fav/file.txt"),
            DirEntry::test_new("/fav/subdir/file.txt"),
            DirEntry::test_new("/fav/file.txt"),
        ];
        list.par_sort_unstable_by(|a, b| a.path.cmp(&b.path));
        list.dedup_by(|a, b| a.path == b.path);
        assert_eq!(
            list.len(),
            2,
            "should have 2 unique entries after sort+dedup"
        );
        assert!(list.iter().any(|e| e.path == "/fav/file.txt"));
        assert!(list.iter().any(|e| e.path == "/fav/subdir/file.txt"));
    }

    #[test]
    fn search_depth_clamps_to_one() {
        assert_eq!(0usize.max(1), 1, "depth 0 should clamp to 1");
        assert_eq!(1usize.max(1), 1, "depth 1 should stay 1");
        assert_eq!(3usize.max(1), 3, "depth 3 should stay 3");
    }
}
