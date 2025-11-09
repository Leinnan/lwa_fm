use crate::{
    app::dir_handling::COLLATER,
    data::time::{ElapsedTime, TimestampSeconds},
};
use bincode::{Decode, Encode};
use rayon::iter::{IntoParallelRefIterator, ParallelBridge, ParallelExtend, ParallelIterator};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    fs::FileType,
    path::{Path, PathBuf},
};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize, Decode, Encode,
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
        if is_dir {
            EntryType::Directory
        } else {
            EntryType::File
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, Decode, Encode)]
pub struct SortKey {
    pub is_file: bool,
    pub sort_key: [u8; 30],
}

fn empty() -> [u8; 30] {
    [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ]
}

impl SortKey {
    #[inline]
    pub fn compare(&self, other: &Self) -> Ordering {
        self.is_file
            .cmp(&other.is_file)
            .then(self.sort_key.cmp(&other.sort_key))
    }
    #[inline]
    pub fn new(entry: &DirEntry) -> Self {
        let base_path = entry.get_splitted_path().1;
        Self::new_path(base_path, entry.is_file())
    }
    #[inline]
    pub fn new_path(base_path: &str, is_file: bool) -> Self {
        let path = if base_path.len() > 15 {
            base_path.split_at(15).0
        } else {
            base_path
        };
        let mut sort_key = Vec::with_capacity(30);
        _ = COLLATER.write_sort_key_to(path, &mut sort_key);
        let sort_key = sort_key.try_into().unwrap_or(empty());
        Self { is_file, sort_key }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Decode, Encode)]
pub struct DirContent {
    pub path: PathBuf,
    pub entries: Vec<DirEntryData>,
}

impl DirContent {
    pub fn read(dir: &Path) -> Option<Self> {
        let paths = {
            #[cfg(feature = "profiling")]
            puffin::profile_scope!("lwa_fm::dir_handling::db_read::with_hidden::dir");
            std::fs::read_dir(dir)
        };
        let Ok(paths) = paths else {
            return None;
        };
        let entries: Vec<DirEntryData> = paths
            .par_bridge()
            .filter_map(|e| {
                puffin::profile_scope!("lwa_fm::dir_handling::db_read::db_mapping::entry");
                let e = e.ok()?;
                e.try_into().ok()
            })
            .collect();
        Some(Self {
            path: dir.into(),
            entries,
        })
    }

    #[inline]
    pub fn populate(&self, entries: &mut Vec<DirEntry>) {
        let file_name_index = self.path.as_os_str().len() + 1;
        entries.par_extend(self.entries.par_iter().map(|e| DirEntry {
            meta: e.meta,
            path: format!("{}/{}", self.path.display(), &e.file_name),
            file_name_index,
            sort_key: e.sort_key,
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
    pub path: String,
    file_name_index: usize,
    pub sort_key: SortKey, // pub sort_key: SmallVec<[u8; 40]>,
}

impl DirEntry {
    #[inline]
    pub fn read_metadata(&mut self) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::conversion::read_metadata");
        let Ok(metadata) = std::fs::metadata(self.path.as_str()) else {
            return;
        };
        self.meta = metadata.into();
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, Decode, Encode)]
pub struct DirEntryMetaData {
    pub entry_type: EntryType,
    pub created_at: TimestampSeconds,
    pub modified_at: TimestampSeconds,
    pub since_modified: ElapsedTime,
    pub size: u32,
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
        DirEntryMetaData {
            entry_type,
            created_at,
            modified_at,
            since_modified,
            size: size as u32,
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
            file_name,
            meta,
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

        // let mut sort_key = SmallVec::<[u8; 40]>::new();
        // let _ =
        //     COLLATER.write_sort_key_utf8_to(value.file_name().as_encoded_bytes(), &mut sort_key);
        let path: String = {
            #[cfg(feature = "profiling")]
            puffin::profile_scope!("lwa_fm::dir_handling::conversion::from_std::string");
            value.path().to_string_lossy().to_string()
        };
        let file_name_index = path.len() - value.file_name().len();
        let sort_key = SortKey::new_path(
            value.file_name().as_os_str().to_str().unwrap(),
            file_type.is_file(),
        );
        Ok(Self {
            path,
            meta,
            file_name_index,
            sort_key,
        })
    }
}

impl TryFrom<walkdir::DirEntry> for DirEntry {
    type Error = ();

    #[inline]
    fn try_from(value: walkdir::DirEntry) -> Result<Self, Self::Error> {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::conversion::from_walkdir");
        let Ok(meta) = value.metadata() else {
            return Err(());
        };
        let meta: DirEntryMetaData = meta.into();
        let path = value.path().to_string_lossy().to_string();
        let file_name_index = path.len() - value.file_name().len();
        // let mut sort_key = SmallVec::<[u8; 40]>::new();
        // let _ =
        //     COLLATER.write_sort_key_utf8_to(value.file_name().as_encoded_bytes(), &mut sort_key);
        let sort_key = SortKey::new_path(
            value.file_name().to_str().unwrap(),
            meta.entry_type.eq(&EntryType::File),
        );
        Ok(Self {
            path,
            meta,
            file_name_index,
            sort_key,
        })
    }
}
impl AsRef<Path> for DirEntry {
    #[inline]
    fn as_ref(&self) -> &Path {
        Path::new(self.path.as_str())
    }
}

impl DirEntry {
    pub fn get_path(&self) -> &Path {
        self.as_ref()
    }

    #[inline]
    pub const fn is_file(&self) -> bool {
        matches!(self.meta.entry_type, EntryType::File)
    }

    #[inline]
    pub fn get_splitted_path(&self) -> (&str, &str) {
        self.path.split_at(self.file_name_index)
    }
}
