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
        directory_view_settings::{DirectoryShowHidden, DirectoryViewSettings},
        dock::{build_collator, CurrentPath},
    },
    helper::DataHolder,
    toast,
    watcher::FileSystemEvent,
};
pub static COLLATER: std::sync::LazyLock<CollatorBorrowed<'static>> =
    std::sync::LazyLock::new(|| build_collator(false));

use super::{dock::TabData, Sort};

impl TabData {
    pub fn set_path(&mut self, path: impl Into<CurrentPath>) -> &CurrentPath {
        let path = path.into();
        self.current_path = path;
        self.start_watching_directory();
        // self.action_to_perform = Some(ActionToPerform::RequestFilesRefresh);
        &self.current_path
    }

    pub fn update_settings(&mut self, data_source: &impl DataHolder) {
        self.show_hidden = data_source
            .data_get_path_or_persisted::<DirectoryShowHidden>(&self.current_path)
            .0;
    }

    /// Start watching the current directory for changes
    pub fn start_watching_directory(&mut self) {
        let Some(ref mut watcher) = self.watcher else {
            return;
        };
        match &self.current_path {
            CurrentPath::One(path_buf) => {
                if let Err(e) = watcher.watch_directory(path_buf) {
                    toast!(Error, "Failed to watch directory: {}", e);
                }
            }
            _ => {
                watcher.stop_watching();
            }
        }
    }

    #[allow(unused)]
    /// Stop watching the current directory
    pub fn stop_watching_directory(&mut self) {
        if let Some(ref mut watcher) = self.watcher {
            watcher.stop_watching();
        }
    }
    /// Check for file system events and handle them
    pub fn check_for_file_system_events(&mut self) -> bool {
        let mut should_refresh = false;
        if let Some(ref mut watcher) = self.watcher {
            // Process all pending events
            while let Some(event) = watcher.try_recv_event() {
                should_refresh = true;
                match event {
                    FileSystemEvent::Created(path) => {
                        if let Some(file_name) = path.file_name() {
                            toast!(Info, "File created: {}", file_name.to_string_lossy());
                        }
                    }
                    FileSystemEvent::Modified(path) => {
                        if let Some(file_name) = path.file_name() {
                            toast!(Info, "File modified: {}", file_name.to_string_lossy());
                        }
                    }
                    FileSystemEvent::Deleted(path) => {
                        if let Some(file_name) = path.file_name() {
                            toast!(Warning, "File deleted: {}", file_name.to_string_lossy());
                        }
                    }
                    FileSystemEvent::Renamed { from, to } => {
                        if let (Some(from_name), Some(to_name)) = (from.file_name(), to.file_name())
                        {
                            toast!(
                                Info,
                                "File renamed: {} → {}",
                                from_name.to_string_lossy(),
                                to_name.to_string_lossy()
                            );
                        }
                    }
                    FileSystemEvent::Error(err) => {
                        toast!(Error, "File system error: {}", err);
                    }
                }
            }
        }
        should_refresh
    }

    pub fn refresh_list(&mut self) {
        if self.is_searching() {
            self.read_dir_filter();
        } else {
            self.read_dir();
        }
    }

    fn read_dir_filter(&mut self) {
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
        let directories: &[PathBuf] = match &self.current_path {
            CurrentPath::None => &[],
            CurrentPath::One(path_buf) => std::slice::from_ref(path_buf),
            CurrentPath::Multiple(path_bufs) => path_bufs.as_slice(),
        };

        let depth = self.search.as_ref().map_or(1, |search| search.depth);
        self.list = directories
            .iter()
            .flat_map(|d| {
                walkdir::WalkDir::new(d)
                    .follow_links(true)
                    .max_depth(depth)
                    .into_iter()
                    .flatten()
                    .skip(1)
                    .filter_map(|e| {
                        let s = e.file_name().to_string_lossy();
                        if !self.show_hidden && (s.starts_with('.') || s.starts_with('$')) {
                            return None;
                        }
                        if search_len > s.len() {
                            return None;
                        }
                        let chars = s.as_bytes();
                        let mut found = false;
                        for i in 0..=(s.len() - search_len) {
                            if collator.compare_utf8(search.as_bytes(), &chars[i..i + search_len])
                                == Ordering::Equal
                            {
                                found = true;
                                break;
                            }
                        }
                        if !found {
                            return None;
                        }
                        e.try_into().ok()
                    })
                    .collect::<Vec<super::dock::DirEntry>>()
            })
            .collect();
    }

    fn read_dir(&mut self) {
        let directories: &[PathBuf] = match &self.current_path {
            CurrentPath::None => &[],
            CurrentPath::One(path_buf) => std::slice::from_ref(path_buf),
            CurrentPath::Multiple(path_bufs) => path_bufs.as_slice(),
        };

        self.list.clear();

        if self.show_hidden {
            for d in directories {
                let Ok(paths) = std::fs::read_dir(d) else {
                    continue;
                };
                self.list.par_extend(paths.par_bridge().filter_map(|e| {
                    let e = e.ok()?;
                    e.try_into().ok()
                }));
            }
        } else {
            for d in directories {
                let Ok(paths) = std::fs::read_dir(d) else {
                    continue;
                };
                self.list.par_extend(paths.par_bridge().filter_map(|e| {
                    let e = e.ok()?;
                    let s = e.file_name().as_encoded_bytes()[0];
                    if s.eq(&b'.') || s.eq(&b'$') {
                        return None;
                    }
                    e.try_into().ok()
                }));
            }
        }
    }

    pub fn sort_entries(&mut self, sort_settings: &DirectoryViewSettings) {
        match sort_settings.sorting {
            Sort::Name => {
                let collator = &COLLATER;
                if sort_settings.invert_sort {
                    self.list.par_sort_by(|a, b| {
                        let file_type_cmp = a.is_file().cmp(&b.is_file());
                        file_type_cmp.then(collator.compare(&b.path, &a.path))
                    });
                } else {
                    self.list.par_sort_by(|a, b| {
                        let file_type_cmp = a.is_file().cmp(&b.is_file());
                        file_type_cmp.then(collator.compare(&a.path, &b.path))
                        // file_type_cmp.then(a.path.cmp(&b.path))
                    });
                }
            }
            Sort::Modified => {
                if sort_settings.invert_sort {
                    self.list.sort_by(|a, b| {
                        let file_type_cmp = a.is_file().cmp(&b.is_file());
                        file_type_cmp.then(b.modified_at.cmp(&a.modified_at))
                    });
                } else {
                    self.list.sort_by(|a, b| {
                        let file_type_cmp = a.is_file().cmp(&b.is_file());
                        file_type_cmp.then(a.modified_at.cmp(&b.modified_at))
                    });
                }
            }
            Sort::Created => {
                if sort_settings.invert_sort {
                    self.list.sort_by(|a, b| {
                        let file_type_cmp = a.is_file().cmp(&b.is_file());
                        file_type_cmp.then(b.created_at.cmp(&a.created_at))
                    });
                } else {
                    self.list.sort_by(|a, b| {
                        let file_type_cmp = a.is_file().cmp(&b.is_file());
                        file_type_cmp.then(a.created_at.cmp(&b.created_at))
                    });
                }
            }
            Sort::Size => {
                if sort_settings.invert_sort {
                    self.list.sort_by(|a, b| {
                        let file_type_cmp = a.is_file().cmp(&b.is_file());
                        file_type_cmp.then(b.size.cmp(&a.size))
                    });
                } else {
                    self.list.sort_by(|a, b| {
                        let file_type_cmp = a.is_file().cmp(&b.is_file());
                        file_type_cmp.then(a.size.cmp(&b.size))
                    });
                }
            }
            Sort::Random => {
                use rand::seq::SliceRandom;
                use rand::thread_rng;
                let mut rng = thread_rng();

                self.list.shuffle(&mut rng);
            }
        }
    }
}

pub fn get_directories(path: &Path, show_hidden: bool) -> BTreeSet<Cow<'static, str>> {
    get_directories_recursive(path, show_hidden, 2)
}

pub fn get_directories_recursive(
    path: &Path,
    show_hidden: bool,
    depth: usize,
) -> BTreeSet<Cow<'static, str>> {
    let directories = walkdir::WalkDir::new(path)
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
                if let Some(parent_name) = parent.file_name().and_then(|name| name.to_str()) {
                    if (parent_name.starts_with('.') || parent_name.starts_with('$'))
                        && !parent.eq(path)
                    {
                        return false;
                    }
                }
                current_path = parent.parent(); // Move to the next parent
            }
            true
        })
        .map(|e| format!("{}", e.path().display()).into())
        .collect::<BTreeSet<_>>();
    directories
}
