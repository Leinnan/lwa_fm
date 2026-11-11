//! RAR 1.3 through RAR 5/7, single files and volume sets, through rars.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::Path;

use rars::{
    ArchiveFamily, ArchiveMember, ArchiveReadOptions, ArchiveReader, AttrSource, ErrorKind,
    ExtractedEntryMeta, ExtractedLink, ExtractionDecision,
};

use super::{Backend, Job, Link, Listing, Sink};
use crate::archive::{Kind, Member};
use crate::error::{CoreError, Result};
use crate::names::{self, Encoding};
use crate::volumes::VolumeSet;

/// Members larger than this stream through sequential extraction.
const STREAM_ABOVE: u64 = 16 << 20;

/// Parallel decoding is used when wanted files average at least this size.
const PARALLEL_MIN_AVERAGE: u64 = 64 << 10;

pub(crate) struct Rar {
    archives: Vec<rars::Archive>,
    /// Raw member names in archive order. rars' callbacks identify members
    /// by name, so these map them back to indexes.
    names: Vec<Vec<u8>>,
    solid: bool,
}

pub(crate) fn open(
    path: &Path,
    password: Option<&[u8]>,
    encoding: Encoding,
) -> Result<(Rar, Listing)> {
    let volumes = VolumeSet::discover(path)?;
    let archives = volumes
        .paths
        .iter()
        .map(|volume| {
            ArchiveReader::read_path_with_options(
                volume,
                ArchiveReadOptions::with_optional_password(password),
            )
            .map_err(|e| match CoreError::from(e) {
                CoreError::Io { message, .. } => CoreError::Io {
                    path: volume.display().to_string(),
                    message,
                },
                other => other,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let archive_members = rars::volume_members(&archives)?;
    if archive_members
        .last()
        .is_some_and(|m| m.meta.is_split_after)
    {
        let name = match volumes.next_volume_name() {
            name if name.is_empty() => format!(
                "the part after {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ),
            name => name,
        };
        return Err(CoreError::MissingVolume { name });
    }

    let members = archive_members
        .iter()
        .map(|member| {
            let meta = &member.meta;
            let name = display_name(member, encoding);
            let mode =
                (meta.attr_source() == AttrSource::Unix).then_some((meta.file_attr & 0o777) as u32);
            Member {
                rel_path: names::safe_relative_path(&name, true),
                display: String::from_utf8_lossy(&name).into_owned(),
                entry_id: None,
                kind: if meta.is_directory {
                    Kind::Dir
                } else if is_link(member) {
                    Kind::Link
                } else {
                    Kind::File
                },
                size: meta.unpacked_size,
                packed_size: meta.packed_size,
                mtime: meta.modification_time(),
                mode,
                encrypted: meta.is_encrypted,
                hidden: false,
            }
        })
        .collect();

    let first = &archives[0];
    let format = match first.family() {
        ArchiveFamily::Rar13 => "RAR 1.3",
        ArchiveFamily::Rar15To40 => "RAR 1.5–4.x",
        ArchiveFamily::Rar50Plus => "RAR 5",
        _ => "RAR",
    };
    let comment = first
        .comment(password)
        .ok()
        .flatten()
        .map(|c| String::from_utf8_lossy(&c).trim().to_string())
        .filter(|c| !c.is_empty());
    let solid = archive_members.iter().any(rars::ArchiveMember::is_solid);
    let listing = Listing {
        members,
        volumes: volumes.paths,
        format: format.to_string(),
        solid,
        comment,
        sizes_exact: true,
        packed_size: None,
    };
    let names = archive_members
        .iter()
        .map(|m| m.meta.name.clone())
        .collect();
    Ok((
        Rar {
            archives,
            names,
            solid,
        },
        listing,
    ))
}

impl Rar {
    /// A single volume without wanted links can skip unwanted members and
    /// stop after the last wanted one. Links and multi-volume sets need the
    /// streaming API, which decodes everything.
    fn controlled(&self, members: &[Member], wanted: &[bool]) -> bool {
        self.archives.len() == 1 && !wants_links(members, wanted)
    }

    /// Independent members of a single volume decode in parallel, but the
    /// parallel API decodes every member and drops links, so it pays off
    /// only when several files and most of the archive are wanted, and no
    /// link is. Tiny files gain nothing: writing them is the bottleneck.
    fn parallel(&self, members: &[Member], wanted: &[bool]) -> bool {
        if self.archives.len() != 1 || self.solid || wants_links(members, wanted) {
            return false;
        }
        let (mut files, mut wanted_bytes, mut total) = (0, 0, 0);
        for (member, &is_wanted) in members.iter().zip(wanted) {
            total += member.size;
            if is_wanted && member.kind == Kind::File {
                files += 1;
                wanted_bytes += member.size;
            }
        }
        files > 1 && wanted_bytes * 2 >= total && wanted_bytes / files >= PARALLEL_MIN_AVERAGE
    }
}

fn wants_links(members: &[Member], wanted: &[bool]) -> bool {
    members
        .iter()
        .zip(wanted)
        .any(|(m, w)| *w && m.kind == Kind::Link)
}

impl Backend for Rar {
    fn planned_bytes(&self, members: &[Member], wanted: &[bool]) -> u64 {
        let controlled = self.controlled(members, wanted) && !self.parallel(members, wanted);
        let last_wanted = wanted.iter().rposition(|&w| w);
        (0..members.len())
            .filter(|&i| {
                members[i].kind != Kind::Dir
                    && (wanted[i]
                        || !controlled
                        || (self.solid && last_wanted.is_some_and(|last| i < last)))
            })
            .map(|i| members[i].size)
            .sum()
    }

    fn extract(&self, job: &Job<'_>, sink: &dyn Sink) -> Result<()> {
        let run = Matcher::new(self, job, sink);
        let options = ArchiveReadOptions::with_optional_password(job.password)
            .with_cancellation(&job.cancel.0);
        // Sequential decoding streams members above this size instead of
        // decoding each whole into memory first, so progress moves within
        // them and memory stays flat, at no measurable cost. The parallel
        // path keeps rars' default, which it also uses to decide whether
        // to fall back to sequential decoding.
        let sequential = options.with_rar50_buffered_decode_limit(STREAM_ABOVE);
        let outcome = if self.parallel(job.members, job.wanted) {
            self.archives[0]
                .extract_to_parallel_buffered_with_options(options, |meta: &ExtractedEntryMeta| {
                    run.open(meta)
                })
        } else if self.controlled(job.members, job.wanted) {
            let last_wanted = job.wanted.iter().rposition(|&w| w);
            self.archives[0]
                .extract_with_control(sequential, |member: &ArchiveMember| {
                    run.decide(member, last_wanted)
                })
                .map(drop)
        } else {
            rars::extract_volumes_with_links(
                &self.archives,
                sequential,
                |meta: &ExtractedEntryMeta| run.open(meta),
                |meta: &ExtractedEntryMeta, link: ExtractedLink<'_>| run.link(meta, link),
            )
        };
        outcome.map_err(|error| run.failure.take().unwrap_or_else(|| error.into()))
    }

    /// Non-solid single volumes are checked in parallel by rars' test API.
    /// Solid ones decode sequentially either way; extracting into counting
    /// sinks keeps their progress moving within large files.
    fn verify(&self, job: &Job<'_>, sink: &dyn Sink) -> Result<()> {
        if self.archives.len() != 1 || self.solid {
            return self.extract(job, sink);
        }
        let run = Matcher::new(self, job, sink);
        let options = ArchiveReadOptions::with_optional_password(job.password)
            .with_cancellation(&job.cancel.0);
        self.archives[0]
            .test_with_options_and_callback(options, |meta: &ExtractedEntryMeta| {
                if let Some(index) = run.take(&meta.name) {
                    sink.checked(index);
                }
                Ok(())
            })
            .map_err(|error| run.failure.take().unwrap_or_else(|| error.into()))
    }
}

/// Matches rars' callbacks to member indexes during one extraction.
struct Matcher<'a> {
    rar: &'a Rar,
    job: &'a Job<'a>,
    sink: &'a dyn Sink,
    /// Members by raw name, in archive order, so duplicate names resolve to
    /// successive members.
    by_name: RefCell<HashMap<&'a [u8], VecDeque<usize>>>,
    failure: RefCell<Option<CoreError>>,
}

impl<'a> Matcher<'a> {
    fn new(rar: &'a Rar, job: &'a Job<'a>, sink: &'a dyn Sink) -> Self {
        let mut by_name: HashMap<&[u8], VecDeque<usize>> = HashMap::new();
        for (index, name) in rar.names.iter().enumerate() {
            by_name.entry(name).or_default().push_back(index);
        }
        Self {
            rar,
            job,
            sink,
            by_name: RefCell::new(by_name),
            failure: RefCell::default(),
        }
    }

    fn take(&self, name: &[u8]) -> Option<usize> {
        self.by_name.borrow_mut().get_mut(name)?.pop_front()
    }

    /// Keeps the first sink error, which carries a path; rars only passes a
    /// message on.
    fn fail(&self, error: CoreError) -> rars::Error {
        let message = error.to_string();
        self.failure.borrow_mut().get_or_insert(error);
        rars::Error::from(io::Error::other(message))
    }

    fn write(&self, index: usize) -> rars::Result<Box<dyn io::Write>> {
        match self.sink.open(index) {
            Ok(writer) => Ok(writer),
            Err(error) => Err(self.fail(error)),
        }
    }

    /// Decision for `extract_with_control`, which can skip and stop.
    fn decide(
        &self,
        member: &ArchiveMember,
        last_wanted: Option<usize>,
    ) -> rars::Result<ExtractionDecision> {
        let index = self.take(&member.meta.name);
        // Nothing wanted comes after this member.
        if let Some(i) = index
            && last_wanted.is_none_or(|last| i > last)
        {
            return Ok(ExtractionDecision::Stop);
        }
        match index {
            Some(i) if self.job.wanted[i] => self.write(i).map(ExtractionDecision::Extract),
            // Directories and redirections carry no data and can always be skipped.
            _ if member.meta.is_directory || member.meta.is_redirection => {
                Ok(ExtractionDecision::Skip)
            }
            // Solid archives refuse skipping: later members need this one's history.
            _ if self.rar.solid => Ok(ExtractionDecision::Extract(self.sink.discard())),
            _ => Ok(ExtractionDecision::Skip),
        }
    }

    /// Writer for the streaming API, which decodes every member.
    fn open(&self, meta: &ExtractedEntryMeta) -> rars::Result<Box<dyn io::Write>> {
        match self.take(meta.name_bytes()) {
            Some(index) if self.job.wanted[index] => self.write(index),
            _ => Ok(self.sink.discard()),
        }
    }

    fn link(&self, meta: &ExtractedEntryMeta, link: ExtractedLink<'_>) -> rars::Result<()> {
        let Some(index) = self.take(meta.name_bytes()).filter(|&i| self.job.wanted[i]) else {
            return Ok(());
        };
        let link = match link {
            ExtractedLink::Symlink { target, .. } => Link::Symlink(target),
            ExtractedLink::HardLink { target } | ExtractedLink::FileCopy { target } => {
                Link::Copy(target)
            }
            _ => Link::Unsupported,
        };
        self.sink.link(index, link).map_err(|e| self.fail(e))
    }
}

/// RAR5 records links as redirections; older archives store a Unix symlink
/// mode with the target as the payload.
fn is_link(member: &ArchiveMember) -> bool {
    const S_IFMT: u64 = 0o170000;
    const S_IFLNK: u64 = 0o120000;
    let meta = &member.meta;
    meta.is_redirection
        || (meta.attr_source() == AttrSource::Unix && meta.file_attr & S_IFMT == S_IFLNK)
}

/// Name bytes to use on disk and on screen. Pre-Unicode archives store names
/// in a DOS or Windows code page; APFS only accepts UTF-8 names, so those are
/// decoded. Callbacks still match members by their raw bytes.
fn display_name(member: &ArchiveMember, encoding: Encoding) -> Vec<u8> {
    let raw = &member.meta.name;
    if member.name_is_unicode() || std::str::from_utf8(raw).is_ok() {
        return raw.clone();
    }
    member
        .decoded_name(Some(encoding))
        .map(|name| name.into_owned())
        .unwrap_or_else(|_| String::from_utf8_lossy(raw).into_owned().into_bytes())
}

/// Variants are chosen from `rars::ErrorKind`, so the UI can react to a
/// missing password without parsing message text.
impl From<rars::Error> for CoreError {
    fn from(error: rars::Error) -> Self {
        let message = error.to_string();
        match error.kind() {
            ErrorKind::PasswordRequired => Self::PasswordRequired,
            ErrorKind::BadPassword => Self::BadPassword,
            ErrorKind::Cancelled => Self::Cancelled,
            ErrorKind::SourceChanged => Self::ArchiveChanged,
            ErrorKind::UnsupportedFormat | ErrorKind::UnsupportedFeature => {
                Self::Unsupported { message }
            }
            ErrorKind::Io => Self::Io {
                path: String::new(),
                message,
            },
            _ => Self::Corrupt { message },
        }
    }
}
