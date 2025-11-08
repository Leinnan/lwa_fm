use egui::TextBuffer;

use crate::data::time::{ElapsedTime, TimestampSeconds};
use std::{borrow::Cow, fs::FileType, path::Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub meta: DirEntryMetaData,
    pub path: Cow<'static, str>,
    file_name_index: usize,
    // pub sort_key: SmallVec<[u8; 40]>,
}

impl DirEntry {
    #[inline]
    pub fn read_metadata(&mut self) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("dir_handling::conversion::read_metadata");
        let Ok(metadata) = std::fs::metadata(self.path.as_str()) else {
            return;
        };
        self.meta = metadata.into();
    }
}

#[derive(Debug, Clone, Copy)]
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
        puffin::profile_scope!("dir_handling::conversion::from_entry_type");
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
        puffin::profile_scope!("dir_handling::conversion::metadata");
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
            size,
        }
    }
}

impl TryFrom<std::fs::DirEntry> for DirEntry {
    type Error = ();

    #[inline]
    fn try_from(value: std::fs::DirEntry) -> Result<Self, Self::Error> {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("dir_handling::conversion::from_std");
        let Ok(file_type) = value.file_type() else {
            return Err(());
        };
        let meta = {
            #[cfg(feature = "profiling")]
            puffin::profile_scope!("dir_handling::conversion::from_std::file_type_meta");
            let entry_type: EntryType = file_type.into();
            let meta: DirEntryMetaData = entry_type.into();
            meta
        };

        // let mut sort_key = SmallVec::<[u8; 40]>::new();
        // let _ =
        //     COLLATER.write_sort_key_utf8_to(value.file_name().as_encoded_bytes(), &mut sort_key);
        let path: Cow<'static, str> = {
            #[cfg(feature = "profiling")]
            puffin::profile_scope!("dir_handling::conversion::from_std::string");
            Cow::Owned(value.path().to_string_lossy().into_owned())
        };
        let file_name_index = path.len() - value.file_name().len();

        Ok(Self {
            path,
            meta,
            file_name_index,
            // sort_key,
        })
    }
}

impl TryFrom<walkdir::DirEntry> for DirEntry {
    type Error = ();

    #[inline]
    fn try_from(value: walkdir::DirEntry) -> Result<Self, Self::Error> {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("dir_handling::conversion::from_walkdir");
        let Ok(meta) = value.metadata() else {
            return Err(());
        };
        let meta: DirEntryMetaData = meta.into();
        let path: Cow<'static, str> = Cow::Owned(value.path().to_string_lossy().to_string());
        let file_name_index = path.len() - value.file_name().len();
        // let mut sort_key = SmallVec::<[u8; 40]>::new();
        // let _ =
        //     COLLATER.write_sort_key_utf8_to(value.file_name().as_encoded_bytes(), &mut sort_key);

        Ok(Self {
            path,
            meta,
            file_name_index,
            // sort_key,
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
