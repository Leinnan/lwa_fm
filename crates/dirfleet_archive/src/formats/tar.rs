//! tar archives (ustar, GNU, PAX), plain or inside a compressed stream such
//! as tar.gz or tar.xz. A compressed tar can only be read front to back, so
//! listing it decodes the whole stream once, and extraction decodes it again
//! up to the last wanted member.

use std::io::{Read, Seek, SeekFrom};
use std::time::{Duration, UNIX_EPOCH};

use tar::EntryType;

use super::detect::Codec;
use super::{Backend, Job, Link, Listing, Sink, copy, read_error};
use crate::archive::{Kind, Member};
use crate::codecs;
use crate::error::Result;
use crate::names::{self, Encoding};
use crate::source::Source;
use crate::volumes::VolumeSet;

pub(crate) struct Tar {
    source: Source,
    codec: Option<Codec>,
}

pub(crate) fn open(
    volumes: VolumeSet,
    codec: Option<Codec>,
    encoding: Encoding,
) -> Result<(Tar, Listing)> {
    let source = Source::open(&volumes.paths)?;
    let packed = source.len();
    let (tar, members) = from_source(source, codec, encoding)?;
    let listing = Listing {
        members,
        volumes: volumes.paths,
        format: match codec {
            None => "tar".to_string(),
            Some(codec) => format!("tar ({})", codec.name()),
        },
        solid: codec.is_some(),
        comment: None,
        sizes_exact: true,
        packed_size: Some(packed),
    };
    Ok((tar, listing))
}

/// The tar archive in `source`, compressed with `codec` if any, and its
/// members.
pub(crate) fn from_source(
    source: Source,
    codec: Option<Codec>,
    encoding: Encoding,
) -> Result<(Tar, Vec<Member>)> {
    let tar = Tar { source, codec };
    let mut members = Vec::new();
    match codec {
        // A plain tar can seek past each member's data instead of reading it.
        None => {
            let mut source = tar.source.clone();
            source
                .seek(SeekFrom::Start(0))
                .map_err(|e| read_error(e, false))?;
            let mut archive = tar::Archive::new(source);
            list(archive.entries_with_seek(), encoding, &mut members)?;
        }
        Some(codec) => {
            let mut archive = tar::Archive::new(tar.reader(codec)?);
            list(archive.entries(), encoding, &mut members)?;
        }
    }
    Ok((tar, members))
}

impl Tar {
    fn reader(&self, codec: Codec) -> Result<Box<dyn Read + Send>> {
        codecs::decoder(codec, self.source.clone()).map_err(|e| read_error(e, false))
    }
}

/// Headers that describe the next entry (long names, PAX records) and are
/// not members themselves. tar folds most of them in; these may remain.
fn is_meta(kind: EntryType) -> bool {
    matches!(
        kind,
        EntryType::XGlobalHeader
            | EntryType::XHeader
            | EntryType::GNULongName
            | EntryType::GNULongLink
    )
}

fn list<R: Read>(
    entries: std::io::Result<tar::Entries<'_, R>>,
    encoding: Encoding,
    members: &mut Vec<Member>,
) -> Result<()> {
    for entry in entries.map_err(|e| read_error(e, false))? {
        let entry = entry.map_err(|e| read_error(e, false))?;
        let header = entry.header();
        let kind = header.entry_type();
        if is_meta(kind) {
            continue;
        }
        let name = names::decode(&entry.path_bytes(), encoding);
        members.push(Member {
            rel_path: names::safe_relative_path(&name, false),
            display: String::from_utf8_lossy(&name).into_owned(),
            entry_id: None,
            kind: match kind {
                EntryType::Directory => Kind::Dir,
                EntryType::Regular | EntryType::Continuous | EntryType::GNUSparse => Kind::File,
                // Links, and devices and pipes, which are reported as skipped.
                _ => Kind::Link,
            },
            size: entry.size(),
            packed_size: header.entry_size().unwrap_or(0),
            mtime: header
                .mtime()
                .ok()
                .map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds)),
            mode: header.mode().ok().map(|m| m & 0o777),
            encrypted: false,
            hidden: false,
        });
    }
    Ok(())
}

impl Backend for Tar {
    fn extract(&self, job: &Job<'_>, sink: &dyn Sink) -> Result<()> {
        match self.codec {
            None => {
                let mut source = self.source.clone();
                source
                    .seek(SeekFrom::Start(0))
                    .map_err(|e| read_error(e, false))?;
                let mut archive = tar::Archive::new(source);
                extract_entries(archive.entries_with_seek(), job, sink)
            }
            Some(codec) => {
                let reader = codecs::ReadAhead::new(self.reader(codec)?);
                let mut archive = tar::Archive::new(reader);
                extract_entries(archive.entries(), job, sink)
            }
        }
    }
}

fn extract_entries<R: Read>(
    entries: std::io::Result<tar::Entries<'_, R>>,
    job: &Job<'_>,
    sink: &dyn Sink,
) -> Result<()> {
    let Some(last) = job.last_wanted() else {
        return Ok(());
    };
    let mut index = 0;
    for entry in entries.map_err(|e| read_error(e, false))? {
        let mut entry = entry.map_err(|e| read_error(e, false))?;
        let kind = entry.header().entry_type();
        if is_meta(kind) {
            continue;
        }
        let current = index;
        index += 1;
        if current > last {
            break;
        }
        if !job.wanted[current] {
            continue;
        }
        job.check_cancelled()?;
        match job.members[current].kind {
            Kind::Dir => drop(sink.open(current)?),
            Kind::File => copy(&mut entry, &mut sink.open(current)?, job, false)?,
            Kind::Link => {
                let target = entry.link_name_bytes().map(|t| t.into_owned());
                let link = match (kind, &target) {
                    (EntryType::Symlink, Some(target)) => Link::Symlink(target),
                    (EntryType::Link, Some(target)) => Link::Copy(target),
                    _ => Link::Unsupported,
                };
                sink.link(current, link)?;
            }
        }
    }
    Ok(())
}
