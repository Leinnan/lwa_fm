//! Building blocks for archives made of other archives, such as packages:
//! [`Composite`] joins backends under path prefixes, and [`Ranges`] serves
//! members that are plain byte ranges of the archive, maybe compressed.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use rayon::prelude::*;

use super::detect::Codec;
use super::{Attributes, Backend, Job, Link, Sink, copy, read_error};
use crate::archive::{Kind, Member};
use crate::codecs;
use crate::error::Result;
use crate::source::Source;

/// Opens a fresh reader of some part's bytes, for each pass over them.
pub(crate) type Opener = Arc<dyn Fn() -> Result<Box<dyn Read + Send>> + Send + Sync>;

struct Part {
    backend: Box<dyn Backend>,
    first: usize,
    count: usize,
}

/// Several backends shown as one archive, each part's members under a
/// prefix of its own.
#[derive(Default)]
pub(crate) struct Composite {
    parts: Vec<Part>,
    pub members: Vec<Member>,
}

impl Composite {
    /// Adds `backend`, whose members are `members`, under `prefix` (no
    /// prefix when empty).
    pub fn push(&mut self, backend: Box<dyn Backend>, members: Vec<Member>, prefix: &str) {
        self.parts.push(Part {
            backend,
            first: self.members.len(),
            count: members.len(),
        });
        self.members
            .extend(members.into_iter().map(|member| under(member, prefix)));
    }

    fn jobs<'a>(&'a self, job: &'a Job<'a>) -> impl Iterator<Item = (&'a Part, Job<'a>)> {
        self.parts.iter().filter_map(move |part| {
            let range = part.first..part.first + part.count;
            job.wanted[range.clone()].contains(&true).then(|| {
                (
                    part,
                    Job {
                        members: &job.members[range.clone()],
                        wanted: &job.wanted[range],
                        password: job.password,
                        cancel: job.cancel,
                    },
                )
            })
        })
    }
}

/// `member` as if it were stored under `prefix`.
fn under(mut member: Member, prefix: &str) -> Member {
    if prefix.is_empty() {
        return member;
    }
    member.display = format!("{prefix}/{}", member.display);
    member.rel_path = member.rel_path.map(|rel| PathBuf::from(prefix).join(rel));
    member
}

impl Backend for Composite {
    fn planned_bytes(&self, members: &[Member], wanted: &[bool]) -> u64 {
        self.parts
            .iter()
            .map(|part| {
                let range = part.first..part.first + part.count;
                part.backend
                    .planned_bytes(&members[range.clone()], &wanted[range])
            })
            .sum()
    }

    fn extract(&self, job: &Job<'_>, sink: &dyn Sink) -> Result<()> {
        for (part, sub) in self.jobs(job) {
            part.backend.extract(
                &sub,
                &Offset {
                    sink,
                    by: part.first,
                },
            )?;
        }
        Ok(())
    }

    fn verify(&self, job: &Job<'_>, sink: &dyn Sink) -> Result<()> {
        for (part, sub) in self.jobs(job) {
            part.backend.verify(
                &sub,
                &Offset {
                    sink,
                    by: part.first,
                },
            )?;
        }
        Ok(())
    }
}

/// A part's view of the archive's sink: its indexes start at `by`.
struct Offset<'a> {
    sink: &'a dyn Sink,
    by: usize,
}

impl Sink for Offset<'_> {
    fn open(&self, index: usize) -> Result<Box<dyn Write + Send>> {
        self.sink.open(index + self.by)
    }

    fn discard(&self) -> Box<dyn Write + Send> {
        self.sink.discard()
    }

    fn link(&self, index: usize, link: Link<'_>) -> Result<()> {
        self.sink.link(index + self.by, link)
    }

    fn checked(&self, index: usize) {
        self.sink.checked(index + self.by);
    }

    fn xattrs(&self, index: usize, attributes: Attributes) {
        self.sink.xattrs(index + self.by, attributes);
    }
}

/// Where a member of [`Ranges`] keeps its bytes.
pub(crate) enum Piece {
    /// `len` bytes at `offset`, compressed with `codec` if any.
    Data {
        offset: u64,
        len: u64,
        codec: Option<Codec>,
    },
    /// A symbolic link's target.
    Link(Vec<u8>),
    /// A folder, or anything else without data.
    None,
}

/// Members that are byte ranges of one source, as in ar and xar archives.
/// They are independent, so wanted ones decode in parallel.
pub(crate) struct Ranges {
    pub source: Source,
    pub pieces: Vec<Piece>,
}

impl Ranges {
    pub fn reader(&self, index: usize) -> Result<Box<dyn Read + Send>> {
        let Piece::Data { offset, len, codec } = self.pieces[index] else {
            return Ok(Box::new(std::io::empty()));
        };
        let slice = self.source.slice(offset, len);
        match codec {
            None => Ok(Box::new(slice)),
            Some(codec) => codecs::decoder(codec, slice).map_err(|e| read_error(e, false)),
        }
    }
}

impl Backend for Ranges {
    fn extract(&self, job: &Job<'_>, sink: &dyn Sink) -> Result<()> {
        let wanted: Vec<usize> = (0..self.pieces.len()).filter(|&i| job.wanted[i]).collect();
        wanted.par_iter().try_for_each(|&index| {
            job.check_cancelled()?;
            match (&self.pieces[index], job.members[index].kind) {
                (Piece::Link(target), _) => sink.link(index, Link::Symlink(target)),
                (_, Kind::Link) => sink.link(index, Link::Unsupported),
                (_, Kind::Dir) => sink.open(index).map(drop),
                _ => copy(&mut self.reader(index)?, &mut sink.open(index)?, job, false),
            }
        })
    }
}
