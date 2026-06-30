use crate::{
    app::dir_handling::COLLATER,
    data::time::{ElapsedTime, TimestampSeconds},
    helper::PathHelper,
};
use bincode::{Decode, Encode};
use rayon::iter::{
    IntoParallelIterator, IntoParallelRefIterator, ParallelExtend, ParallelIterator,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::{cmp::Ordering, fs::FileType, path::{Path, PathBuf}};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize, Decode, Encode, Hash,
)]
pub enum EntryType {
    File,
    Directory,
}

impl From<FileType> for EntryType {
    #[inline]
    fn from(value: FileType) -> Self {
        #[cfg(target_os = "windows")]
        let is_dir = {
            use std::os::windows::fs::FileTypeExt;
            value.is_dir() || value.is_symlink_dir()
        };
        #[cfg(not(target_os = "windows"))]
        let is_dir = value.is_dir();
        if is_dir { Self::Directory } else { Self::File }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Decode, Encode)]
pub struct SortKey {
    pub is_file: bool,
    pub sort_key: Vec<u8>,
}

impl SortKey {
    #[inline]
    #[must_use]
    pub fn compare(&self, other: &Self) -> Ordering {
        self.is_file
            .cmp(&other.is_file)
            .then(self.sort_key.cmp(&other.sort_key))
    }
    #[inline]
    #[must_use]
    pub fn new(entry: &DirEntry) -> Self {
        let base_path = entry.get_splitted_path().1;
        Self::new_path(base_path, entry.is_file())
    }
    #[inline]
    pub fn new_path(base_path: &str, is_file: bool) -> Self {
        let mut sort_key = Vec::with_capacity(30);
        _ = COLLATER.write_sort_key_to(base_path, &mut sort_key);
        Self { is_file, sort_key }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Decode, Encode)]
pub struct DirContent {
    pub path: String,
    pub entries: Vec<DirEntryData>,
}

impl DirContent {
    #[must_use]
    pub fn read(dir: &Path) -> Option<Self> {
        let paths = {
            #[cfg(feature = "profiling")]
            puffin::profile_scope!("lwa_fm::dir_handling::db_read::with_hidden::dir");
            std::fs::read_dir(dir)
        };
        let Ok(paths) = paths else {
            return None;
        };
        let dir_entries: Vec<std::fs::DirEntry> = paths.filter_map(Result::ok).collect();
        let entries: Vec<DirEntryData> = if dir_entries.len() > 100 {
            dir_entries
                .into_par_iter()
                .filter_map(|e| e.try_into().ok())
                .collect()
        } else {
            dir_entries
                .into_iter()
                .filter_map(|e| e.try_into().ok())
                .collect()
        };
        Some(Self {
            path: dir.to_full_path_string(),
            entries,
        })
    }

    #[inline]
    pub fn populate(&self, entries: &mut Vec<DirEntry>) {
        let dir: Arc<str> = Arc::from(self.path.as_str());
        entries.par_extend(self.entries.par_iter().map(|e| {
            DirEntry {
                meta: e.meta,
                sort_key: e.sort_key.clone(),
                dir: Arc::clone(&dir),
                file_name: e.file_name.clone(),
            }
        }));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Decode, Encode)]
pub struct DirEntryData {
    pub meta: DirEntryMetaData,
    pub file_name: String,
    pub sort_key: SortKey,
}

impl DirEntryData {
    #[inline]
    pub fn read_metadata(&mut self, dir_path: &Path) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::conversion::read_metadata");
        let Ok(metadata) = std::fs::metadata(dir_path.join(&self.file_name)) else {
            return;
        };
        self.meta = metadata.into();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Decode, Encode)]
pub struct DirEntry {
    pub meta: DirEntryMetaData,
    pub sort_key: SortKey,
    pub dir: Arc<str>,
    pub file_name: String,
}

impl DirEntry {
    #[inline]
    pub fn read_metadata(&mut self) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::conversion::read_metadata");
        let path = self.get_path();
        let Ok(metadata) = std::fs::metadata(&path) else {
            return;
        };
        self.meta = metadata.into();
    }

    #[must_use]
    pub fn full_path_string(&self) -> String {
        let mut s = String::with_capacity(self.dir.len() + 1 + self.file_name.len());
        s.push_str(&self.dir);
        s.push(std::path::MAIN_SEPARATOR);
        s.push_str(&self.file_name);
        s
    }

    #[must_use]
    pub fn get_path(&self) -> PathBuf {
        PathBuf::from(self.full_path_string())
    }

    #[inline]
    #[must_use]
    pub fn is_file(&self) -> bool {
        matches!(self.meta.entry_type, EntryType::File)
    }

    #[inline]
    #[must_use]
    pub fn get_splitted_path(&self) -> (&str, &str) {
        (&self.dir, &self.file_name)
    }

    #[must_use]
    pub fn to_full_path_string(&self) -> String {
        self.get_path().to_full_path_string()
    }

    #[cfg(test)]
    pub fn test_new(path: &str) -> Self {
        let sep = path.rfind(std::path::MAIN_SEPARATOR)
            .or_else(|| path.rfind('/'));
        let (dir, file_name) = sep
            .map_or(("", path), |i| (&path[..i], &path[i + 1..]));
        // Normalize forward slashes to platform separator (preserving leading /)
        let dir = if dir.is_empty() {
            Arc::from("")
        } else if dir.starts_with('/') {
            let sep_str = std::path::MAIN_SEPARATOR.to_string();
            let normalized = format!(
                "{}{}",
                std::path::MAIN_SEPARATOR,
                dir[1..].replace('/', &sep_str)
            );
            Arc::from(normalized.as_str())
        } else {
            Arc::from(dir.replace('/', &std::path::MAIN_SEPARATOR.to_string()).as_str())
        };
        Self {
            meta: DirEntryMetaData {
                entry_type: EntryType::File,
                created_at: Default::default(),
                modified_at: Default::default(),
                since_modified: Default::default(),
                size: 0,
            },
            dir,
            file_name: file_name.to_string(),
            sort_key: SortKey::new_path(file_name, true),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, Decode, Encode, PartialEq, Eq, Hash)]
pub struct DirEntryMetaData {
    pub entry_type: EntryType,
    pub created_at: TimestampSeconds,
    pub modified_at: TimestampSeconds,
    pub since_modified: ElapsedTime,
    pub size: u64,
}

impl From<EntryType> for DirEntryMetaData {
    #[inline]
    fn from(entry_type: EntryType) -> Self {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::conversion::from_entry_type");
        let created_at = TimestampSeconds::default();
        let modified_at = created_at;
        let since_modified = ElapsedTime::default();
        let size = 0;
        Self {
            entry_type,
            created_at,
            modified_at,
            since_modified,
            size,
        }
    }
}

impl From<std::fs::Metadata> for DirEntryMetaData {
    #[inline]
    fn from(meta: std::fs::Metadata) -> Self {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::conversion::metadata");
        let file_type = meta.file_type();
        let entry_type: EntryType = file_type.into();
        let created_at = meta
            .created()
            .map(TimestampSeconds::from)
            .unwrap_or_default();
        let modified_at = meta
            .modified()
            .map(TimestampSeconds::from)
            .unwrap_or_default();
        #[cfg(windows)]
        let size = std::os::windows::fs::MetadataExt::file_size(&meta);
        #[cfg(target_os = "linux")]
        let size = std::os::linux::fs::MetadataExt::st_size(&meta);
        #[cfg(target_os = "macos")]
        let size = std::os::macos::fs::MetadataExt::st_size(&meta);
        let since_modified = modified_at.elapsed();
        Self {
            entry_type,
            created_at,
            modified_at,
            since_modified,
            size,
        }
    }
}
impl TryFrom<std::fs::DirEntry> for DirEntryData {
    type Error = ();

    #[inline]
    fn try_from(value: std::fs::DirEntry) -> Result<Self, Self::Error> {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::conversion::from_std");
        let Ok(meta) = value.metadata() else {
            return Err(());
        };
        let meta: DirEntryMetaData = meta.into();
        let file_name: String = {
            #[cfg(feature = "profiling")]
            puffin::profile_scope!("lwa_fm::dir_handling::conversion::from_std::string");
            value.file_name().to_string_lossy().to_string()
        };
        let sort_key = SortKey::new_path(&file_name, meta.entry_type.eq(&EntryType::File));
        Ok(Self {
            meta,
            file_name,
            sort_key,
        })
    }
}

impl TryFrom<std::fs::DirEntry> for DirEntry {
    type Error = ();

    #[inline]
    fn try_from(value: std::fs::DirEntry) -> Result<Self, Self::Error> {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::conversion::from_std");
        let Ok(file_type) = value.file_type() else {
            return Err(());
        };

        let meta = {
            #[cfg(feature = "profiling")]
            puffin::profile_scope!("lwa_fm::dir_handling::conversion::from_std::file_type_meta");
            let entry_type: EntryType = file_type.into();
            let meta: DirEntryMetaData = entry_type.into();
            meta
        };

        let path = value.path().to_full_path_string();
        let file_name = value.file_name().to_string_lossy().into_owned();
        let dir_len = path.len().saturating_sub(file_name.len() + 1);
        let dir = Arc::from(&path[..dir_len]);
        let sort_key = SortKey::new_path(&file_name, file_type.is_file());
        Ok(Self {
            meta,
            dir,
            file_name,
            sort_key,
        })
    }
}

/// Lazy-loading directory listing backed by [`DirEntryData`].
///
/// Stores raw entry data in a shared [`Arc`] slice, keeping sort/filter state as
/// index permutations. Full [`DirEntry`] values are materialised only on request,
/// which avoids allocating full-path strings and sort-key vectors for entries
/// that are never displayed.
#[derive(Debug, Clone)]
pub struct DirList {
    pub dir: Arc<str>,
    pub entries: Arc<[DirEntryData]>,
    pub sorted: Vec<usize>,
    pub visible: Vec<usize>,
}

impl DirList {
    #[must_use]
    pub fn new(dir: Arc<str>, entries: Vec<DirEntryData>) -> Self {
        let entries: Arc<[DirEntryData]> = Arc::from(entries);
        let sorted: Vec<usize> = (0..entries.len()).collect();
        let visible = sorted.clone();
        Self {
            dir,
            entries,
            sorted,
            visible,
        }
    }

    #[inline]
    pub fn from_content(content: &DirContent) -> Self {
        Self::new(Arc::from(content.path.as_str()), content.entries.clone())
    }

    /// Materialise a single [`DirEntry`] for the entry at `sorted_index`.
    #[inline]
    #[must_use]
    pub fn materialize(&self, sorted_index: usize) -> DirEntry {
        let data = &self.entries[sorted_index];
        DirEntry {
            meta: data.meta,
            sort_key: data.sort_key.clone(),
            dir: Arc::clone(&self.dir),
            file_name: data.file_name.clone(),
        }
    }

    /// Number of entries passing the current filter.
    #[inline]
    #[must_use]
    pub fn visible_count(&self) -> usize {
        self.visible.len()
    }

    /// Total number of entries in the directory.
    #[inline]
    #[must_use]
    pub fn total_count(&self) -> usize {
        self.entries.len()
    }

    /// Populate `out` with materialised entries for the current visible range.
    pub fn populate_visible(&self, out: &mut Vec<DirEntry>) {
        out.reserve(self.visible.len());
        for &vi in &self.visible {
            let si = self.sorted[vi];
            out.push(self.materialize(si));
        }
    }

    /// Build a `DirList` by **moving** entries out of `list` (no clone), making
    /// this store canonical so the caller can drop its `Vec<DirEntry>`. All
    /// entries MUST share the same `dir` prefix; the per-entry `dir: Arc<str>`
    /// is replaced by a single shared `Arc<str>`. Returns `None` for an empty
    /// list. Index order is preserved, so indices computed against `list`
    /// remain valid against [`Self::entries`] / [`Self::materialize`].
    pub fn from_owned_list(list: Vec<DirEntry>) -> Option<Self> {
        let dir = list.first()?.dir.clone();
        let entries: Vec<DirEntryData> = list
            .into_iter()
            .map(|e| DirEntryData {
                meta: e.meta,
                file_name: e.file_name,
                sort_key: e.sort_key,
            })
            .collect();
        Some(Self::new(dir, entries))
    }

    /// Build a `DirList` from an already-sorted `Vec<DirEntry>`.
    /// All entries MUST share the same `dir` prefix.
    pub fn from_sorted_list(list: &[DirEntry]) -> Option<Self> {
        let dir = list.first()?.dir.clone();
        let entries: Vec<DirEntryData> = list
            .iter()
            .map(|e| DirEntryData {
                meta: e.meta,
                file_name: e.file_name.clone(),
                sort_key: e.sort_key.clone(),
            })
            .collect();
        Some(Self::new(dir, entries))
    }

    /// Build a sorted `DirList` from a raw `DirContent`.
    pub fn from_sorted_content(content: &DirContent) -> Self {
        let mut dl = Self::from_content(content);
        dl.sorted = (0..dl.entries.len()).collect();
        dl.visible = dl.sorted.clone();
        dl
    }
}

impl TryFrom<walkdir::DirEntry> for DirEntry {
    type Error = ();

    #[inline]
    fn try_from(value: walkdir::DirEntry) -> Result<Self, Self::Error> {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::conversion::from_walkdir");
        let meta = value.metadata().map_err(|_| ())?;
        let meta: DirEntryMetaData = meta.into();
        let path = value.path().to_full_path_string();
        let file_name = value.file_name().to_string_lossy().into_owned();
        let dir_len = path.len().saturating_sub(file_name.len() + 1);
        let dir = Arc::from(&path[..dir_len]);
        let sort_key = SortKey::new_path(&file_name, meta.entry_type.eq(&EntryType::File));
        Ok(Self {
            meta,
            dir,
            file_name,
            sort_key,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_owned_list_preserves_order_and_materialises() {
        let entries: Vec<DirEntry> = ["/a/one.txt", "/a/two.dat", "/a/three.log"]
            .into_iter()
            .map(DirEntry::test_new)
            .collect();
        let names: Vec<String> = entries.iter().map(|e| e.file_name.clone()).collect();
        let expected_dir: Arc<str> = entries[0].dir.clone();

        // `from_owned_list` consumes `entries` (no clone of the source).
        let list = DirList::from_owned_list(entries).expect("non-empty list");
        assert_eq!(list.total_count(), 3);
        assert_eq!(list.dir, expected_dir);

        // Order is preserved, so indices computed against the source `Vec` remain
        // valid: `materialize(i)` must reconstruct exactly the i-th source entry.
        for (i, expected) in names.iter().enumerate() {
            let materialised = list.materialize(i);
            assert_eq!(materialised.file_name, *expected, "name mismatch at {i}");
            assert!(materialised.is_file(), "expected file at {i}");
            assert_eq!(materialised.dir, expected_dir, "shared dir prefix lost at {i}");
            assert_eq!(materialised.get_splitted_path().1, expected.as_str());
        }
    }

    #[test]
    fn from_owned_list_empty_returns_none() {
        assert!(DirList::from_owned_list(Vec::new()).is_none());
    }
}
