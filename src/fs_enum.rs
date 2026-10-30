//! Bulk directory enumeration.
//!
//! On Windows this bypasses `std::fs::read_dir` (one `FindNextFileW` per
//! ~4 KiB of kernel buffer plus a 592-byte `WIN32_FIND_DATAW`, an `OsString`
//! and often a `PathBuf` per entry) and asks `NtQueryDirectoryFile` to fill a
//! reused 64 KiB buffer with `FILE_ID_EXTD_DIR_INFORMATION` records. Names are
//! handed out as UTF-16 slices into that buffer and appended straight into the
//! listing arena, so a directory read allocates nothing per entry. The records
//! also carry the attributes (hidden, system, reparse tag) that the standard
//! library discards.
//!
//! Elsewhere the same interface is served by `std::fs::read_dir`.

use std::{
    borrow::Cow,
    ffi::OsString,
    io,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::data::files::{DirEntryMetaData, EntryFlags, EntryType};

#[cfg(target_os = "macos")]
mod package;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
pub use package::is_package;

/// Whether the directory at `path` is a package (only macOS has them).
#[cfg(not(target_os = "macos"))]
#[must_use]
pub const fn is_package(_path: &Path) -> bool {
    false
}

/// `FILE_ATTRIBUTE_READONLY`.
pub const ATTR_READONLY: u32 = 0x1;
/// `FILE_ATTRIBUTE_HIDDEN`.
pub const ATTR_HIDDEN: u32 = 0x2;
/// `FILE_ATTRIBUTE_SYSTEM`.
pub const ATTR_SYSTEM: u32 = 0x4;
/// `FILE_ATTRIBUTE_DIRECTORY`.
pub const ATTR_DIRECTORY: u32 = 0x10;
/// `FILE_ATTRIBUTE_REPARSE_POINT`.
pub const ATTR_REPARSE_POINT: u32 = 0x400;
/// Not a Windows attribute: marks a directory that is a package (see
/// [`is_package`]). No `FILE_ATTRIBUTE_*` uses this bit.
pub const ATTR_PACKAGE: u32 = 0x8000_0000;
/// `IO_REPARSE_TAG_MOUNT_POINT` (junctions).
pub const REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
/// `IO_REPARSE_TAG_SYMLINK`.
pub const REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

/// An entry name as the platform delivered it: UTF-16 straight out of the
/// kernel buffer on Windows, already UTF-8 elsewhere.
#[derive(Debug, Clone)]
pub enum EntryName<'a> {
    Utf16(&'a [u16]),
    Str(Cow<'a, str>),
}

impl EntryName<'_> {
    /// Appends the name as UTF-8. Unpaired surrogates become U+FFFD.
    pub fn append_utf8(&self, out: &mut String) {
        match self {
            Self::Str(name) => out.push_str(name),
            Self::Utf16(units) => {
                if units.iter().all(|&unit| unit < 0x80) {
                    out.reserve(units.len());
                    out.extend(units.iter().map(|&unit| char::from(unit as u8)));
                } else {
                    out.extend(
                        char::decode_utf16(units.iter().copied())
                            .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER)),
                    );
                }
            }
        }
    }

    /// Whether the name starts with this ASCII byte (no decoding needed).
    #[must_use]
    pub fn starts_with_ascii(&self, byte: u8) -> bool {
        match self {
            Self::Str(name) => name.as_bytes().first() == Some(&byte),
            Self::Utf16(units) => units.first() == Some(&u16::from(byte)),
        }
    }

    #[must_use]
    pub fn to_os_string(&self) -> OsString {
        match self {
            Self::Str(name) => OsString::from(name.as_ref()),
            #[cfg(windows)]
            Self::Utf16(units) => std::os::windows::ffi::OsStringExt::from_wide(units),
            #[cfg(not(windows))]
            Self::Utf16(_) => OsString::from(self.to_string_lossy().into_owned()),
        }
    }

    #[must_use]
    pub fn to_string_lossy(&self) -> Cow<'_, str> {
        match self {
            Self::Str(name) => Cow::Borrowed(name),
            Self::Utf16(_) => {
                let mut name = String::new();
                self.append_utf8(&mut name);
                Cow::Owned(name)
            }
        }
    }
}

/// One directory record, valid for the duration of the sink callback.
#[derive(Debug, Clone)]
pub struct RawEntry<'a> {
    pub name: EntryName<'a>,
    /// `FILE_ATTRIBUTE_*` bits (synthesised from the file type on Unix).
    pub attributes: u32,
    /// Reparse tag when `attributes` has [`ATTR_REPARSE_POINT`], else 0.
    pub reparse_tag: u32,
    pub size: u64,
    pub created: Option<SystemTime>,
    pub modified: Option<SystemTime>,
    /// Volume-unique file identifier (NTFS/ReFS file reference, inode on
    /// Unix). 0 when the filesystem does not provide one.
    pub file_id: u64,
}

impl RawEntry<'_> {
    #[inline]
    #[must_use]
    pub const fn is_dir(&self) -> bool {
        self.attributes & ATTR_DIRECTORY != 0
    }

    /// A directory shown and opened as a single item (see [`is_package`]).
    #[inline]
    #[must_use]
    pub const fn is_package(&self) -> bool {
        self.attributes & ATTR_PACKAGE != 0
    }

    /// A symbolic link or junction (not other reparse points such as cloud
    /// placeholders, which are ordinary files and folders to the user).
    #[inline]
    #[must_use]
    pub const fn is_symlink(&self) -> bool {
        self.attributes & ATTR_REPARSE_POINT != 0
            && matches!(
                self.reparse_tag,
                REPARSE_TAG_SYMLINK | REPARSE_TAG_MOUNT_POINT
            )
    }

    /// Hidden by attribute (Explorer semantics: hidden or system).
    #[inline]
    #[must_use]
    pub const fn is_hidden_attr(&self) -> bool {
        self.attributes & (ATTR_HIDDEN | ATTR_SYSTEM) != 0
    }

    #[must_use]
    pub fn metadata(&self) -> DirEntryMetaData {
        let entry_type = if self.attributes & ATTR_PACKAGE != 0 {
            EntryType::Package
        } else if self.is_dir() {
            EntryType::Directory
        } else {
            EntryType::File
        };
        let modified_at = self.modified.map(Into::into).unwrap_or_default();
        let source_revision = self
            .modified
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_nanos());
        DirEntryMetaData {
            entry_type,
            created_at: self.created.map(Into::into).unwrap_or_default(),
            modified_at,
            source_revision,
            since_modified: modified_at.elapsed(),
            size: self.size,
            flags: EntryFlags::from_attributes(self.attributes, self.is_symlink()),
            file_id: self.file_id,
        }
    }
}

/// [`ATTR_HIDDEN`] when the `UF_HIDDEN` flag is set: what Finder hides
/// besides dot names (`~/Library`, `/usr`, `/bin`, ...).
#[cfg(target_os = "macos")]
#[must_use]
pub fn macos_hidden_attr(meta: &std::fs::Metadata) -> u32 {
    /// `UF_HIDDEN` from `<sys/stat.h>`.
    const UF_HIDDEN: u32 = 0x8000;
    if std::os::macos::fs::MetadataExt::st_flags(meta) & UF_HIDDEN == 0 {
        0
    } else {
        ATTR_HIDDEN
    }
}

/// Lists the entries of `dir` (without `.` and `..`) into `sink`.
///
/// Fails only when the directory cannot be opened or read at all; entries
/// that cannot be described are skipped. On a failure callers should fall
/// back to [`enumerate_std`], which may succeed on exotic volumes.
pub fn enumerate(dir: &Path, sink: &mut dyn FnMut(&RawEntry<'_>)) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows::enumerate(dir, sink)
    }
    #[cfg(not(windows))]
    {
        enumerate_std(dir, sink)
    }
}

/// Portable implementation on top of `std::fs::read_dir`.
pub fn enumerate_std(dir: &Path, sink: &mut dyn FnMut(&RawEntry<'_>)) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)?.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let file_type = meta.file_type();
        let mut attributes = 0;
        let mut reparse_tag = 0;
        let is_dir = if file_type.is_symlink() {
            attributes |= ATTR_REPARSE_POINT;
            reparse_tag = REPARSE_TAG_SYMLINK;
            // Follow the link once so a linked directory lists as a directory.
            std::fs::metadata(entry.path()).is_ok_and(|target| target.is_dir())
        } else {
            file_type.is_dir()
        };
        if is_dir {
            attributes |= ATTR_DIRECTORY;
            if is_package(&entry.path()) {
                attributes |= ATTR_PACKAGE;
            }
        }
        #[cfg(windows)]
        {
            attributes |= std::os::windows::fs::MetadataExt::file_attributes(&meta);
        }
        #[cfg(target_os = "macos")]
        {
            attributes |= macos_hidden_attr(&meta);
        }
        let name = entry.file_name();
        let raw = RawEntry {
            name: EntryName::Str(name.to_string_lossy()),
            attributes,
            reparse_tag,
            size: meta.len(),
            created: meta.created().ok(),
            modified: meta.modified().ok(),
            file_id: file_id_of(&meta),
        };
        sink(&raw);
    }
    Ok(())
}

#[cfg(unix)]
fn file_id_of(meta: &std::fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::ino(meta)
}

#[cfg(not(unix))]
#[allow(clippy::missing_const_for_fn)]
fn file_id_of(_meta: &std::fs::Metadata) -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn unique_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lwa_fm_fs_enum_{tag}_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    fn collect(dir: &Path) -> BTreeMap<String, (bool, u64, Option<SystemTime>)> {
        let mut out = BTreeMap::new();
        enumerate(dir, &mut |entry| {
            let mut name = String::new();
            entry.name.append_utf8(&mut name);
            out.insert(name, (entry.is_dir(), entry.size, entry.modified));
        })
        .expect("enumerate");
        out
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn packages_list_as_single_items() {
        let dir = unique_dir("packages");
        std::fs::create_dir_all(dir.join("Tool.app/Contents")).expect("app");
        std::fs::create_dir(dir.join("Notes.rtfd")).expect("rtfd");
        std::fs::create_dir(dir.join("conf.d")).expect("dir");
        let mut types = BTreeMap::new();
        enumerate(&dir, &mut |entry| {
            types.insert(
                entry.name.to_string_lossy().into_owned(),
                entry.metadata().entry_type,
            );
        })
        .expect("enumerate");
        assert_eq!(types["Tool.app"], EntryType::Package);
        assert_eq!(types["Notes.rtfd"], EntryType::Package);
        assert_eq!(types["conf.d"], EntryType::Directory);
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_hidden_flag_hides() {
        let dir = unique_dir("hidden_flag");
        std::fs::write(dir.join("flagged"), b"").expect("write");
        std::fs::write(dir.join("plain"), b"").expect("write");
        assert!(
            std::process::Command::new("chflags")
                .arg("hidden")
                .arg(dir.join("flagged"))
                .status()
                .expect("chflags")
                .success()
        );
        let mut hidden = BTreeMap::new();
        enumerate(&dir, &mut |entry| {
            hidden.insert(
                entry.name.to_string_lossy().into_owned(),
                entry.is_hidden_attr(),
            );
        })
        .expect("enumerate");
        assert!(hidden["flagged"]);
        assert!(!hidden["plain"]);
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn names_sizes_and_types_match_std() {
        let dir = unique_dir("basic");
        std::fs::write(dir.join("plain.txt"), b"hello").expect("write");
        std::fs::write(dir.join("zażółć gęślą.dat"), b"xy").expect("write");
        std::fs::create_dir(dir.join("sub")).expect("mkdir");
        let entries = collect(&dir);
        assert_eq!(
            entries.keys().cloned().collect::<Vec<_>>(),
            ["plain.txt", "sub", "zażółć gęślą.dat"]
        );
        let meta = std::fs::metadata(dir.join("plain.txt")).expect("meta");
        assert_eq!(entries["plain.txt"], (false, 5, meta.modified().ok()));
        assert_eq!(entries["zażółć gęślą.dat"].1, 2);
        assert!(entries["sub"].0);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Many entries with every name length from 1 to 120 characters span
    /// several 64 KiB kernel buffers and put short records at buffer ends.
    #[test]
    fn large_directories_match_std_exactly() {
        let dir = unique_dir("large");
        let mut expected = std::collections::BTreeSet::new();
        for index in 0..3_000usize {
            let len = 1 + index % 120;
            let stem = format!("{index}{}", "x".repeat(len));
            let name: String = stem
                .chars()
                .take(len.max(index.to_string().len()))
                .collect();
            if expected.insert(name.clone()) {
                std::fs::write(dir.join(&name), b"").expect("write");
            }
        }
        let mut native = std::collections::BTreeSet::new();
        enumerate(&dir, &mut |entry| {
            native.insert(entry.name.to_string_lossy().into_owned());
        })
        .expect("enumerate");
        let mut via_std = std::collections::BTreeSet::new();
        enumerate_std(&dir, &mut |entry| {
            via_std.insert(entry.name.to_string_lossy().into_owned());
        })
        .expect("enumerate_std");
        assert_eq!(native.len(), expected.len());
        assert_eq!(native, expected);
        assert_eq!(via_std, expected);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn empty_and_missing_directories() {
        let dir = unique_dir("empty");
        assert!(collect(&dir).is_empty());
        let missing = dir.join("nope");
        assert!(enumerate(&missing, &mut |_| {}).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn metadata_conversion_marks_directories() {
        let raw = RawEntry {
            name: EntryName::Str(Cow::Borrowed("x")),
            attributes: ATTR_DIRECTORY | ATTR_HIDDEN,
            reparse_tag: 0,
            size: 7,
            created: None,
            modified: Some(UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000)),
            file_id: 0,
        };
        let meta = raw.metadata();
        assert_eq!(meta.entry_type, EntryType::Directory);
        assert_eq!(meta.size, 7);
        assert_eq!(meta.source_revision, 1_700_000_000 * 1_000_000_000);
        assert!(raw.is_hidden_attr());
        assert!(!raw.is_symlink());
        assert!(raw.name.starts_with_ascii(b'x'));
    }

    #[test]
    fn utf16_names_decode_with_ascii_fast_path() {
        let ascii: Vec<u16> = "file.txt".encode_utf16().collect();
        let unicode: Vec<u16> = "zażółć".encode_utf16().collect();
        let mut out = String::new();
        EntryName::Utf16(&ascii).append_utf8(&mut out);
        EntryName::Utf16(&unicode).append_utf8(&mut out);
        assert_eq!(out, "file.txtzażółć");
        assert!(EntryName::Utf16(&ascii).starts_with_ascii(b'f'));
        assert_eq!(EntryName::Utf16(&unicode).to_string_lossy(), "zażółć");
        // A lone high surrogate is replaced rather than dropped.
        let mut lossy = String::new();
        EntryName::Utf16(&[0xD800, u16::from(b'a')]).append_utf8(&mut lossy);
        assert_eq!(lossy, "\u{FFFD}a");
    }

    #[cfg(windows)]
    #[test]
    fn windows_attributes_and_junctions() {
        let dir = unique_dir("attrs");
        std::fs::write(dir.join("hidden.txt"), b"h").expect("write");
        let status = std::process::Command::new("attrib")
            .arg("+h")
            .arg(dir.join("hidden.txt"))
            .status()
            .expect("attrib");
        assert!(status.success());
        std::fs::create_dir(dir.join("target")).expect("mkdir");
        let junction = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(dir.join("link"))
            .arg(dir.join("target"))
            .output()
            .expect("mklink");
        assert!(junction.status.success(), "{junction:?}");
        let mut seen = BTreeMap::new();
        enumerate(&dir, &mut |entry| {
            seen.insert(
                entry.name.to_string_lossy().into_owned(),
                (entry.is_hidden_attr(), entry.is_dir(), entry.is_symlink()),
            );
        })
        .expect("enumerate");
        assert_eq!(seen["hidden.txt"], (true, false, false));
        assert_eq!(seen["target"], (false, true, false));
        assert_eq!(seen["link"], (false, true, true));
        // The std fallback agrees on the shape of every entry.
        let mut via_std = BTreeMap::new();
        enumerate_std(&dir, &mut |entry| {
            via_std.insert(
                entry.name.to_string_lossy().into_owned(),
                (entry.is_hidden_attr(), entry.is_dir()),
            );
        })
        .expect("enumerate_std");
        for (name, (hidden, is_dir, _)) in &seen {
            assert_eq!(via_std[name], (*hidden, *is_dir), "{name}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
