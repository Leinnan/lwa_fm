//! Reading folders into the core's listings: what the core leaves out
//! because it touches the file system or the clock.
use std::fs::FileType;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use dirfleet_core::listing::{
    DirEntry, DirEntryMetaData, ElapsedTime, EntryFlags, EntryType, Listing, ListingBuilder,
    TimestampSeconds,
};

use crate::fs_enum::{EntryName, enumerate, enumerate_std, is_package};
use crate::paths::{PathHelper as _, canonical_string};

/// How long ago a moment was, by the clock now.
pub trait Elapsed {
    fn elapsed(self) -> ElapsedTime;
}

impl Elapsed for TimestampSeconds {
    #[inline]
    fn elapsed(self) -> ElapsedTime {
        self.elapsed_at(SystemTime::now())
    }
}

/// Appends entries straight from a directory read.
pub trait PushRaw {
    /// Decodes `name` into the arena without an intermediate `String`.
    fn push_raw(&mut self, dir_index: u32, name: &EntryName<'_>, meta: DirEntryMetaData);
}

impl PushRaw for ListingBuilder {
    #[inline]
    fn push_raw(&mut self, dir_index: u32, name: &EntryName<'_>, meta: DirEntryMetaData) {
        self.push_with(dir_index, meta, |names| name.append_utf8(names));
    }
}

/// The type of an entry from its file type; links to folders count as folders on Windows.
#[inline]
#[must_use]
pub fn entry_type_of(value: FileType) -> EntryType {
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

/// The type of a directory at `path`, telling packages apart.
#[must_use]
#[cfg_attr(
    not(target_os = "macos"),
    allow(clippy::missing_const_for_fn, reason = "packages exist only on macOS")
)]
pub fn entry_type_of_dir(path: &Path) -> EntryType {
    if is_package(path) {
        EntryType::Package
    } else {
        EntryType::Directory
    }
}

/// The listing metadata of an entry read with `std::fs`.
#[must_use]
pub fn metadata_of(meta: &std::fs::Metadata) -> DirEntryMetaData {
    let entry_type = entry_type_of(meta.file_type());
    let created_at = meta
        .created()
        .map(TimestampSeconds::from)
        .unwrap_or_default();
    let accessed_at = meta
        .accessed()
        .map(TimestampSeconds::from)
        .unwrap_or_default();
    let modified = meta.modified().ok();
    let modified_at = modified.map(TimestampSeconds::from).unwrap_or_default();
    let source_revision = modified
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos());
    #[cfg(windows)]
    let size = std::os::windows::fs::MetadataExt::file_size(meta);
    #[cfg(target_os = "linux")]
    let size = std::os::linux::fs::MetadataExt::st_size(meta);
    #[cfg(target_os = "macos")]
    let size = std::os::macos::fs::MetadataExt::st_size(meta);
    #[cfg(windows)]
    let attributes = std::os::windows::fs::MetadataExt::file_attributes(meta);
    #[cfg(target_os = "macos")]
    let attributes = crate::fs_enum::macos_hidden_attr(meta);
    #[cfg(not(any(windows, target_os = "macos")))]
    let attributes = 0;
    DirEntryMetaData {
        entry_type,
        created_at,
        modified_at,
        accessed_at,
        source_revision,
        since_modified: modified_at.elapsed(),
        size,
        flags: EntryFlags::from_attributes(attributes, meta.file_type().is_symlink()),
        file_id: 0,
    }
}

/// An entry of a `std::fs` directory read, typed but without its metadata.
#[must_use]
pub fn dir_entry_of(value: &std::fs::DirEntry) -> Option<DirEntry> {
    let file_type = value.file_type().ok()?;
    let mut entry_type = entry_type_of(file_type);
    if entry_type == EntryType::Directory {
        entry_type = entry_type_of_dir(&value.path());
    }
    Some(DirEntry {
        meta: entry_type.into(),
        dir: entry_dir(&value.path()),
        file_name: value.file_name().to_string_lossy().into_owned(),
    })
}

/// Directory part of an entry path, normalised lexically. Callers walk roots
/// that were already normalised once, so no per-entry `canonicalize` is needed.
fn entry_dir(path: &Path) -> Arc<str> {
    path.parent().map_or_else(
        || Arc::from(""),
        |parent| Arc::from(canonical_string(parent)),
    )
}

/// Reads one directory, or `None` when it can't be read.
///
/// Uses the bulk enumerator, falling back to `std::fs::read_dir` on volumes
/// that reject it. Collation keys are built eagerly here (on the calling
/// background thread, in parallel for large directories) so they are
/// persisted with the cache entry.
#[must_use]
pub fn read_listing(dir: &Path) -> Option<Listing> {
    let dir_name: Arc<str> = Arc::from(dir.to_full_path_string());
    let mut builder = ListingBuilder::for_dir(&dir_name, 256, false);
    let mut read = enumerate(dir, &mut |entry| {
        builder.push_raw(0, &entry.name, entry.metadata());
    });
    // Elsewhere `enumerate` already is `enumerate_std`.
    if cfg!(windows) && read.is_err() {
        builder = ListingBuilder::for_dir(&dir_name, 256, false);
        read = enumerate_std(dir, &mut |entry| {
            builder.push_raw(0, &entry.name, entry.metadata());
        });
    }
    read.ok()?;
    let listing = builder.finish();
    let _ = listing.sort_keys();
    Some(listing)
}

/// Sets the collator to the device's language. Shells call this once at start.
pub fn init_locale() {
    let lang = bevy_device_lang::get_lang().unwrap_or_else(|| "en".to_owned());
    dirfleet_core::listing::set_collator_locale(&lang);
}

/// The names of `path`'s subfolders (packages left out), sorted ignoring
/// case. Uses only the directory listing's file types.
///
/// # Errors
/// When the folder can't be read.
pub fn child_directories(path: &Path, show_hidden: bool) -> std::io::Result<Vec<String>> {
    type Reader = fn(&Path, &mut dyn FnMut(&crate::fs_enum::RawEntry<'_>)) -> std::io::Result<()>;
    let collect = |read: Reader| {
        let mut names: Vec<String> = Vec::new();
        read(path, &mut |entry| {
            if !entry.is_dir() || entry.is_package() {
                return;
            }
            let name = entry.name.to_string_lossy().into_owned();
            if show_hidden || !dirfleet_core::listing::is_hidden_entry(&name, &entry.metadata()) {
                names.push(name);
            }
        })
        .map(|()| names)
    };
    let mut names =
        collect(crate::fs_enum::enumerate).or_else(|_| collect(crate::fs_enum::enumerate_std))?;
    names.sort_by_cached_key(|name| name.to_lowercase());
    Ok(names)
}
