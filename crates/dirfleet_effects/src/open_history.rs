//! Files opened from the app: when each was last opened and how often.
//!
//! As egui's open history: the file system's access time misses opens and
//! goes stale in cached listings, so "Date opened" combines the two. Kept as
//! JSON beside the app's state.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};
use std::time::SystemTime;

use dirfleet_core::listing::Listing;
use serde::{Deserialize, Serialize};

use crate::paths::{canonical_form, normalize_path};
use crate::sort::OpeningHistory;

/// The oldest records are dropped beyond this many files.
const MAX_RECORDS: usize = 10_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenRecord {
    /// Unix time.
    pub last_opened: u32,
    pub count: u32,
}

/// Records grouped by folder, so a listing looks its folder up once and
/// then each entry by name.
#[derive(Default)]
pub struct OpenHistory {
    by_dir: RwLock<HashMap<PathBuf, HashMap<String, OpenRecord>>>,
    file: Option<PathBuf>,
}

/// The folder (as listings name it, symlinks resolved) and the name.
fn split(path: &Path) -> Option<(PathBuf, String)> {
    let path = canonical_form(path);
    let name = path.file_name()?.to_string_lossy().into_owned();
    Some((normalize_path(path.parent()?), name))
}

fn now() -> u32 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|since| u32::try_from(since.as_secs()).ok())
        .unwrap_or_default()
}

impl OpenHistory {
    /// The history kept in `file`, or none kept when there is no file.
    pub fn load(file: Option<PathBuf>) -> Self {
        let records: HashMap<PathBuf, OpenRecord> = file
            .as_deref()
            .and_then(|file| std::fs::read(file).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let mut by_dir: HashMap<PathBuf, HashMap<String, OpenRecord>> = HashMap::new();
        for (path, record) in records {
            if let Some((dir, name)) = split(&path) {
                by_dir.entry(dir).or_default().insert(name, record);
            }
        }
        Self {
            by_dir: RwLock::new(by_dir),
            file,
        }
    }

    pub fn get(&self, path: &Path) -> Option<OpenRecord> {
        let (dir, name) = split(path)?;
        let by_dir = self.by_dir.read().unwrap_or_else(PoisonError::into_inner);
        by_dir.get(&dir)?.get(&name).copied()
    }

    /// Counts one more open of each path, now.
    pub fn record(&self, paths: &[PathBuf]) {
        let now = now();
        self.change(|by_dir| {
            for (dir, name) in paths.iter().filter_map(|path| split(path)) {
                let record = by_dir.entry(dir).or_default().entry(name).or_default();
                record.count = record.count.saturating_add(1);
                record.last_opened = now;
            }
            trim(by_dir);
        });
    }

    pub fn forget(&self, paths: &[PathBuf]) {
        self.change(|by_dir| {
            for (dir, name) in paths.iter().filter_map(|path| split(path)) {
                if let Some(names) = by_dir.get_mut(&dir) {
                    names.remove(&name);
                    if names.is_empty() {
                        by_dir.remove(&dir);
                    }
                }
            }
        });
    }

    /// Moves the records of each `from` (a file, or every file below a
    /// folder) to `to`, as egui's.
    pub fn rename(&self, pairs: &[(PathBuf, PathBuf)]) {
        self.change(|by_dir| {
            for (from, to) in pairs {
                let from = canonical_form(from);
                let moved: Vec<(PathBuf, OpenRecord)> = by_dir
                    .iter()
                    .flat_map(|(dir, names)| {
                        names
                            .iter()
                            .map(move |(name, record)| (dir.join(name), *record))
                    })
                    .filter(|(path, _)| path.starts_with(&from))
                    .collect();
                for (old, record) in moved {
                    let Ok(rest) = old.strip_prefix(&from) else {
                        continue;
                    };
                    let new = if rest.as_os_str().is_empty() {
                        to.clone()
                    } else {
                        to.join(rest)
                    };
                    if let (Some(dir), Some(name)) = (old.parent(), old.file_name())
                        && let Some(names) = by_dir.get_mut(dir)
                    {
                        names.remove(name.to_string_lossy().as_ref());
                        if names.is_empty() {
                            by_dir.remove(dir);
                        }
                    }
                    if let Some((dir, name)) = split(&new) {
                        by_dir.entry(dir).or_default().insert(name, record);
                    }
                }
            }
        });
    }

    pub fn clear(&self) {
        self.change(HashMap::clear);
    }

    fn change(&self, edit: impl FnOnce(&mut HashMap<PathBuf, HashMap<String, OpenRecord>>)) {
        let mut by_dir = self.by_dir.write().unwrap_or_else(PoisonError::into_inner);
        edit(&mut by_dir);
        let Some(file) = &self.file else {
            return;
        };
        let flat: HashMap<PathBuf, OpenRecord> = by_dir
            .iter()
            .flat_map(|(dir, names)| names.iter().map(|(name, record)| (dir.join(name), *record)))
            .collect();
        drop(by_dir);
        match serde_json::to_vec(&flat) {
            Ok(bytes) => {
                if let Err(error) = crate::storage::write_atomically(file, &bytes) {
                    log::warn!("open history not saved: {error}");
                }
            }
            Err(error) => log::warn!("open history not saved: {error}"),
        }
    }

    fn is_empty(&self) -> bool {
        self.by_dir
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    }
}

/// Drops the least recently opened records beyond [`MAX_RECORDS`], and a
/// tenth more so this runs rarely.
fn trim(by_dir: &mut HashMap<PathBuf, HashMap<String, OpenRecord>>) {
    let len: usize = by_dir.values().map(HashMap::len).sum();
    if len <= MAX_RECORDS {
        return;
    }
    let mut all: Vec<(u32, PathBuf, String)> = by_dir
        .iter()
        .flat_map(|(dir, names)| {
            names
                .iter()
                .map(|(name, record)| (record.last_opened, dir.clone(), name.clone()))
        })
        .collect();
    all.sort_unstable();
    for (_, dir, name) in all.into_iter().take(len - MAX_RECORDS + MAX_RECORDS / 10) {
        if let Some(names) = by_dir.get_mut(&dir) {
            names.remove(&name);
        }
    }
    by_dir.retain(|_, names| !names.is_empty());
}

impl OpeningHistory for OpenHistory {
    /// `(times opened, last opened)` for each entry; last opened is the later
    /// of the recorded open and the file system's access time.
    fn keys(&self, listing: &Listing) -> Vec<(u32, u32)> {
        // The listing's folders' records, copied so the lock is held briefly.
        let folders: Vec<Option<HashMap<String, OpenRecord>>> = {
            let by_dir = self.by_dir.read().unwrap_or_else(PoisonError::into_inner);
            if by_dir.is_empty() {
                Vec::new()
            } else {
                listing
                    .dirs()
                    .iter()
                    .map(|dir| {
                        by_dir
                            .get(&canonical_form(Path::new(dir.as_ref())))
                            .cloned()
                    })
                    .collect()
            }
        };
        (0..listing.len())
            .map(|index| {
                let accessed = *listing.meta(index).accessed_at;
                let record = folders
                    .get(listing.dir_index(index))
                    .and_then(Option::as_ref)
                    .and_then(|names| names.get(listing.name(index)));
                record.map_or((0, accessed), |record| {
                    (record.count, record.last_opened.max(accessed))
                })
            })
            .collect()
    }
}

impl OpenHistory {
    /// Each entry's open keys, or none when nothing was ever opened (the
    /// core then falls back to access times).
    pub fn keys_if_any(&self, listing: &Listing) -> Vec<(u32, u32)> {
        if self.is_empty() {
            Vec::new()
        } else {
            self.keys(listing)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_are_counted_kept_and_forgotten() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("open_history.json");
        let doc = dir.path().join("a.txt");
        let history = OpenHistory::load(Some(file.clone()));
        history.record(std::slice::from_ref(&doc));
        history.record(std::slice::from_ref(&doc));
        assert_eq!(history.get(&doc).map(|r| r.count), Some(2));

        let again = OpenHistory::load(Some(file));
        assert_eq!(again.get(&doc).map(|r| r.count), Some(2));
        again.forget(std::slice::from_ref(&doc));
        assert_eq!(again.get(&doc), None);
    }
}
