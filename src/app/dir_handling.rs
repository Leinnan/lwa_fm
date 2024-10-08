use std::{
    cmp::Ordering,
    collections::BTreeSet,
    ffi::OsStr,
    path::{Path, PathBuf},
};
use walkdir::DirEntry;

use crate::toast;

use super::{dock::TabData, Sort};

impl TabData {
    pub fn get_name_from_path(path: &Path) -> String {
        #[cfg(not(windows))]
        {
            path.iter()
                .last()
                .expect("FAILED")
                .to_string_lossy()
                .into_owned()
        }
        #[cfg(windows)]
        {
            let mut result = path
                .iter()
                .last()
                .expect("FAILED")
                .to_string_lossy()
                .into_owned();
            if result.len() == 1 {
                result = path.display().to_string();
            }
            result
        }
    }
    pub fn set_path(&mut self, path: &PathBuf) {
        self.name = Self::get_name_from_path(path);
        self.current_path.clone_from(path);
        self.path_change = None;
        if self.settings.is_searching() {
            self.settings.search.value = String::new();
            self.settings.search.visible = false;
        }
        self.refresh_list();
    }

    pub fn refresh_list(&mut self) {
        self.list = self.read_dir();
        self.dir_has_cargo = self
            .list
            .iter()
            .any(|entry| entry.file_name().eq(OsStr::new("Cargo.toml")));
    }

    fn read_dir(&self) -> Vec<walkdir::DirEntry> {
        let search = &self.settings.search.value;
        let use_search = self.settings.is_searching();
        let locations = self.locations.borrow();
        let directories = if use_search && self.settings.search.favorites {
            locations
                .get("Favorites")
                .map_or_else(Vec::new, |favorites| {
                    favorites
                        .locations
                        .iter()
                        .map(|location| &location.path)
                        .collect()
                })
        } else {
            [&self.current_path].to_vec()
        };

        let depth = if use_search {
            self.settings.search.depth
        } else {
            1
        };
        let mut dir_entries: Vec<walkdir::DirEntry> = directories
            .iter()
            .flat_map(|d| {
                walkdir::WalkDir::new(d)
                    .follow_links(true)
                    .max_depth(depth)
                    .into_iter()
                    .flatten()
                    .skip(1)
                    .filter(|e| {
                        let s = e.file_name().to_string_lossy();
                        if !self.settings.show_hidden && (s.starts_with('.') || s.starts_with('$'))
                        {
                            return false;
                        }
                        if self.settings.search.case_sensitive {
                            s.contains(search)
                        } else {
                            s.to_ascii_lowercase()
                                .contains(&search.to_ascii_lowercase())
                        }
                    })
                    .collect::<Vec<walkdir::DirEntry>>()
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
        if self.settings.invert_sort {
            dir_entries.reverse();
        }
        dir_entries
    }

    fn sort_entries(&self, dir_entries: &mut [DirEntry]) {
        dir_entries.sort_by(|a, b| {
            // Extract metadata for both entries and handle errors
            let metadata_a = a.metadata();
            let metadata_b = b.metadata();

            let file_type_cmp = a.file_type().is_file().cmp(&b.file_type().is_file());

            file_type_cmp.then(match &self.settings.sorting {
                Sort::Random => {
                    toast!(Info, "Random sort is not supported");
                    let name_a = a.file_name().to_ascii_lowercase();
                    let name_b = b.file_name().to_ascii_lowercase();
                    name_a.cmp(&name_b)
                }
                Sort::Name => {
                    let name_a = a.file_name().to_ascii_lowercase();
                    let name_b = b.file_name().to_ascii_lowercase();
                    name_a.cmp(&name_b)
                }
                Sort::Modified => match (metadata_a, metadata_b) {
                    (Ok(meta_a), Ok(meta_b)) => match (meta_a.modified(), meta_b.modified()) {
                        (Ok(time_a), Ok(time_b)) => time_a.cmp(&time_b),
                        (Err(_), _) | (_, Err(_)) => Ordering::Equal,
                    },
                    _ => Ordering::Equal,
                },
                Sort::Created => match (metadata_a, metadata_b) {
                    (Ok(meta_a), Ok(meta_b)) => match (meta_a.created(), meta_b.created()) {
                        (Ok(time_a), Ok(time_b)) => time_a.cmp(&time_b),
                        (Err(_), _) | (_, Err(_)) => Ordering::Equal,
                    },
                    _ => Ordering::Equal,
                },
                #[cfg(windows)]
                Sort::Size => match (metadata_a, metadata_b) {
                    (Ok(meta_a), Ok(meta_b)) => {
                        let size_a = std::os::windows::fs::MetadataExt::file_size(&meta_a);
                        let size_b = std::os::windows::fs::MetadataExt::file_size(&meta_b);
                        size_a.cmp(&size_b)
                    }
                    _ => Ordering::Equal,
                },
                #[cfg(target_os = "linux")]
                Sort::Size => match (metadata_a, metadata_b) {
                    (Ok(meta_a), Ok(meta_b)) => {
                        let size_a = std::os::linux::fs::MetadataExt::st_size(&meta_a);
                        let size_b = std::os::linux::fs::MetadataExt::st_size(&meta_b);
                        size_a.cmp(&size_b)
                    }
                    _ => Ordering::Equal,
                },
                #[cfg(target_os = "macos")]
                Sort::Size => match (metadata_a, metadata_b) {
                    (Ok(meta_a), Ok(meta_b)) => {
                        let size_a = std::os::macos::fs::MetadataExt::st_size(&meta_a);
                        let size_b = std::os::macos::fs::MetadataExt::st_size(&meta_b);
                        size_a.cmp(&size_b)
                    }
                    _ => Ordering::Equal,
                },
            })
        });
    }
}

pub fn get_directories(path: &Path, show_hidden: bool) -> BTreeSet<String> {
    get_directories_recursive(path, show_hidden, 2)
}

pub fn get_directories_recursive(path: &Path, show_hidden: bool, depth: usize) -> BTreeSet<String> {
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
        .map(|e| format!("{}", e.path().display()))
        .collect::<BTreeSet<_>>();
    directories
}
