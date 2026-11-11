//! Compressed streams: a tar or cpio archive inside (`.tar.gz`, `.tgz`,
//! `.cpio.gz`), or a single compressed file (`notes.txt.gz`), shown as an
//! archive of one file.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::composite::Opener;
use super::detect::{self, Codec};
use super::{Backend, Job, Listing, Sink, boxed, copy, cpio, read_error, read_up_to, tar};
use crate::archive::{Kind, Member};
use crate::codecs;
use crate::error::Result;
use crate::names::{self, Encoding};
use crate::source::Source;
use crate::volumes::VolumeSet;

pub(crate) struct Stream {
    source: Source,
    codec: Codec,
}

pub(crate) fn open(
    volumes: VolumeSet,
    codec: Codec,
    encoding: Encoding,
) -> Result<(Box<dyn Backend>, Listing)> {
    let source = Source::open(&volumes.paths)?;
    let mut head = [0u8; 512];
    let mut decoder = codecs::decoder(codec, source.clone()).map_err(|e| read_error(e, false))?;
    let n = read_up_to(&mut decoder, &mut head).map_err(|e| read_error(e, false))?;
    if detect::is_tar(&head[..n]) {
        return Ok(boxed(tar::open(volumes, Some(codec), encoding)?));
    }
    if cpio::is_cpio(&head[..n]) {
        let payload = source.clone();
        let opener: Opener = Arc::new(move || {
            codecs::decoder(codec, payload.clone()).map_err(|e| read_error(e, false))
        });
        let (cpio, members) = cpio::open(opener, encoding)?;
        let listing = Listing {
            members,
            volumes: volumes.paths,
            format: format!("cpio ({})", codec.name()),
            solid: true,
            comment: None,
            sizes_exact: true,
            packed_size: Some(source.len()),
        };
        return Ok((Box::new(cpio), listing));
    }

    let first = &volumes.paths[0];
    let gzip = (codec == Codec::Gzip)
        .then(|| gzip_header(&source))
        .flatten();
    let name = gzip
        .as_ref()
        .and_then(|header| header.name.clone())
        .unwrap_or_else(|| stem(first, codec).into_bytes());
    let name = names::decode(&name, encoding);
    let (size, sizes_exact) = match codecs::uncompressed_size(codec, &source) {
        Some(size) => (size, true),
        None if codec == Codec::Gzip => (codecs::gzip_size_hint(&source).unwrap_or(0), false),
        None => (0, false),
    };
    let mtime = gzip
        .and_then(|header| header.mtime)
        .or_else(|| first.metadata().and_then(|m| m.modified()).ok());
    let member = Member {
        rel_path: names::safe_relative_path(&name, false),
        display: String::from_utf8_lossy(&name).into_owned(),
        entry_id: None,
        kind: Kind::File,
        size,
        packed_size: source.len(),
        mtime,
        mode: None,
        encrypted: false,
        hidden: false,
    };
    let listing = Listing {
        members: vec![member],
        volumes: volumes.paths,
        format: codec.name().to_string(),
        solid: false,
        comment: None,
        sizes_exact,
        packed_size: Some(source.len()),
    };
    Ok((Box::new(Stream { source, codec }), listing))
}

impl Backend for Stream {
    fn extract(&self, job: &Job<'_>, sink: &dyn Sink) -> Result<()> {
        if !job.wanted[0] {
            return Ok(());
        }
        let decoder =
            codecs::decoder(self.codec, self.source.clone()).map_err(|e| read_error(e, false))?;
        copy(
            &mut codecs::ReadAhead::new(decoder),
            &mut sink.open(0)?,
            job,
            false,
        )
    }
}

/// The file name without the compression extension: `notes.txt.gz` →
/// `notes.txt`. A name that is only an extension becomes "Untitled".
fn stem(path: &Path, codec: Codec) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stripped = codec.extensions().iter().find_map(|ext| {
        let suffix = format!(".{ext}");
        let cut = name.len().checked_sub(suffix.len())?;
        (cut > 0 && name[cut..].eq_ignore_ascii_case(&suffix)).then(|| name[..cut].to_string())
    });
    match stripped {
        Some(stem) => stem,
        None if name.is_empty() => "Untitled".to_string(),
        None => name,
    }
}

struct GzipHeader {
    name: Option<Vec<u8>>,
    mtime: Option<SystemTime>,
}

/// The original name and modification time a gzip header may record.
fn gzip_header(source: &Source) -> Option<GzipHeader> {
    const FEXTRA: u8 = 4;
    const FNAME: u8 = 8;
    let mut source = source.clone();
    source.seek(SeekFrom::Start(0)).ok()?;
    let mut fixed = [0u8; 10];
    source.read_exact(&mut fixed).ok()?;
    let flags = fixed[3];
    let seconds = u32::from_le_bytes(fixed[4..8].try_into().ok()?);
    if flags & FEXTRA != 0 {
        let mut len = [0u8; 2];
        source.read_exact(&mut len).ok()?;
        source
            .seek(SeekFrom::Current(i64::from(u16::from_le_bytes(len))))
            .ok()?;
    }
    let name = if flags & FNAME != 0 {
        let mut name = Vec::new();
        let mut byte = [0u8; 1];
        while source.read_exact(&mut byte).is_ok() && byte[0] != 0 && name.len() < 4096 {
            name.push(byte[0]);
        }
        // Only the last component: the header is not a path to trust.
        let base = name
            .rsplit(|&b| b == b'/' || b == b'\\')
            .next()
            .unwrap_or(&[]);
        (!base.is_empty()).then(|| base.to_vec())
    } else {
        None
    };
    Some(GzipHeader {
        name,
        mtime: (seconds != 0).then(|| UNIX_EPOCH + Duration::from_secs(u64::from(seconds))),
    })
}
