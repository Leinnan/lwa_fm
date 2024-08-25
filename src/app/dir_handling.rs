use std::{cmp::Ordering, ffi::OsStr, path::PathBuf};
use walkdir::{DirEntry, WalkDir};

use crate::toast;

use super::{App, Sort};

impl App {
    pub(crate) fn change_current_dir(&mut self, new_path: PathBuf) {
        self.cur_path = new_path;
        if !self.search.value.is_empty() {
            self.search.value = String::new();
        }
        self.refresh_list();
    }

    pub(crate) fn refresh_list(&mut self) {
        self.list = self.read_dir();
        self.dir_has_cargo = self
            .list
            .iter()
            .any(|entry| entry.file_name().eq(OsStr::new("Cargo.toml")));
    }

    fn read_dir(&self) -> Vec<walkdir::DirEntry> {
        let search = &self.search.value;
        let use_search = !self.search.value.is_empty();
        let directories = if use_search && self.search.favorites {
            self.locations
                .get("Favorites")
                .map_or_else(Vec::new, |favorites| {
                    favorites
                        .locations
                        .iter()
                        .map(|location| &location.path)
                        .collect()
                })
        } else {
            [&self.cur_path].to_vec()
        };

        let depth = if use_search { self.search.depth } else { 1 };
        let mut dir_entries: Vec<walkdir::DirEntry> = directories
            .iter()
            .flat_map(|d| {
                WalkDir::new(d)
                    .follow_links(true)
                    .max_depth(depth)
                    .into_iter()
                    .flatten()
                    .skip(1)
                    .filter(|e| {
                        let s = e.file_name().to_string_lossy();
                        if !self.show_hidden && (s.starts_with('.') || s.starts_with('$')) {
                            return false;
                        }
                        if self.search.case_sensitive {
                            s.contains(search)
                        } else {
                            s.to_ascii_lowercase()
                                .contains(&search.to_ascii_lowercase())
                        }
                    })
                    .collect::<Vec<walkdir::DirEntry>>()
            })
            .collect();
        if self.sorting == Sort::Random {
            use rand::seq::SliceRandom;
            use rand::thread_rng;
            let mut rng = thread_rng();

            dir_entries.shuffle(&mut rng);
            return dir_entries;
        }
        self.sort_entries(&mut dir_entries);
        if self.invert_sort {
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

            file_type_cmp.then(match &self.sorting {
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

        // Ok(())
    }
}
