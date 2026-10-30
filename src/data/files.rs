use crate::{
    app::dir_handling::COLLATER,
    data::time::{ElapsedTime, TimestampSeconds},
    fs_enum::{EntryName, enumerate, enumerate_std},
    helper::{PathHelper, canonical_string},
};
use bincode::{Decode, Encode};
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};
use std::{
    fs::FileType,
    path::{Path, PathBuf},
};

/// Bumped whenever the persisted [`Listing`] encoding changes, so stale cache
/// records decode as a version mismatch instead of garbage.
const LISTING_FORMAT_VERSION: u8 = 4;
/// Below this many entries the rayon dispatch costs more than it saves.
const PARALLEL_THRESHOLD: usize = 2048;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize, Decode, Encode, Hash,
)]
pub enum EntryType {
    File,
    Directory,
    /// A directory the system presents as a single item (a macOS `.app`,
    /// `.rtfd`, ...): opened like a file instead of browsed into.
    Package,
}

impl EntryType {
    /// Whether this is opened rather than browsed into: a file or a package.
    #[inline]
    #[must_use]
    pub const fn is_leaf(self) -> bool {
        matches!(self, Self::File | Self::Package)
    }

    /// The type of a directory at `path`, telling packages apart.
    #[must_use]
    pub fn of_dir(path: &Path) -> Self {
        if crate::fs_enum::is_package(path) {
            Self::Package
        } else {
            Self::Directory
        }
    }
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

#[derive(Debug, Clone, Copy, Deserialize, Serialize, Decode, Encode, PartialEq, Eq, Hash)]
pub struct DirEntryMetaData {
    pub entry_type: EntryType,
    pub created_at: TimestampSeconds,
    pub modified_at: TimestampSeconds,
    /// Last access as tracked by the filesystem; 0 when unknown.
    pub accessed_at: TimestampSeconds,
    /// Nanoseconds since the Unix epoch, used to invalidate derived assets.
    pub source_revision: u128,
    pub since_modified: ElapsedTime,
    pub size: u64,
    /// Attribute bits kept from the directory read.
    pub flags: EntryFlags,
    /// Volume-unique file identifier, 0 when unknown.
    pub file_id: u64,
}

/// Compact copy of the platform file attributes a listing needs.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Deserialize,
    Serialize,
    Decode,
    Encode,
)]
pub struct EntryFlags(u8);

impl EntryFlags {
    pub const HIDDEN: Self = Self(1);
    pub const SYSTEM: Self = Self(1 << 1);
    pub const SYMLINK: Self = Self(1 << 2);
    pub const READONLY: Self = Self(1 << 3);
    pub const REPARSE: Self = Self(1 << 4);

    #[inline]
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[inline]
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Hidden the way Explorer hides things: the hidden or system attribute.
    #[inline]
    #[must_use]
    pub const fn is_hidden(self) -> bool {
        self.0 & (Self::HIDDEN.0 | Self::SYSTEM.0) != 0
    }

    /// Flags from `FILE_ATTRIBUTE_*` bits; `symlink` says whether the reparse
    /// point is a link (the attribute alone cannot tell).
    #[must_use]
    pub const fn from_attributes(attributes: u32, symlink: bool) -> Self {
        use crate::fs_enum::{ATTR_HIDDEN, ATTR_READONLY, ATTR_REPARSE_POINT, ATTR_SYSTEM};
        let mut bits = 0;
        if attributes & ATTR_HIDDEN != 0 {
            bits |= Self::HIDDEN.0;
        }
        if attributes & ATTR_SYSTEM != 0 {
            bits |= Self::SYSTEM.0;
        }
        if attributes & ATTR_READONLY != 0 {
            bits |= Self::READONLY.0;
        }
        if attributes & ATTR_REPARSE_POINT != 0 {
            bits |= Self::REPARSE.0;
        }
        if symlink {
            bits |= Self::SYMLINK.0;
        }
        Self(bits)
    }
}

/// The one hidden rule: a hidden name (see [`is_hidden_name`]) or the hidden
/// attribute: Explorer's hidden or system attribute on Windows, the
/// `UF_HIDDEN` flag (`chflags hidden`, as on `~/Library`) on macOS.
#[inline]
#[must_use]
pub fn is_hidden_entry(name: &str, meta: &DirEntryMetaData) -> bool {
    meta.flags.is_hidden() || is_hidden_name(name)
}

/// A name that hides its item: a leading `.`, or on Windows a leading `$`
/// (`$Recycle.Bin`, `$WinREAgent`). On other systems `$` is an ordinary
/// first character.
#[inline]
#[must_use]
pub fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.') || (cfg!(windows) && name.starts_with('$'))
}

/// Finder's bookkeeping files (`.DS_Store`, `._` resource fork companions on
/// non-Mac volumes): never listed, not even with hidden items shown, as in
/// Finder.
#[inline]
#[must_use]
pub fn is_system_clutter(name: &str) -> bool {
    cfg!(target_os = "macos") && (name == ".DS_Store" || name.starts_with("._"))
}

impl From<EntryType> for DirEntryMetaData {
    #[inline]
    fn from(entry_type: EntryType) -> Self {
        Self {
            entry_type,
            created_at: TimestampSeconds::default(),
            modified_at: TimestampSeconds::default(),
            accessed_at: TimestampSeconds::default(),
            source_revision: 0,
            since_modified: ElapsedTime::default(),
            size: 0,
            flags: EntryFlags::default(),
            file_id: 0,
        }
    }
}

impl From<std::fs::Metadata> for DirEntryMetaData {
    #[inline]
    fn from(meta: std::fs::Metadata) -> Self {
        let entry_type: EntryType = meta.file_type().into();
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
            .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_nanos());
        #[cfg(windows)]
        let size = std::os::windows::fs::MetadataExt::file_size(&meta);
        #[cfg(target_os = "linux")]
        let size = std::os::linux::fs::MetadataExt::st_size(&meta);
        #[cfg(target_os = "macos")]
        let size = std::os::macos::fs::MetadataExt::st_size(&meta);
        #[cfg(windows)]
        let attributes = std::os::windows::fs::MetadataExt::file_attributes(&meta);
        #[cfg(target_os = "macos")]
        let attributes = crate::fs_enum::macos_hidden_attr(&meta);
        #[cfg(not(any(windows, target_os = "macos")))]
        let attributes = 0;
        Self {
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
}

/// An owned entry, for the few places that must keep one beyond the listing it
/// came from (rename modal, inspector, tests). Rows are rendered from
/// [`EntryRef`]s that borrow the shared [`Listing`] instead.
#[derive(Debug, Clone, Serialize, Deserialize, Decode, Encode)]
pub struct DirEntry {
    pub meta: DirEntryMetaData,
    pub dir: Arc<str>,
    pub file_name: String,
}

impl DirEntry {
    #[inline]
    #[must_use]
    pub fn entry_ref(&self) -> EntryRef<'_> {
        EntryRef {
            dir: &self.dir,
            file_name: &self.file_name,
            meta: &self.meta,
        }
    }

    #[must_use]
    pub fn full_path_string(&self) -> String {
        self.entry_ref().full_path_string()
    }

    #[must_use]
    pub fn get_path(&self) -> PathBuf {
        self.entry_ref().get_path()
    }

    /// A file or a package: opened rather than browsed into.
    #[inline]
    #[must_use]
    pub const fn is_file(&self) -> bool {
        self.meta.entry_type.is_leaf()
    }

    #[inline]
    #[must_use]
    pub const fn is_package(&self) -> bool {
        matches!(self.meta.entry_type, EntryType::Package)
    }

    #[inline]
    #[must_use]
    pub fn get_splitted_path(&self) -> (&str, &str) {
        (&self.dir, &self.file_name)
    }

    #[cfg(test)]
    #[must_use]
    pub fn test_new(path: &str) -> Self {
        let sep = path
            .rfind(std::path::MAIN_SEPARATOR)
            .or_else(|| path.rfind('/'));
        let (dir, file_name) = sep.map_or(("", path), |i| (&path[..i], &path[i + 1..]));
        // Normalize forward slashes to platform separator (preserving leading /)
        let dir = if dir.is_empty() {
            Arc::from("")
        } else if let Some(stripped) = dir.strip_prefix('/') {
            let normalized = format!(
                "{}{}",
                std::path::MAIN_SEPARATOR,
                stripped.replace('/', std::path::MAIN_SEPARATOR_STR)
            );
            Arc::from(normalized.as_str())
        } else {
            Arc::from(dir.replace('/', std::path::MAIN_SEPARATOR_STR).as_str())
        };
        Self {
            meta: EntryType::File.into(),
            dir,
            file_name: file_name.to_string(),
        }
    }
}

impl TryFrom<std::fs::DirEntry> for DirEntry {
    type Error = ();

    #[inline]
    fn try_from(value: std::fs::DirEntry) -> Result<Self, Self::Error> {
        let file_type = value.file_type().map_err(|_| ())?;
        let mut entry_type: EntryType = file_type.into();
        if entry_type == EntryType::Directory {
            entry_type = EntryType::of_dir(&value.path());
        }
        Ok(Self {
            meta: entry_type.into(),
            dir: entry_dir(&value.path()),
            file_name: value.file_name().to_string_lossy().into_owned(),
        })
    }
}

/// Directory part of an entry path, normalised lexically. Callers walk roots
/// that were already normalised once, so no per-entry `canonicalize` is needed.
fn entry_dir(path: &Path) -> Arc<str> {
    path.parent().map_or_else(
        || Arc::from(""),
        |parent| Arc::from(canonical_string(parent)),
    )
}

/// A borrowed view of one listing row. Cheap to create every frame: it is
/// three references, with no string or key cloning.
#[derive(Debug, Clone, Copy)]
pub struct EntryRef<'a> {
    pub dir: &'a Arc<str>,
    pub file_name: &'a str,
    pub meta: &'a DirEntryMetaData,
}

impl EntryRef<'_> {
    /// A file or a package: opened rather than browsed into.
    #[inline]
    #[must_use]
    pub const fn is_file(&self) -> bool {
        self.meta.entry_type.is_leaf()
    }

    #[inline]
    #[must_use]
    pub const fn is_package(&self) -> bool {
        matches!(self.meta.entry_type, EntryType::Package)
    }

    #[inline]
    #[must_use]
    pub fn get_splitted_path(&self) -> (&str, &str) {
        (self.dir, self.file_name)
    }

    #[must_use]
    pub fn full_path_string(&self) -> String {
        let mut s = String::with_capacity(self.dir.len() + 1 + self.file_name.len());
        s.push_str(self.dir);
        s.push(std::path::MAIN_SEPARATOR);
        s.push_str(self.file_name);
        s
    }

    #[must_use]
    pub fn get_path(&self) -> PathBuf {
        PathBuf::from(self.full_path_string())
    }

    #[must_use]
    pub fn to_entry(&self) -> DirEntry {
        DirEntry {
            meta: *self.meta,
            dir: Arc::clone(self.dir),
            file_name: self.file_name.to_owned(),
        }
    }
}

/// ICU collation keys for every entry, stored in one arena.
#[derive(Debug, Clone, Default, Encode, Decode)]
pub struct SortKeys {
    bytes: Vec<u8>,
    ofs: Vec<u32>,
}

impl SortKeys {
    #[inline]
    #[must_use]
    pub fn get(&self, index: usize) -> &[u8] {
        &self.bytes[self.ofs[index] as usize..self.ofs[index + 1] as usize]
    }

    fn build(listing: &Listing) -> Self {
        let build_range = |range: std::ops::Range<usize>| {
            let mut keys = Self {
                bytes: Vec::with_capacity(range.len() * 24),
                ofs: Vec::with_capacity(range.len()),
            };
            for index in range {
                keys.ofs.push(keys.bytes.len() as u32);
                _ = COLLATER.write_sort_key_to(listing.name(index), &mut keys.bytes);
            }
            keys
        };
        let len = listing.len();
        let mut keys = if len < PARALLEL_THRESHOLD {
            build_range(0..len)
        } else {
            let chunks: Vec<Self> = (0..len.div_ceil(PARALLEL_THRESHOLD))
                .into_par_iter()
                .map(|chunk| {
                    let start = chunk * PARALLEL_THRESHOLD;
                    build_range(start..(start + PARALLEL_THRESHOLD).min(len))
                })
                .collect();
            let mut merged = Self {
                bytes: Vec::with_capacity(chunks.iter().map(|c| c.bytes.len()).sum()),
                ofs: Vec::with_capacity(len + 1),
            };
            for chunk in chunks {
                let base = merged.bytes.len() as u32;
                merged.ofs.extend(chunk.ofs.iter().map(|ofs| ofs + base));
                merged.bytes.extend_from_slice(&chunk.bytes);
            }
            merged
        };
        keys.ofs.push(keys.bytes.len() as u32);
        keys
    }
}

/// Columnar, arena-backed directory listing.
///
/// All names live in one `String`; metadata lives in one `Vec`. A listing is
/// immutable once built and shared through an [`Arc`] between the directory
/// cache and every tab showing it, so a cache hit is an `Arc::clone`. Tabs keep
/// sort order and filter results as `u32` index permutations over it.
#[derive(Clone, Default)]
pub struct Listing {
    dirs: Vec<Arc<str>>,
    names: String,
    /// Start offset of each name in `names`; one extra trailing offset.
    name_ofs: Vec<u32>,
    /// Index into `dirs` per entry. Empty when every entry shares `dirs[0]`.
    dir_idx: Vec<u32>,
    meta: Vec<DirEntryMetaData>,
    sort_keys: OnceLock<SortKeys>,
    /// Case-folded names for plain search; see [`Listing::folded_names`].
    folded: OnceLock<FoldedNames>,
}

impl std::fmt::Debug for Listing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Listing")
            .field("dirs", &self.dirs.len())
            .field("entries", &self.len())
            .finish_non_exhaustive()
    }
}

/// Lower-cased copies of every name in one arena, built on first search.
#[derive(Debug, Clone, Default)]
pub struct FoldedNames {
    text: String,
    ofs: Vec<u32>,
    /// Whether each name is pure ASCII (folding is then byte-exact).
    ascii: Vec<bool>,
}

impl FoldedNames {
    #[inline]
    #[must_use]
    pub fn get(&self, index: usize) -> &str {
        &self.text[self.ofs[index] as usize..self.ofs[index + 1] as usize]
    }

    #[inline]
    #[must_use]
    pub fn is_ascii(&self, index: usize) -> bool {
        self.ascii[index]
    }
}

#[derive(Encode)]
struct StoredListingRef<'a> {
    dirs: Vec<&'a str>,
    names: &'a str,
    name_ofs: &'a [u32],
    dir_idx: &'a [u32],
    meta: &'a [DirEntryMetaData],
    sort_keys: Option<&'a SortKeys>,
}

#[derive(Decode)]
struct StoredListing {
    dirs: Vec<String>,
    names: String,
    name_ofs: Vec<u32>,
    dir_idx: Vec<u32>,
    meta: Vec<DirEntryMetaData>,
    sort_keys: Option<SortKeys>,
}

/// Incremental [`Listing`] construction.
#[derive(Default)]
pub struct ListingBuilder {
    listing: Listing,
    dir_lookup: foldhash::HashMap<Arc<str>, u32>,
    keys: Option<SortKeys>,
}

impl ListingBuilder {
    #[must_use]
    pub fn with_capacity(entries: usize) -> Self {
        let mut builder = Self::default();
        builder.listing.names.reserve(entries * 16);
        builder.listing.name_ofs.reserve(entries + 1);
        builder.listing.meta.reserve(entries);
        builder
    }

    /// Single-directory builder that optionally carries collation keys.
    fn for_dir(dir: &Arc<str>, entries: usize, with_keys: bool) -> Self {
        let mut builder = Self::with_capacity(entries);
        builder.intern_dir(dir);
        if with_keys {
            builder.keys = Some(SortKeys {
                bytes: Vec::with_capacity(entries * 24),
                ofs: Vec::with_capacity(entries + 1),
            });
        }
        builder
    }

    pub(crate) fn intern_dir(&mut self, dir: &Arc<str>) -> u32 {
        if let Some(&index) = self.dir_lookup.get(dir) {
            return index;
        }
        let index = self.listing.dirs.len() as u32;
        self.listing.dirs.push(Arc::clone(dir));
        self.dir_lookup.insert(Arc::clone(dir), index);
        index
    }

    pub fn push(&mut self, dir: &Arc<str>, name: &str, meta: DirEntryMetaData) {
        let dir_index = self.intern_dir(dir);
        self.push_indexed(dir_index, name, meta, None);
    }

    /// Appends an entry whose name comes straight from a directory read,
    /// decoding it into the arena without an intermediate `String`.
    pub fn push_raw(&mut self, dir_index: u32, name: &EntryName<'_>, meta: DirEntryMetaData) {
        let start = self.begin_entry(dir_index, meta);
        name.append_utf8(&mut self.listing.names);
        self.end_entry(start, None);
    }

    fn push_indexed(
        &mut self,
        dir_index: u32,
        name: &str,
        meta: DirEntryMetaData,
        key: Option<&[u8]>,
    ) {
        let start = self.begin_entry(dir_index, meta);
        self.listing.names.push_str(name);
        self.end_entry(start, key);
    }

    /// Records everything but the name and returns the name's start offset.
    fn begin_entry(&mut self, dir_index: u32, meta: DirEntryMetaData) -> usize {
        let listing = &mut self.listing;
        if dir_index != 0 && listing.dir_idx.is_empty() {
            listing.dir_idx = vec![0; listing.meta.len()];
        }
        if !listing.dir_idx.is_empty() {
            listing.dir_idx.push(dir_index);
        }
        let start = listing.names.len();
        listing.name_ofs.push(start as u32);
        listing.meta.push(meta);
        start
    }

    /// Adds the collation key for the name written since `start`.
    fn end_entry(&mut self, start: usize, key: Option<&[u8]>) {
        if let Some(keys) = &mut self.keys {
            keys.ofs.push(keys.bytes.len() as u32);
            match key {
                Some(key) => keys.bytes.extend_from_slice(key),
                None => {
                    _ = COLLATER.write_sort_key_to(&self.listing.names[start..], &mut keys.bytes);
                }
            }
        }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.listing.meta.is_empty()
    }

    #[must_use]
    pub fn finish(mut self) -> Listing {
        self.listing.name_ofs.push(self.listing.names.len() as u32);
        if let Some(mut keys) = self.keys {
            keys.ofs.push(keys.bytes.len() as u32);
            _ = self.listing.sort_keys.set(keys);
        }
        self.listing
    }
}

/// A watcher-driven edit to a single-directory listing.
pub enum ListingChange<'a> {
    Upsert(&'a str, DirEntryMetaData),
    Update(&'a str, DirEntryMetaData),
    Remove(&'a str),
}

impl Listing {
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.meta.len()
    }

    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.meta.is_empty()
    }

    #[inline]
    #[must_use]
    pub fn name(&self, index: usize) -> &str {
        &self.names[self.name_ofs[index] as usize..self.name_ofs[index + 1] as usize]
    }

    #[inline]
    #[must_use]
    pub fn dir(&self, index: usize) -> &Arc<str> {
        &self.dirs[self.dir_index(index)]
    }

    /// Index into [`Self::dirs`] of the folder holding entry `index`.
    #[inline]
    #[must_use]
    pub fn dir_index(&self, index: usize) -> usize {
        self.dir_idx.get(index).map_or(0, |&dir| dir as usize)
    }

    #[inline]
    #[must_use]
    pub fn meta(&self, index: usize) -> &DirEntryMetaData {
        &self.meta[index]
    }

    #[inline]
    #[must_use]
    pub fn is_file(&self, index: usize) -> bool {
        self.meta[index].entry_type.is_leaf()
    }

    #[inline]
    #[must_use]
    pub fn entry(&self, index: usize) -> EntryRef<'_> {
        EntryRef {
            dir: self.dir(index),
            file_name: self.name(index),
            meta: &self.meta[index],
        }
    }

    #[inline]
    #[must_use]
    pub fn dirs(&self) -> &[Arc<str>] {
        &self.dirs
    }

    /// The only directory of a single-directory listing.
    #[inline]
    #[must_use]
    pub fn single_dir(&self) -> Option<&Arc<str>> {
        (self.dirs.len() == 1).then(|| &self.dirs[0])
    }

    /// Rough heap footprint, for byte-budgeted caches.
    #[must_use]
    pub fn approx_bytes(&self) -> usize {
        self.names.capacity()
            + self.name_ofs.capacity() * 4
            + self.dir_idx.capacity() * 4
            + self.meta.capacity() * std::mem::size_of::<DirEntryMetaData>()
            + self
                .sort_keys
                .get()
                .map_or(0, |keys| keys.bytes.capacity() + keys.ofs.capacity() * 4)
            + self
                .dirs
                .iter()
                .map(|dir| dir.len() + std::mem::size_of::<Arc<str>>())
                .sum::<usize>()
    }

    pub fn iter(&self) -> impl Iterator<Item = EntryRef<'_>> {
        (0..self.len()).map(|index| self.entry(index))
    }

    /// ICU collation keys, computed (in parallel) on first use and then shared
    /// by every holder of this listing.
    pub fn sort_keys(&self) -> &SortKeys {
        self.sort_keys.get_or_init(|| SortKeys::build(self))
    }

    pub fn folded_names(&self) -> &FoldedNames {
        self.folded.get_or_init(|| {
            let mut folded = FoldedNames {
                text: String::with_capacity(self.names.len()),
                ofs: Vec::with_capacity(self.len() + 1),
                ascii: Vec::with_capacity(self.len()),
            };
            for index in 0..self.len() {
                let name = self.name(index);
                folded.ofs.push(folded.text.len() as u32);
                let ascii = name.is_ascii();
                if ascii {
                    folded.text.extend(
                        name.bytes()
                            .map(|byte| char::from(byte.to_ascii_lowercase())),
                    );
                } else {
                    // Composed first, so a decomposed "é" (HFS+, SMB) matches
                    // a typed one.
                    use unicode_normalization::UnicodeNormalization;
                    folded.text.extend(name.nfc().flat_map(char::to_lowercase));
                }
                folded.ascii.push(ascii);
            }
            folded.ofs.push(folded.text.len() as u32);
            folded
        })
    }

    /// Read one directory with the bulk enumerator, falling back to
    /// `std::fs::read_dir` on volumes that reject it. Collation keys are built
    /// eagerly here (on the background thread, in parallel for large
    /// directories) so they are persisted with the cache entry.
    #[must_use]
    pub fn read(dir: &Path) -> Option<Self> {
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

    /// Concatenate listings (multi-root views). Sort keys are carried over
    /// when every part already has them.
    #[must_use]
    pub fn concat<'a>(parts: impl Iterator<Item = &'a Self> + Clone) -> Self {
        let entries = parts.clone().map(Self::len).sum();
        let with_keys = parts.clone().all(|part| part.sort_keys.get().is_some());
        let mut builder = ListingBuilder::with_capacity(entries);
        if with_keys {
            builder.keys = Some(SortKeys::default());
        }
        for part in parts {
            let keys = part.sort_keys.get();
            for index in 0..part.len() {
                let dir_index = builder.intern_dir(part.dir(index));
                builder.push_indexed(
                    dir_index,
                    part.name(index),
                    part.meta[index],
                    keys.map(|keys| keys.get(index)),
                );
            }
        }
        builder.finish()
    }

    #[must_use]
    pub fn from_entries(entries: &[DirEntry]) -> Self {
        let mut builder = ListingBuilder::with_capacity(entries.len());
        for entry in entries {
            builder.push(&entry.dir, &entry.file_name, entry.meta);
        }
        builder.finish()
    }

    #[must_use]
    pub fn position(&self, name: &str) -> Option<usize> {
        (0..self.len()).find(|&index| self.name(index) == name)
    }

    /// Apply watcher changes to a single-directory listing, producing a new
    /// listing (the original may be shared with the cache and other tabs).
    /// Existing collation keys are carried over rather than recomputed.
    /// Returns `None` if nothing changed.
    #[must_use]
    pub fn with_changes(&self, changes: &[ListingChange<'_>]) -> Option<Self> {
        let dir = Arc::clone(self.dirs.first()?);
        let mut removed = vec![false; self.len()];
        let mut meta = self.meta.clone();
        let mut appended: Vec<(&str, DirEntryMetaData)> = Vec::new();
        let mut any_change = false;
        // A batch against a large listing looks names up through one index
        // instead of a linear scan per change.
        let index: Option<foldhash::HashMap<&str, usize>> = (changes.len() > 1 && self.len() > 512)
            .then(|| {
                (0..self.len())
                    .map(|index| (self.name(index), index))
                    .collect()
            });
        let position = |name: &str| {
            index
                .as_ref()
                .map_or_else(|| self.position(name), |index| index.get(name).copied())
        };
        for change in changes {
            match *change {
                ListingChange::Update(name, new_meta) => {
                    if let Some(index) = position(name) {
                        meta[index] = new_meta;
                        any_change = true;
                    }
                }
                ListingChange::Upsert(name, new_meta) => {
                    any_change = true;
                    if let Some(index) = position(name) {
                        meta[index] = new_meta;
                        removed[index] = false;
                    } else if let Some(slot) = appended.iter_mut().find(|(n, _)| *n == name) {
                        slot.1 = new_meta;
                    } else {
                        appended.push((name, new_meta));
                    }
                }
                ListingChange::Remove(name) => {
                    if let Some(index) = position(name) {
                        any_change |= !removed[index];
                        removed[index] = true;
                    }
                    let before = appended.len();
                    appended.retain(|(n, _)| *n != name);
                    any_change |= appended.len() != before;
                }
            }
        }
        if !any_change {
            return None;
        }
        if !removed.contains(&true) && appended.is_empty() {
            // Metadata-only: arenas (and indices) are unchanged.
            let mut next = self.clone();
            next.meta = meta;
            return Some(next);
        }
        let keys = self.sort_keys.get();
        let mut builder =
            ListingBuilder::for_dir(&dir, self.len() + appended.len(), keys.is_some());
        for index in (0..self.len()).filter(|&index| !removed[index]) {
            builder.push_indexed(
                0,
                self.name(index),
                meta[index],
                keys.map(|keys| keys.get(index)),
            );
        }
        for (name, new_meta) in appended {
            builder.push_indexed(0, name, new_meta, None);
        }
        Some(builder.finish())
    }

    pub fn encode(&self) -> Option<Vec<u8>> {
        let stored = StoredListingRef {
            dirs: self.dirs.iter().map(AsRef::as_ref).collect(),
            names: &self.names,
            name_ofs: &self.name_ofs,
            dir_idx: &self.dir_idx,
            meta: &self.meta,
            sort_keys: self.sort_keys.get(),
        };
        bincode::encode_to_vec(
            (LISTING_FORMAT_VERSION, stored),
            bincode::config::standard(),
        )
        .ok()
    }

    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let ((version, stored), _): ((u8, StoredListing), _) =
            bincode::decode_from_slice(bytes, bincode::config::standard()).ok()?;
        if version != LISTING_FORMAT_VERSION
            || stored.name_ofs.len() != stored.meta.len() + 1
            || stored.dirs.is_empty()
            || stored.name_ofs.last().copied() != Some(stored.names.len() as u32)
            || !(stored.dir_idx.is_empty() || stored.dir_idx.len() == stored.meta.len())
            || stored
                .name_ofs
                .iter()
                .any(|&ofs| !stored.names.is_char_boundary(ofs as usize))
        {
            return None;
        }
        let listing = Self {
            dirs: stored.dirs.into_iter().map(Arc::from).collect(),
            names: stored.names,
            name_ofs: stored.name_ofs,
            dir_idx: stored.dir_idx,
            meta: stored.meta,
            sort_keys: OnceLock::new(),
            folded: OnceLock::new(),
        };
        if let Some(keys) = stored.sort_keys
            && keys.ofs.len() == listing.len() + 1
        {
            _ = listing.sort_keys.set(keys);
        }
        Some(listing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing_of(names: &[&str]) -> Listing {
        let entries: Vec<DirEntry> = names
            .iter()
            .map(|name| DirEntry::test_new(&format!("/a/{name}")))
            .collect();
        Listing::from_entries(&entries)
    }

    #[test]
    fn listing_preserves_order_and_shares_dir() {
        let listing = listing_of(&["one.txt", "two.dat", "three.log"]);
        assert_eq!(listing.len(), 3);
        assert_eq!(listing.dirs().len(), 1);
        for (index, name) in ["one.txt", "two.dat", "three.log"].iter().enumerate() {
            let entry = listing.entry(index);
            assert_eq!(entry.file_name, *name);
            assert!(entry.is_file());
            assert!(Arc::ptr_eq(entry.dir, &listing.dirs()[0]));
        }
    }

    #[test]
    fn multi_dir_listing_tracks_each_entry_dir() {
        let entries = [
            DirEntry::test_new("/a/x.txt"),
            DirEntry::test_new("/b/y.txt"),
            DirEntry::test_new("/a/z.txt"),
        ];
        let listing = Listing::from_entries(&entries);
        assert_eq!(listing.dirs().len(), 2);
        assert_eq!(listing.dir(1).as_ref(), entries[1].dir.as_ref());
        assert_eq!(listing.dir(2).as_ref(), entries[0].dir.as_ref());
    }

    #[test]
    fn encode_round_trips_with_sort_keys() {
        let listing = listing_of(&["b", "a", "ż"]);
        let _ = listing.sort_keys();
        let decoded = Listing::decode(&listing.encode().expect("encode")).expect("decode");
        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded.name(2), "ż");
        assert!(decoded.sort_keys.get().is_some());
        assert_eq!(decoded.sort_keys().get(1), listing.sort_keys().get(1));
        assert!(Listing::decode(b"garbage").is_none());
    }

    #[test]
    fn encode_keeps_flags_and_file_ids_and_rejects_old_versions() {
        let mut entry = DirEntry::test_new("/a/hidden.txt");
        entry.meta.flags = EntryFlags::HIDDEN.union(EntryFlags::READONLY);
        entry.meta.file_id = 0x1234_5678_9abc;
        let listing = Listing::from_entries(&[entry]);
        let bytes = listing.encode().expect("encode");
        let decoded = Listing::decode(&bytes).expect("decode");
        assert_eq!(
            decoded.meta(0).flags,
            EntryFlags::HIDDEN.union(EntryFlags::READONLY)
        );
        assert!(decoded.meta(0).flags.is_hidden());
        assert_eq!(decoded.meta(0).file_id, 0x1234_5678_9abc);
        // A record written by the previous format version is discarded.
        let mut old = bytes;
        old[0] = LISTING_FORMAT_VERSION - 1;
        assert!(Listing::decode(&old).is_none());
    }

    #[test]
    fn batched_changes_on_large_listings_match_one_at_a_time() {
        let names: Vec<String> = (0..5_000).map(|index| format!("f{index:05}.txt")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let listing = listing_of(&refs);
        let mut meta: DirEntryMetaData = EntryType::File.into();
        meta.size = 9;
        let new_names: Vec<String> = (0..40).map(|index| format!("new{index}.txt")).collect();
        let mut changes = Vec::new();
        for index in (0..5_000).step_by(97) {
            changes.push(ListingChange::Remove(refs[index]));
        }
        for index in (1..5_000).step_by(131) {
            changes.push(ListingChange::Update(refs[index], meta));
        }
        for name in &new_names {
            changes.push(ListingChange::Upsert(name, meta));
        }
        let batched = listing.with_changes(&changes).expect("batched");
        let mut sequential = listing;
        for change in &changes {
            if let Some(next) = sequential.with_changes(std::slice::from_ref(change)) {
                sequential = next;
            }
        }
        let rows = |listing: &Listing| {
            listing
                .iter()
                .map(|entry| (entry.file_name.to_owned(), entry.meta.size))
                .collect::<Vec<_>>()
        };
        assert_eq!(rows(&batched), rows(&sequential));
        assert_eq!(batched.len(), 5_000 - 52 + 40);
    }

    #[test]
    fn changes_carry_keys_and_apply_in_order() {
        let listing = listing_of(&["a", "b", "c"]);
        let _ = listing.sort_keys();
        let mut meta: DirEntryMetaData = EntryType::File.into();
        meta.size = 42;
        let next = listing
            .with_changes(&[
                ListingChange::Remove("b"),
                ListingChange::Upsert("d", meta),
                ListingChange::Update("a", meta),
            ])
            .expect("changed");
        let names: Vec<_> = next.iter().map(|e| e.file_name).collect();
        assert_eq!(names, ["a", "c", "d"]);
        assert_eq!(next.meta(0).size, 42);
        assert!(next.sort_keys.get().is_some());
        assert_eq!(next.sort_keys().get(1), listing.sort_keys().get(2));
        assert!(
            listing
                .with_changes(&[ListingChange::Remove("zz")])
                .is_none()
        );
    }

    #[test]
    fn folded_names_lowercase_unicode() {
        let listing = listing_of(&["ReadMe.TXT", "ŻÓŁW"]);
        let folded = listing.folded_names();
        assert_eq!(folded.get(0), "readme.txt");
        assert!(folded.is_ascii(0));
        assert_eq!(folded.get(1), "żółw");
        assert!(!folded.is_ascii(1));
    }

    #[test]
    fn folded_names_compose_decomposed_names() {
        // "Café" as HFS+ stores it: "e" followed by a combining acute accent.
        let listing = listing_of(&["Cafe\u{301}.txt"]);
        assert_eq!(listing.folded_names().get(0), "café.txt");
    }

    #[test]
    fn hidden_names_follow_the_platform() {
        assert!(is_hidden_name(".git"));
        assert_eq!(is_hidden_name("$Recycle.Bin"), cfg!(windows));
        assert_eq!(is_system_clutter(".DS_Store"), cfg!(target_os = "macos"));
        assert_eq!(is_system_clutter("._photo.jpg"), cfg!(target_os = "macos"));
        assert!(!is_system_clutter(".gitignore"));
    }
}
