//! Archive formats behind one interface. A backend lists its members when it
//! is opened and decodes them on request into a [`Sink`], which owns all that
//! is format-independent: staging paths, collisions, links and progress.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::archive::{Kind, Member};
use crate::error::{CoreError, Result};
use crate::extract::CancelToken;
use crate::names::Encoding;
use crate::volumes::VolumeSet;

mod appledouble;
pub(crate) use appledouble::Attributes;
mod ar;
mod cab;
mod composite;
pub(crate) mod cpio;
pub(crate) mod detect;
// Disk images are read with hdiutil and the fallback is the system's
// libarchive; Windows has neither.
#[cfg(not(windows))]
mod image;
#[cfg(all(feature = "libarchive", not(windows)))]
mod libarchive;
pub(crate) mod lzw;
mod rar;
mod rpm;
mod sevenz;
mod stream;
mod tar;
mod xar;
mod zip;

/// What a backend reports when it opens an archive.
pub(crate) struct Listing {
    /// Physical members in archive order.
    pub members: Vec<Member>,
    /// Every file the archive spans, first volume first.
    pub volumes: Vec<PathBuf>,
    /// Shown to the user, e.g. "RAR 5".
    pub format: String,
    pub solid: bool,
    pub comment: Option<String>,
    /// Whether member sizes are known exactly; a compressed stream may only
    /// record an estimate, or nothing.
    pub sizes_exact: bool,
    /// The archive's size on disk, when members don't record their share.
    pub packed_size: Option<u64>,
}

pub(crate) trait Backend: Send + Sync {
    /// Unpacked bytes that decoding `wanted` produces, counting members that
    /// are decoded only to reach later ones. This is the progress total.
    fn planned_bytes(&self, members: &[Member], wanted: &[bool]) -> u64 {
        members
            .iter()
            .zip(wanted)
            .filter(|(m, wanted)| **wanted && m.kind != Kind::Dir)
            .map(|(m, _)| m.size)
            .sum()
    }

    /// Decodes the wanted members into `sink`. Errors raised by the sink are
    /// returned as they are.
    fn extract(&self, job: &Job<'_>, sink: &dyn Sink) -> Result<()>;

    /// Decodes and checks every member, keeping nothing. A backend may skip
    /// the writers and report each member to [`Sink::checked`] instead.
    fn verify(&self, job: &Job<'_>, sink: &dyn Sink) -> Result<()> {
        self.extract(job, sink)
    }
}

pub(crate) struct Job<'a> {
    pub members: &'a [Member],
    pub wanted: &'a [bool],
    pub password: Option<&'a [u8]>,
    pub cancel: &'a CancelToken,
}

impl Job<'_> {
    pub fn check_cancelled(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            Err(CoreError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Index of the last wanted member.
    pub fn last_wanted(&self) -> Option<usize> {
        self.wanted.iter().rposition(|&w| w)
    }
}

/// Where a backend puts decoded members.
pub(crate) trait Sink: Sync {
    /// Destination for wanted member `index`: its staged file, or a writer
    /// that discards the (empty) payload of a folder.
    fn open(&self, index: usize) -> Result<Box<dyn Write + Send>>;

    /// Destination for bytes that must be decoded but are not kept. They
    /// still count as progress.
    fn discard(&self) -> Box<dyn Write + Send>;

    /// Recreates wanted link member `index`.
    fn link(&self, index: usize, link: Link<'_>) -> Result<()>;

    /// Member `index` was checked without passing its bytes to a writer.
    fn checked(&self, _index: usize) {}

    /// Extended attributes for wanted member `index`, set once it is written.
    fn xattrs(&self, _index: usize, _attributes: appledouble::Attributes) {}
}

pub(crate) enum Link<'a> {
    /// A symbolic link to `target`, as stored.
    Symlink(&'a [u8]),
    /// A copy of another member, named by its path in the archive.
    Copy(&'a [u8]),
    /// A kind of link, or a device or pipe, that can't be recreated.
    Unsupported,
}

/// Opens the archive at `path`, or the volume set it belongs to.
pub(crate) fn open(
    path: &Path,
    password: Option<&[u8]>,
    encoding: Encoding,
) -> Result<(Box<dyn Backend>, Listing)> {
    let volumes = VolumeSet::discover(path)?;
    let first = &volumes.paths[0];
    let (head, tail) = head_and_tail(first).map_err(|e| CoreError::io(first, e))?;
    let native = match detect::detect(first, &head, &tail) {
        Some(format) => open_native(format, path, volumes.clone(), password, encoding),
        None => Err(CoreError::Unsupported {
            message: "this isn't an archive uunpack can read".to_string(),
        }),
    };
    // libarchive reads some formats and variants the native backends don't.
    #[cfg(all(feature = "libarchive", not(windows)))]
    if matches!(native, Err(CoreError::Unsupported { .. }))
        && let Ok(opened) = libarchive::open(volumes, password, encoding)
    {
        return Ok(boxed(opened));
    }
    native
}

fn open_native(
    format: detect::Format,
    path: &Path,
    volumes: VolumeSet,
    password: Option<&[u8]>,
    encoding: Encoding,
) -> Result<(Box<dyn Backend>, Listing)> {
    Ok(match format {
        detect::Format::Rar => boxed(rar::open(path, password, encoding)?),
        detect::Format::Zip => boxed(zip::open(volumes, encoding)?),
        detect::Format::SevenZip => boxed(sevenz::open(volumes, password)?),
        detect::Format::Tar => boxed(tar::open(volumes, None, encoding)?),
        detect::Format::Compressed(codec) => stream::open(volumes, codec, encoding)?,
        detect::Format::Cpio => boxed(cpio::open_file(volumes, encoding)?),
        detect::Format::Ar if ar::is_deb(&crate::source::Source::open(&volumes.paths)?) => {
            boxed(ar::open_deb(volumes, encoding)?)
        }
        detect::Format::Ar => boxed(ar::open(volumes, encoding)?),
        detect::Format::Rpm => boxed(rpm::open(volumes, encoding)?),
        detect::Format::Xar => xar::open(volumes, encoding)?,
        detect::Format::Cab => boxed(cab::open(volumes)?),
        #[cfg(not(windows))]
        detect::Format::Image => boxed(image::open(volumes, password)?),
        #[cfg(windows)]
        detect::Format::Image => {
            return Err(CoreError::Unsupported {
                message: "disk images can only be opened on macOS".to_string(),
            });
        }
    })
}

fn boxed<B: Backend + 'static>((backend, listing): (B, Listing)) -> (Box<dyn Backend>, Listing) {
    (Box::new(backend), listing)
}

/// The first [`detect::HEAD`] and last [`detect::TAIL`] bytes of `path`.
fn head_and_tail(path: &Path) -> io::Result<(Vec<u8>, Vec<u8>)> {
    use std::io::{Seek, SeekFrom};
    let mut file = File::open(path)?;
    let mut head = vec![0u8; detect::HEAD];
    let n = read_up_to(&mut file, &mut head)?;
    head.truncate(n);
    let len = file.metadata()?.len();
    let mut tail = Vec::new();
    if len >= detect::TAIL as u64 {
        file.seek(SeekFrom::Start(len - detect::TAIL as u64))?;
        tail = vec![0u8; detect::TAIL];
        file.read_exact(&mut tail)?;
    }
    Ok((head, tail))
}

pub(crate) use crate::platform::local_time;

/// Reads until `buf` is full or the input ends.
pub(crate) fn read_up_to(reader: &mut dyn Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Copies a member's bytes into its writer, checking for cancellation
/// between chunks. Read errors mean damaged data, or the wrong password for
/// an encrypted member; write errors concern the destination.
pub(crate) fn copy(
    reader: &mut dyn Read,
    writer: &mut dyn Write,
    job: &Job<'_>,
    encrypted: bool,
) -> Result<()> {
    let mut buf = [0u8; 64 << 10];
    loop {
        job.check_cancelled()?;
        let n = match reader.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(read_error(e, encrypted)),
        };
        writer.write_all(&buf[..n]).map_err(|e| CoreError::Io {
            path: String::new(),
            message: e.to_string(),
        })?;
    }
}

/// A decoder's I/O error, which concerns the archive.
pub(crate) fn read_error(error: io::Error, encrypted: bool) -> CoreError {
    match error.kind() {
        // A wrong key decodes to garbage that fails the checksum.
        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof if encrypted => {
            CoreError::BadPassword
        }
        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof | io::ErrorKind::Other => {
            CoreError::Corrupt {
                message: error.to_string(),
            }
        }
        _ => CoreError::Io {
            path: String::new(),
            message: error.to_string(),
        },
    }
}

/// Reads a link member's target, which is its whole payload.
pub(crate) fn read_target(reader: &mut dyn Read, encrypted: bool) -> Result<Vec<u8>> {
    let mut target = Vec::new();
    reader
        .take(64 << 10)
        .read_to_end(&mut target)
        .map_err(|e| read_error(e, encrypted))?;
    Ok(target)
}
