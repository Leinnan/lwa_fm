use std::{
    borrow::Cow,
    cmp::Ordering,
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use icu::collator::CollatorBorrowed;
use rayon::{
    iter::{ParallelBridge, ParallelExtend, ParallelIterator},
    slice::ParallelSliceMut,
};

use crate::{
    app::{
        database,
        directory_view_settings::{DirectoryShowHidden, DirectoryViewSettings},
        dock::{CurrentPath, build_collator},
    },
    data::files::DirEntry,
    helper::DataHolder,
};
pub static COLLATER: std::sync::LazyLock<CollatorBorrowed<'static>> =
    std::sync::LazyLock::new(|| build_collator(false));

use super::{Sort, dock::TabData};

impl TabData {
    pub fn set_path(&mut self, path: impl Into<CurrentPath>) -> &CurrentPath {
        let path = path.into();
        self.current_path = path;
        if let Some(path) = self.current_path.get_path() {
            self.top_display_path.build(&path, self.show_hidden);
        }
        // self.start_watching_directory();
        // self.action_to_perform = Some(ActionToPerform::RequestFilesRefresh);
        &self.current_path
    }

    pub fn update_settings(&mut self, data_source: &impl DataHolder) {
        self.show_hidden = data_source
            .data_get_path_or_persisted::<DirectoryShowHidden>(&self.current_path)
            .0;
        if let Some(path) = self.current_path.get_path() {
            self.top_display_path.build(&path, self.show_hidden);
        }
    }

    // /// Start watching the current directory for changes
    // pub fn start_watching_directory(&mut self) {
    //     #[cfg(feature = "profiling")]
    //     puffin::profile_scope!("lwa_fm::dir_handling::start_watching_directory");
    //     let Some(ref mut watcher) = self.watcher else {
    //         return;
    //     };
    //     match &self.current_path {
    //         CurrentPath::One(path_buf) => {
    //             if let Err(e) = watcher.watch_directory(path_buf) {
    //                 toast!(Error, "Failed to watch directory: {}", e);
    //             }
    //         }
    //         _ => {
    //             watcher.stop_watching();
    //         }
    //     }
    // }

    // #[allow(unused)]
    // /// Stop watching the current directory
    // pub fn stop_watching_directory(&mut self) {
    //     if let Some(ref mut watcher) = self.watcher {
    //         watcher.stop_watching();
    //     }
    // }
    // /// Check for file system events and handle them
    // pub fn check_for_file_system_events(&mut self) -> bool {
    //     let mut should_refresh = false;
    //     if let Some(ref mut watcher) = self.watcher {
    //         // Process all pending events
    //         while let Some(event) = watcher.try_recv_event() {
    //             should_refresh = true;
    //             match event {
    //                 FileSystemEvent::Created(path) => {
    //                     if let Some(file_name) = path.file_name() {
    //                         toast!(Info, "File created: {}", file_name.to_string_lossy());
    //                     }
    //                 }
    //                 FileSystemEvent::Modified(path) => {
    //                     if let Some(file_name) = path.file_name() {
    //                         toast!(Info, "File modified: {}", file_name.to_string_lossy());
    //                     }
    //                 }
    //                 FileSystemEvent::Deleted(path) => {
    //                     if let Some(file_name) = path.file_name() {
    //                         toast!(Warning, "File deleted: {}", file_name.to_string_lossy());
    //                     }
    //                 }
    //                 FileSystemEvent::Renamed { from, to } => {
    //                     if let (Some(from_name), Some(to_name)) = (from.file_name(), to.file_name())
    //                     {
    //                         toast!(
    //                             Info,
    //                             "File renamed: {} → {}",
    //                             from_name.to_string_lossy(),
    //                             to_name.to_string_lossy()
    //                         );
    //                     }
    //                 }
    //                 FileSystemEvent::Error(err) => {
    //                     toast!(Error, "File system error: {}", err);
    //                 }
    //             }
    //         }
    //     }
    //     should_refresh
    // }

    pub fn refresh_list(&mut self) {
        self.read_dir();
        self.update_visible_entries();
        // if self.is_searching() {
        //     self.read_dir_filter();
        // } else {
        //     self.read_dir();
        // }
    }

    // pub fn get_visible_entries(&self) -> impl Iterator<Item = &DirEntry> {
    //     self.visible_entries.iter().map(|&idx| &self.list[idx])
    // }
    pub fn update_visible_entries(&mut self) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!(
            "dir_handling::update_visible_entries",
            self.list.len().to_string()
        );
        self.visible_entries.clear();
        let is_searching = self.is_searching();
        let case_sensitive = self
            .search
            .as_ref()
            .is_some_and(|search| search.case_sensitive);
        let search = self
            .search
            .as_ref()
            .map(|search| search.value.as_str())
            .unwrap_or_default();
        let search_len = search.len();
        let collator = build_collator(case_sensitive);
        for (i, entry) in self.list.iter().enumerate() {
            if !self.show_hidden {
                let name = entry.get_splitted_path().1;
                if name.starts_with(".") || name.starts_with("$") {
                    continue;
                }
            }
            if is_searching {
                let name = entry.get_splitted_path().1;
                if search_len > name.len() {
                    continue;
                }
                let chars = name.as_bytes();
                let mut found = false;
                for i in 0..=(name.len() - search_len) {
                    if collator.compare_utf8(search.as_bytes(), &chars[i..i + search_len])
                        == Ordering::Equal
                    {
                        found = true;
                        log::error!(
                            "MATCH: {}",
                            name[i..i + search_len].escape_debug().to_string()
                        );
                        break;
                    }
                }
                if !found {
                    if name.contains(search) {
                        log::error!("WTF: {}", name);
                    } else {
                        continue;
                    }
                }
            }
            self.visible_entries.push(i);
        }
    }

    fn read_dir(&mut self) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::read_dir");
        self.list.clear();
        let directories: &[PathBuf] = match &self.current_path {
            CurrentPath::None => &[],
            CurrentPath::One(path_buf) => {
                // let paths = {
                //     #[cfg(feature = "profiling")]
                //     puffin::profile_scope!("lwa_fm::dir_handling::read_dir::with_hidden::dir");
                //     std::fs::read_dir(path_buf)
                // };
                // let Ok(paths) = paths else {
                //     return;
                // };
                #[cfg(feature = "profiling")]
                puffin::profile_scope!("lwa_fm::dir_handling::read_dir::with_hidden::mapping");
                // // self.list
                // //     .extend(paths.filter_map(|e| e.ok().and_then(|e| e.try_into().ok())));
                // self.list.par_extend(paths.par_bridge().filter_map(|e| {
                //     let e = e.ok()?;
                //     e.try_into().ok()
                // }));
                database::read_dir(path_buf, &mut self.list);
                // #[cfg(feature = "profiling")]
                // puffin::profile_scope!("lwa_fm::dir_handling::read_dir::meta");
                // self.list.par_iter_mut().for_each(|e| e.read_metadata());
                return;
            }
            CurrentPath::Multiple(path_bufs) => path_bufs.as_slice(),
        };
        let depth = self.search.as_ref().map_or(1, |search| search.depth);
        eprintln!("DEPTH: {}", depth);
        for d in directories {
            self.list.extend(
                walkdir::WalkDir::new(d)
                    .follow_links(true)
                    .max_depth(depth)
                    .into_iter()
                    .flatten()
                    .skip(1)
                    .filter_map(|e| e.try_into().ok()),
            );
            let paths = {
                #[cfg(feature = "profiling")]
                puffin::profile_scope!("lwa_fm::dir_handling::read_dir::with_hidden::dir");
                std::fs::read_dir(d)
            };
            let Ok(paths) = paths else {
                continue;
            };

            #[cfg(feature = "profiling")]
            puffin::profile_scope!("lwa_fm::dir_handling::read_dir::with_hidden::mapping");
            self.list.par_extend(paths.par_bridge().filter_map(|e| {
                let e = e.ok()?;
                e.try_into().ok()
            }));
        }
    }

    pub fn sort_entries(&mut self, sort_settings: &DirectoryViewSettings) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::sort_entries");
        let sort_fn: fn(&DirEntry, &DirEntry) -> Ordering = match sort_settings.sorting {
            Sort::Modified => {
                if sort_settings.invert_sort {
                    |a, b| {
                        let file_type_cmp = a.is_file().cmp(&b.is_file());
                        file_type_cmp.then(b.meta.modified_at.cmp(&a.meta.modified_at))
                    }
                } else {
                    |a, b| {
                        let file_type_cmp = a.is_file().cmp(&b.is_file());
                        file_type_cmp.then(a.meta.modified_at.cmp(&b.meta.modified_at))
                    }
                }
            }
            Sort::Name => {
                if sort_settings.invert_sort {
                    |a, b| b.sort_key.compare(&a.sort_key)
                } else {
                    |a, b| a.sort_key.compare(&b.sort_key)
                }
            }
            Sort::Created => {
                if sort_settings.invert_sort {
                    |a, b| {
                        let file_type_cmp = a.is_file().cmp(&b.is_file());
                        file_type_cmp.then(b.meta.created_at.cmp(&a.meta.created_at))
                    }
                } else {
                    |a, b| {
                        let file_type_cmp = a.is_file().cmp(&b.is_file());
                        file_type_cmp.then(a.meta.created_at.cmp(&b.meta.created_at))
                    }
                }
            }
            Sort::Size => {
                if sort_settings.invert_sort {
                    |a, b| {
                        let file_type_cmp = a.is_file().cmp(&b.is_file());
                        file_type_cmp.then(b.meta.size.cmp(&a.meta.size))
                    }
                } else {
                    |a, b| {
                        let file_type_cmp = a.is_file().cmp(&b.is_file());
                        file_type_cmp.then(a.meta.size.cmp(&b.meta.size))
                    }
                }
            }
            Sort::Random => {
                use rand::seq::SliceRandom;
                use rand::thread_rng;
                let mut rng = thread_rng();

                self.list.shuffle(&mut rng);
                return;
            }
        };
        self.list.par_sort_unstable_by(sort_fn);
    }
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
