use std::{
    borrow::Cow,
    cmp::Ordering,
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use crate::{
    app::dock::{build_collator, CurrentPath},
    toast,
    watcher::FileSystemEvent,
};

use super::{dock::TabData, Sort};

impl TabData {
    pub fn set_path(&mut self, path: impl Into<CurrentPath>) -> &CurrentPath {
        let path = path.into();
        self.current_path = path;
        self.start_watching_directory();
        self.refresh_list();
        &self.current_path
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
            // CurrentPath::Multiple(_) => {}
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
    pub fn check_for_file_system_events(&mut self) {
        if let Some(ref mut watcher) = self.watcher {
            let mut should_refresh = false;

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

            // Refresh the directory listing if any events occurred
            if should_refresh {
                self.refresh_list();
            }
        }
    }

    pub fn refresh_list(&mut self) {
        self.list = self.read_dir();
    }

    fn read_dir(&self) -> Vec<super::dock::DirEntry> {
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
        let use_search = self.is_searching();
        // let locations = self.locations.borrow();
        let directories: &[PathBuf] = match &self.current_path {
            CurrentPath::None => &[],
            CurrentPath::One(path_buf) => std::slice::from_ref(path_buf),
            CurrentPath::Multiple(path_bufs) => path_bufs.as_slice(),
        };

        let depth = if use_search {
            self.search.as_ref().map_or(1, |search| search.depth)
        } else {
            1
        };
        let mut dir_entries: Vec<super::dock::DirEntry> = directories
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
                        if !self.settings.show_hidden && (s.starts_with('.') || s.starts_with('$'))
                        {
                            return None;
                        }
                        if search_len > s.len() {
                            return None;
                        } else if !search.is_empty() {
                            let chars = s.as_bytes();
                            let mut found = false;
                            for i in 0..=(s.len() - search_len) {
                                if collator
                                    .compare_utf8(search.as_bytes(), &chars[i..i + search_len])
                                    == Ordering::Equal
                                {
                                    found = true;
                                    break;
                                }
                            }
                            if !found {
                                return None;
                            }
                        }
                        e.try_into().ok()
                    })
                    .collect::<Vec<super::dock::DirEntry>>()
            })
            .collect();
        if self.settings.sorting == Sort::Random {
            use rand::seq::SliceRandom;
            use rand::thread_rng;
            let mut rng = thread_rng();

            dir_entries.shuffle(&mut rng);
            return dir_entries;
        }
        self.sort_entries(&mut dir_entries);
        dir_entries
    }

    fn sort_entries(&self, dir_entries: &mut [super::dock::DirEntry]) {
        dir_entries.sort_by(|a, b| {
            let file_type_cmp = a.is_file().cmp(&b.is_file());

            let result = file_type_cmp.then(match &self.settings.sorting {
                Sort::Name => self.collator.compare(&a.path, &b.path),
                Sort::Modified => a.modified_at.cmp(&b.modified_at),
                Sort::Created => a.created_at.cmp(&b.created_at),
                Sort::Size => a.size.cmp(&b.size),
                Sort::Random => {
                    let name_a = a.get_path().as_os_str().to_ascii_lowercase();
                    let name_b = b.get_path().as_os_str().to_ascii_lowercase();
                    name_a.cmp(&name_b)
                }
            });
            if self.settings.invert_sort {
                file_type_cmp.then(result.reverse())
            } else {
                result
            }
        });
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
