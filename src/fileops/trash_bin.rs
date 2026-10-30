//! Reading and managing the Recycle Bin / Trash for its own view.
//!
//! Windows and Linux go through `trash::os_limited`. On macOS the crate cannot
//! list the Trash, so `~/.Trash` is read as a folder (this needs Full Disk
//! Access), and putting items back is not possible.

use crate::data::files::{DirEntryMetaData, EntryRef, EntryType, Listing, ListingBuilder};
use crate::data::time::TimestampSeconds;
use std::path::PathBuf;

/// What the platform calls it.
pub const LABEL: &str = if cfg!(windows) {
    "Recycle Bin"
} else {
    "Trash"
};

/// One trashed item, as shown in the view: where it was deleted from, and when.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrashKey {
    /// The original path on Windows and Linux; the path inside `~/.Trash` on macOS.
    pub path: PathBuf,
    pub deleted_at: u32,
}

impl TrashKey {
    /// The key for a row of the Recycle Bin listing.
    pub fn of(entry: &EntryRef<'_>) -> Self {
        Self {
            path: entry.get_path(),
            deleted_at: *entry.meta.modified_at,
        }
    }
}

/// Whether items can be put back where they came from.
pub const fn supports_restore() -> bool {
    cfg!(not(target_os = "macos"))
}

fn meta(entry_type: EntryType, deleted_at: TimestampSeconds, size: u64) -> DirEntryMetaData {
    DirEntryMetaData {
        modified_at: deleted_at,
        created_at: deleted_at,
        since_modified: deleted_at.elapsed(),
        size,
        ..DirEntryMetaData::from(entry_type)
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::{EntryType, Listing, ListingBuilder, PathBuf, TimestampSeconds, TrashKey};
    use std::{ffi::OsString, path::Path, sync::Arc};
    use trash::{TrashItem, TrashItemSize, os_limited};

    /// The item's real file name. Windows reports the display name, which
    /// leaves out the extension when Explorer hides known extensions; the
    /// item's own name in the bin (`$R1A2B3C.txt`) still carries it.
    fn file_name(item: &TrashItem) -> OsString {
        let mut name = item.name.clone();
        if cfg!(windows) {
            let real = Path::new(&item.id).extension();
            if real.is_some() && Path::new(&item.name).extension() != real {
                name.push(".");
                name.push(real.unwrap_or_default());
            }
        }
        name
    }

    fn deleted_at(item: &TrashItem) -> u32 {
        u32::try_from(item.time_deleted.max(0)).unwrap_or(u32::MAX)
    }

    fn matches(item: &TrashItem, key: &TrashKey) -> bool {
        deleted_at(item) == key.deleted_at
            && key.path.parent() == Some(item.original_parent.as_path())
            && key.path.file_name() == Some(file_name(item).as_os_str())
    }

    fn find(keys: &[TrashKey]) -> anyhow::Result<Vec<TrashItem>> {
        let items: Vec<TrashItem> = os_limited::list()?
            .into_iter()
            .filter(|item| keys.iter().any(|key| matches(item, key)))
            .collect();
        anyhow::ensure!(
            !items.is_empty(),
            "The items are no longer in the {}",
            super::LABEL
        );
        Ok(items)
    }

    pub fn listing() -> anyhow::Result<Listing> {
        let items = os_limited::list()?;
        let mut builder = ListingBuilder::with_capacity(items.len());
        for item in &items {
            let (entry_type, size) = match os_limited::metadata(item).map(|m| m.size) {
                Ok(TrashItemSize::Entries(_)) => (EntryType::Directory, 0),
                Ok(TrashItemSize::Bytes(size)) => (EntryType::File, size),
                Err(_) => (EntryType::File, 0),
            };
            let deleted = TimestampSeconds::from(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(u64::from(deleted_at(item))),
            );
            let dir: Arc<str> = item.original_parent.to_string_lossy().into();
            builder.push(
                &dir,
                &file_name(item).to_string_lossy(),
                super::meta(entry_type, deleted, size),
            );
        }
        Ok(builder.finish())
    }

    /// Puts the items back and returns where they reappear.
    pub fn restore(keys: &[TrashKey]) -> anyhow::Result<Vec<PathBuf>> {
        let items = find(keys)?;
        // Where each item lands, which is short of its extension when the
        // display name dropped it, and where it belongs.
        let moves: Vec<(PathBuf, PathBuf)> = items
            .iter()
            .map(|item| {
                (
                    item.original_path(),
                    item.original_parent.join(file_name(item)),
                )
            })
            .collect();
        os_limited::restore_all(items)?;
        for (landed, real) in &moves {
            if landed != real && landed.exists() && !real.exists() {
                std::fs::rename(landed, real)?;
            }
        }
        Ok(moves.into_iter().map(|(_, real)| real).collect())
    }

    pub fn purge(keys: &[TrashKey]) -> anyhow::Result<()> {
        os_limited::purge_all(find(keys)?)?;
        Ok(())
    }

    pub fn empty() -> anyhow::Result<()> {
        os_limited::purge_all(os_limited::list()?)?;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{EntryType, Listing, ListingBuilder, PathBuf, TimestampSeconds, TrashKey};
    use std::sync::Arc;

    fn trash_dir() -> anyhow::Result<PathBuf> {
        directories::UserDirs::new()
            .map(|dirs| dirs.home_dir().join(".Trash"))
            .ok_or_else(|| anyhow::anyhow!("Could not find the home folder"))
    }

    pub fn listing() -> anyhow::Result<Listing> {
        let dir = trash_dir()?;
        let read = std::fs::read_dir(&dir).map_err(|error| {
            anyhow::anyhow!(
                "Could not read the Trash ({error}). Allow Full Disk Access for DirFleet in System Settings > Privacy & Security."
            )
        })?;
        let dir_name: Arc<str> = dir.to_string_lossy().into();
        let mut builder = ListingBuilder::with_capacity(16);
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".DS_Store" {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let mut entry_type = metadata.file_type().into();
            if entry_type == EntryType::Directory {
                entry_type = EntryType::of_dir(&entry.path());
            }
            let deleted = metadata
                .modified()
                .map(TimestampSeconds::from)
                .unwrap_or_default();
            builder.push(
                &dir_name,
                &name,
                super::meta(entry_type, deleted, metadata.len()),
            );
        }
        Ok(builder.finish())
    }

    pub fn restore(_keys: &[TrashKey]) -> anyhow::Result<Vec<PathBuf>> {
        anyhow::bail!("Restoring from the Trash isn't supported on macOS")
    }

    pub fn purge(keys: &[TrashKey]) -> anyhow::Result<()> {
        let dir = trash_dir()?;
        for key in keys {
            // Only ever remove what is inside the Trash.
            anyhow::ensure!(key.path.parent() == Some(dir.as_path()), "Not in the Trash");
            super::super::worker::remove_any(&key.path)?;
        }
        Ok(())
    }

    pub fn empty() -> anyhow::Result<()> {
        let status = std::process::Command::new("osascript")
            .args(["-e", "tell application \"Finder\" to empty trash"])
            .status()?;
        anyhow::ensure!(status.success(), "Finder could not empty the Trash");
        Ok(())
    }
}

pub use platform::{empty, listing, purge, restore};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Arc;

    #[test]
    fn key_reads_path_and_deletion_time() {
        let dir: Arc<str> = Arc::from(if cfg!(windows) { "C:\\data" } else { "/data" });
        let mut builder = ListingBuilder::with_capacity(1);
        let deleted = TimestampSeconds::from(
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000),
        );
        builder.push(&dir, "clip.mp4", meta(EntryType::File, deleted, 42));
        let listing = builder.finish();
        let key = TrashKey::of(&listing.entry(0));
        assert_eq!(key.path, Path::new(dir.as_ref()).join("clip.mp4"));
        assert_eq!(key.deleted_at, 1_700_000_000);
        assert_eq!(listing.entry(0).meta.size, 42);
    }
}
