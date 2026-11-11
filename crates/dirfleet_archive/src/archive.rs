use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{CoreError, Result};
use crate::formats::{self, Backend};
use crate::names::{self, Encoding};
use crate::volumes::archive_stem;

/// One row of the contents tree. Parents always precede their children, and
/// folders that the archive only implies (`a/b.txt` without an `a/` member)
/// are synthesized so the tree is complete.
#[derive(Debug, Clone)]
pub struct Entry {
    pub id: u32,
    pub parent_id: Option<u32>,
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    /// Unpacked size; for folders, the total of everything inside.
    pub size: u64,
    pub packed_size: u64,
    /// Modification time in Unix seconds.
    pub modified: Option<i64>,
    pub encrypted: bool,
    pub is_link: bool,
}

#[derive(Debug, Clone)]
pub struct ArchiveInfo {
    /// Path of the first volume.
    pub path: String,
    /// Archive name without volume and archive suffixes.
    pub name: String,
    pub format: String,
    pub volumes: Vec<String>,
    pub is_solid: bool,
    pub has_encrypted_entries: bool,
    pub file_count: u64,
    pub total_size: u64,
    pub packed_size: u64,
    pub comment: Option<String>,
    /// Whether `total_size` is exact. Some compressed files record only an
    /// estimate of their size, or none (then it is 0).
    pub sizes_exact: bool,
    /// Members whose names would escape the destination; they are never extracted.
    pub unsafe_names: Vec<String>,
}

/// A physical archive member, in archive order.
pub(crate) struct Member {
    /// Destination-relative path, or `None` when the name is unsafe.
    pub rel_path: Option<PathBuf>,
    /// The name as stored, decoded for display.
    pub display: String,
    pub entry_id: Option<u32>,
    pub kind: Kind,
    pub size: u64,
    pub packed_size: u64,
    pub mtime: Option<SystemTime>,
    /// Unix permission bits, when the archive was made on a Unix host.
    pub mode: Option<u32>,
    pub encrypted: bool,
    /// Bookkeeping the backend handles itself, such as AppleDouble files:
    /// neither listed nor extracted.
    pub hidden: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    File,
    Dir,
    /// A symbolic link, hard link or file copy, recreated through `Sink::link`.
    Link,
}

pub struct ArchiveHandle {
    pub(crate) backend: Box<dyn Backend>,
    pub(crate) members: Vec<Member>,
    pub(crate) entries: Vec<Entry>,
    pub(crate) password: Option<String>,
    info: ArchiveInfo,
}

/// Opens an archive, or the whole volume set that `path` belongs to.
///
/// `legacy_encoding` names the code page of pre-Unicode file names
/// (`cp437`, `cp850`, `cp852`, `cp866`, `windows-1251`, `windows-1252`);
/// it defaults to `cp437`, the DOS code page WinRAR used for English.
pub fn open_archive(
    path: String,
    password: Option<String>,
    legacy_encoding: Option<String>,
) -> Result<Arc<ArchiveHandle>> {
    let encoding = match legacy_encoding.as_deref() {
        None | Some("") => Encoding::Cp437,
        Some(name) => name.parse().map_err(|_| CoreError::InvalidInput {
            message: format!("Unknown filename encoding: {name}"),
        })?,
    };
    crate::with_large_stack(|| ArchiveHandle::open(Path::new(&path), password, encoding))
        .map(Arc::new)
}

impl ArchiveHandle {
    pub fn info(&self) -> ArchiveInfo {
        self.info.clone()
    }

    pub fn entries(&self) -> Vec<Entry> {
        self.entries.clone()
    }
}

impl ArchiveHandle {
    pub(crate) fn open(path: &Path, password: Option<String>, encoding: Encoding) -> Result<Self> {
        let password = password.filter(|p| !p.is_empty());
        let (backend, listing) =
            formats::open(path, password.as_deref().map(str::as_bytes), encoding)?;
        let mut members = listing.members;

        let mut tree = TreeBuilder::default();
        for member in members.iter_mut().filter(|m| !m.hidden) {
            member.entry_id = match &member.rel_path {
                Some(rel) => tree.add(rel, member),
                None => {
                    tree.unsafe_names.push(member.display.clone());
                    None
                }
            };
        }
        let unsafe_names = std::mem::take(&mut tree.unsafe_names);
        let entries = tree.finish();

        let files = members.iter().filter(|m| m.kind != Kind::Dir && !m.hidden);
        let info = ArchiveInfo {
            path: listing.volumes[0].display().to_string(),
            name: archive_stem(&listing.volumes[0]),
            format: listing.format,
            volumes: listing
                .volumes
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            is_solid: listing.solid,
            has_encrypted_entries: members.iter().any(|m| m.encrypted),
            file_count: files.clone().count() as u64,
            total_size: files.clone().map(|m| m.size).sum(),
            packed_size: listing
                .packed_size
                .unwrap_or_else(|| files.map(|m| m.packed_size).sum()),
            comment: listing.comment,
            sizes_exact: listing.sizes_exact,
            unsafe_names,
        };

        Ok(Self {
            backend,
            members,
            entries,
            password,
            info,
        })
    }

    /// Whether `id` is one of `selected` or lies inside one of them.
    pub(crate) fn is_within(&self, mut id: u32, selected: &std::collections::HashSet<u32>) -> bool {
        loop {
            if selected.contains(&id) {
                return true;
            }
            match self.entries.get(id as usize).and_then(|e| e.parent_id) {
                Some(parent) => id = parent,
                None => return false,
            }
        }
    }

    /// For each member, whether a request for `selection` (all when `None`) extracts it.
    pub(crate) fn wanted_members(&self, selection: Option<&[u32]>) -> Vec<bool> {
        let selected = selection.map(|ids| ids.iter().copied().collect());
        self.members
            .iter()
            .map(|m| match (m.entry_id, &selected) {
                (None, _) => false,
                (Some(_), None) => true,
                (Some(id), Some(selected)) => self.is_within(id, selected),
            })
            .collect()
    }
}

#[derive(Default)]
struct TreeBuilder {
    entries: Vec<Entry>,
    folders: HashMap<PathBuf, u32>,
    unsafe_names: Vec<String>,
}

impl TreeBuilder {
    fn add(&mut self, rel: &Path, member: &Member) -> Option<u32> {
        let parts: Vec<_> = rel
            .components()
            .filter_map(|c| match c {
                Component::Normal(part) => Some(part),
                _ => None,
            })
            .collect();
        let (last, parents) = parts.split_last()?;

        let mut parent_id = None;
        let mut path = PathBuf::new();
        for part in parents {
            path.push(part);
            parent_id = Some(self.folder(&path, parent_id));
        }
        path.push(last);

        let modified = member
            .mtime
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        if member.kind == Kind::Dir {
            let id = self.folder(&path, parent_id);
            self.entries[id as usize].modified = modified;
            return Some(id);
        }
        let id = self.entries.len() as u32;
        self.entries.push(Entry {
            id,
            parent_id,
            name: last.to_string_lossy().into_owned(),
            path: names::display_rel(&path),
            is_dir: false,
            size: member.size,
            packed_size: member.packed_size,
            modified,
            encrypted: member.encrypted,
            is_link: member.kind == Kind::Link,
        });
        Some(id)
    }

    fn folder(&mut self, path: &Path, parent_id: Option<u32>) -> u32 {
        if let Some(&id) = self.folders.get(path) {
            return id;
        }
        let id = self.entries.len() as u32;
        self.entries.push(Entry {
            id,
            parent_id,
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            path: names::display_rel(path),
            is_dir: true,
            size: 0,
            packed_size: 0,
            modified: None,
            encrypted: false,
            is_link: false,
        });
        self.folders.insert(path.to_path_buf(), id);
        id
    }

    /// Rolls sizes up into folders. Children always come after their parent,
    /// so a reverse pass sees every subtree complete before its parent.
    fn finish(mut self) -> Vec<Entry> {
        for i in (0..self.entries.len()).rev() {
            let (size, packed, encrypted) = {
                let e = &self.entries[i];
                (e.size, e.packed_size, e.encrypted)
            };
            if let Some(parent) = self.entries[i].parent_id {
                let parent = &mut self.entries[parent as usize];
                parent.size += size;
                parent.packed_size += packed;
                parent.encrypted |= encrypted;
            }
        }
        self.entries
    }
}
