//! Files opened from the app: when they were last opened and how often.
//!
//! The filesystem's last access time misses opens made within an hour of the
//! previous access (NTFS only writes it once per hour) and cached listings keep
//! the old value, so "Date opened" combines it with this history.
use crate::data::{files::EntryRef, time::TimestampSeconds};
use crate::helper::canonical_form;
use bincode::{Decode, Encode};
use foldhash::{HashMap, HashMapExt};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    sync::{LazyLock, RwLock},
    time::SystemTime,
};

/// Oldest records are dropped beyond this many files.
const MAX_RECORDS: usize = 10_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Encode, Decode)]
pub struct OpenRecord {
    pub last_opened: TimestampSeconds,
    pub count: u32,
}

/// Records grouped by folder, so a listing looks its folder up once and then
/// each entry by name.
struct OpenHistory {
    tree: Option<sled::Tree>,
    by_dir: HashMap<PathBuf, HashMap<String, OpenRecord>>,
    len: usize,
}

impl OpenHistory {
    fn load(tree: Option<sled::Tree>) -> Self {
        let mut history = Self {
            tree,
            by_dir: HashMap::new(),
            len: 0,
        };
        let records: Vec<(PathBuf, OpenRecord)> = history
            .tree
            .iter()
            .flat_map(sled::Tree::iter)
            .filter_map(Result::ok)
            .filter_map(|(key, value)| {
                let path = PathBuf::from(std::str::from_utf8(&key).ok()?);
                let (record, _) =
                    bincode::decode_from_slice(&value, bincode::config::standard()).ok()?;
                Some((path, record))
            })
            .collect();
        for (path, record) in records {
            history.insert_in_memory(&path, record);
        }
        history
    }

    fn split(path: &Path) -> Option<(PathBuf, String)> {
        let path = canonical_form(path);
        let name = path.file_name()?.to_string_lossy().into_owned();
        Some((path.parent()?.to_path_buf(), name))
    }

    fn get(&self, path: &Path) -> Option<OpenRecord> {
        let (dir, name) = Self::split(path)?;
        self.by_dir.get(&dir)?.get(&name).copied()
    }

    fn insert_in_memory(&mut self, path: &Path, record: OpenRecord) {
        let Some((dir, name)) = Self::split(path) else {
            return;
        };
        if self.by_dir.entry(dir).or_default().insert(name, record).is_none() {
            self.len += 1;
        }
    }

    fn remove_in_memory(&mut self, path: &Path) -> Option<OpenRecord> {
        let (dir, name) = Self::split(path)?;
        let names = self.by_dir.get_mut(&dir)?;
        let removed = names.remove(&name)?;
        if names.is_empty() {
            self.by_dir.remove(&dir);
        }
        self.len -= 1;
        Some(removed)
    }

    fn key(path: &Path) -> Vec<u8> {
        canonical_form(path).to_string_lossy().into_owned().into_bytes()
    }

    fn put(&mut self, path: &Path, record: OpenRecord) {
        self.insert_in_memory(path, record);
        if let Some(tree) = &self.tree
            && let Ok(bytes) = bincode::encode_to_vec(record, bincode::config::standard())
        {
            _ = tree.insert(Self::key(path), bytes);
        }
    }

    fn remove(&mut self, path: &Path) -> Option<OpenRecord> {
        if let Some(tree) = &self.tree {
            _ = tree.remove(Self::key(path));
        }
        self.remove_in_memory(path)
    }

    fn record(&mut self, path: &Path, now: TimestampSeconds) {
        let previous = self.get(path).unwrap_or_default();
        self.put(
            path,
            OpenRecord {
                last_opened: now,
                count: previous.count.saturating_add(1),
            },
        );
        self.trim();
    }

    /// Drops the least recently opened records beyond [`MAX_RECORDS`].
    fn trim(&mut self) {
        if self.len <= MAX_RECORDS {
            return;
        }
        let mut all: Vec<(TimestampSeconds, PathBuf)> = self
            .by_dir
            .iter()
            .flat_map(|(dir, names)| {
                names
                    .iter()
                    .map(|(name, record)| (record.last_opened, dir.join(name)))
            })
            .collect();
        all.sort_unstable();
        // Trim a little further than needed so this runs rarely.
        let excess = self.len - MAX_RECORDS + MAX_RECORDS / 10;
        for (_, path) in all.into_iter().take(excess) {
            self.remove(&path);
        }
    }

    fn clear(&mut self) {
        if let Some(tree) = &self.tree {
            _ = tree.clear();
        }
        self.by_dir.clear();
        self.len = 0;
    }
}

static HISTORY: LazyLock<RwLock<OpenHistory>> = LazyLock::new(|| {
    #[cfg(not(test))]
    let tree = crate::app::database::SLED_DIRS.open_tree("open_history").ok();
    // Tests never touch the real history.
    #[cfg(test)]
    let tree = sled::Config::new()
        .temporary(true)
        .open()
        .ok()
        .and_then(|db| db.open_tree("open_history").ok());
    RwLock::new(OpenHistory::load(tree))
});

/// Bumped on every change, so views can tell their order may be stale.
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn changed() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

static TRACKING: AtomicBool = AtomicBool::new(true);

/// Turns recording on or off (the "Remember opened files" setting).
pub fn set_tracking(enabled: bool) {
    TRACKING.store(enabled, Ordering::Relaxed);
}

/// Notes that the user opened `path`, when it is a file and recording is on.
pub fn opened(path: &Path) {
    if TRACKING.load(Ordering::Relaxed) && path.is_file() {
        record(path);
    }
}

/// Notes that `path` was just opened.
pub fn record(path: &Path) {
    let now = TimestampSeconds::from(SystemTime::now());
    if let Ok(mut history) = HISTORY.write() {
        history.record(path, now);
        changed();
    }
}

#[must_use]
pub fn get(path: &Path) -> Option<OpenRecord> {
    HISTORY.read().ok()?.get(path)
}

/// The record of a listed entry; cheap when the history is empty.
#[must_use]
pub fn of_entry(entry: &EntryRef<'_>) -> Option<OpenRecord> {
    let history = HISTORY.read().ok()?;
    if history.len == 0 {
        return None;
    }
    history
        .by_dir
        .get(&canonical_form(Path::new(entry.dir.as_ref())))?
        .get(entry.file_name)
        .copied()
}

/// Forgets `paths`; returns whether any had a record.
pub fn forget<'a>(paths: impl IntoIterator<Item = &'a Path>) -> bool {
    let Ok(mut history) = HISTORY.write() else {
        return false;
    };
    let mut any = false;
    for path in paths {
        any |= history.remove(path).is_some();
    }
    if any {
        changed();
    }
    any
}

/// Moves the record of `from` (a file, or every file below a folder) to `to`.
pub fn rename(from: &Path, to: &Path) {
    let Ok(mut history) = HISTORY.write() else {
        return;
    };
    let from = canonical_form(from);
    let moved: Vec<(PathBuf, OpenRecord)> = history
        .by_dir
        .iter()
        .flat_map(|(dir, names)| {
            names
                .iter()
                .map(move |(name, record)| (dir.join(name), *record))
        })
        .filter(|(path, _)| path.starts_with(&from))
        .collect();
    if moved.is_empty() {
        return;
    }
    for (old, record) in moved {
        let Ok(rest) = old.strip_prefix(&from) else {
            continue;
        };
        let new = if rest.as_os_str().is_empty() {
            to.to_path_buf()
        } else {
            to.join(rest)
        };
        history.remove(&old);
        history.put(&new, record);
    }
    changed();
}

pub fn clear() {
    if let Ok(mut history) = HISTORY.write() {
        history.clear();
        changed();
    }
}

#[must_use]
pub fn is_empty() -> bool {
    HISTORY.read().map_or(true, |history| history.len == 0)
}

/// The records of every file in `dir`, by name; `None` when there are none.
/// Lets a whole listing be looked up with one lock and one path lookup.
#[must_use]
pub fn records_in(dir: &Path) -> Option<HashMap<String, OpenRecord>> {
    let history = HISTORY.read().ok()?;
    if history.len == 0 {
        return None;
    }
    history.by_dir.get(&canonical_form(dir)).cloned()
}

/// "Date opened": the later of the app's record and the filesystem's last
/// access time.
#[must_use]
pub fn last_opened(record: Option<OpenRecord>, accessed: TimestampSeconds) -> TimestampSeconds {
    record.map_or(accessed, |record| record.last_opened.max(accessed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_history() -> OpenHistory {
        let db = sled::Config::new().temporary(true).open().expect("temp db");
        OpenHistory::load(db.open_tree("open_history").ok())
    }

    fn ts(secs: u64) -> TimestampSeconds {
        TimestampSeconds::from(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
    }

    #[test]
    fn recording_counts_opens_and_keeps_the_latest_time() {
        let mut history = temp_history();
        let path = Path::new("/docs/report.pdf");
        history.record(path, ts(10));
        history.record(path, ts(20));
        assert_eq!(
            history.get(path),
            Some(OpenRecord {
                last_opened: ts(20),
                count: 2
            })
        );
        assert_eq!(history.len, 1);
    }

    #[test]
    fn records_survive_a_reload_and_can_be_removed() {
        let db = sled::Config::new().temporary(true).open().expect("temp db");
        let path = Path::new("/docs/a.txt");
        {
            let mut history = OpenHistory::load(db.open_tree("open_history").ok());
            history.record(path, ts(5));
        }
        let mut history = OpenHistory::load(db.open_tree("open_history").ok());
        assert_eq!(history.get(path).map(|record| record.count), Some(1));
        history.remove(path);
        assert_eq!(history.get(path), None);
        assert!(history.by_dir.is_empty());
        let history = OpenHistory::load(db.open_tree("open_history").ok());
        assert_eq!(history.len, 0);
    }

    #[test]
    fn the_cap_drops_the_least_recently_opened() {
        let mut history = temp_history();
        for index in 0..=MAX_RECORDS as u64 {
            history.record(&PathBuf::from(format!("/many/{index}.txt")), ts(index + 1));
        }
        assert!(history.len < MAX_RECORDS);
        assert_eq!(history.get(Path::new("/many/0.txt")), None);
        let newest = PathBuf::from(format!("/many/{MAX_RECORDS}.txt"));
        assert!(history.get(&newest).is_some());
    }

    #[test]
    fn date_opened_is_the_later_of_record_and_access_time() {
        let record = OpenRecord {
            last_opened: ts(50),
            count: 1,
        };
        assert_eq!(last_opened(Some(record), ts(40)), ts(50));
        assert_eq!(last_opened(Some(record), ts(60)), ts(60));
        assert_eq!(last_opened(None, ts(40)), ts(40));
    }

    #[test]
    fn renaming_a_folder_moves_the_records_below_it() {
        record(Path::new("/rename_src/sub/x.txt"));
        rename(Path::new("/rename_src"), Path::new("/rename_dst"));
        assert!(get(Path::new("/rename_src/sub/x.txt")).is_none());
        assert_eq!(
            get(Path::new("/rename_dst/sub/x.txt")).map(|record| record.count),
            Some(1)
        );
        assert!(forget([Path::new("/rename_dst/sub/x.txt")]));
    }
}
