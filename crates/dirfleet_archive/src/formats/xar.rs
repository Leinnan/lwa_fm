//! XAR archives, and the macOS installer packages built on them. A XAR is a
//! zlib-compressed XML table of contents followed by a heap of member data,
//! each member compressed on its own. A package's components hold their
//! files in a `Payload` (a cpio archive, compressed with pbzx or gzip) and
//! their install scripts in `Scripts`; both are expanded in place, so the
//! package shows what it installs.

use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::time::SystemTime;

use quick_xml::events::Event;

use super::composite::{Composite, Opener, Piece, Ranges};
use super::detect::Codec;
use super::{Backend, Listing, cpio, read_error, read_up_to};
use crate::archive::{Kind, Member};
use crate::error::{CoreError, Result};
use crate::names::{self, Encoding};
use crate::source::Source;
use crate::volumes::VolumeSet;

pub(crate) const MAGIC: &[u8] = b"xar!";

fn corrupt(message: impl Into<String>) -> CoreError {
    CoreError::Corrupt {
        message: message.into(),
    }
}

/// A `<file>` of the table of contents.
#[derive(Default)]
struct Record {
    name: String,
    kind: String,
    offset: u64,
    length: u64,
    size: u64,
    encoding: String,
    mode: Option<u32>,
    mtime: Option<SystemTime>,
    link: Option<String>,
    parent: Option<usize>,
}

fn read_toc(source: &Source) -> Result<(Vec<Record>, u64)> {
    let mut source = source.clone();
    let mut header = [0u8; 28];
    source
        .seek(SeekFrom::Start(0))
        .map_err(|e| read_error(e, false))?;
    source
        .read_exact(&mut header)
        .map_err(|e| read_error(e, false))?;
    if &header[..4] != MAGIC {
        return Err(corrupt("not a XAR archive"));
    }
    let header_size = u64::from(u16::from_be_bytes([header[4], header[5]]));
    let toc_compressed = u64::from_be_bytes(header[8..16].try_into().unwrap());
    let toc = source.slice(header_size, toc_compressed);
    let xml = BufReader::new(flate2::read::ZlibDecoder::new(toc));
    let records =
        parse_toc(xml).map_err(|e| corrupt(format!("damaged XAR table of contents: {e}")))?;
    Ok((records, header_size + toc_compressed))
}

fn parse_toc(xml: impl BufRead) -> std::result::Result<Vec<Record>, quick_xml::Error> {
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut records: Vec<Record> = Vec::new();
    // Open <file> elements, innermost last, and the element path inside the
    // innermost one.
    let mut files: Vec<usize> = Vec::new();
    let mut path: Vec<Vec<u8>> = Vec::new();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(element) => {
                let name = element.local_name().as_ref().to_vec();
                if name == b"file" {
                    records.push(Record {
                        parent: files.last().copied(),
                        ..Default::default()
                    });
                    files.push(records.len() - 1);
                    path.clear();
                } else {
                    path.push(name);
                }
            }
            Event::Empty(element) => {
                if element.local_name().as_ref() == b"encoding"
                    && let Some(&file) = files.last()
                {
                    for attribute in element.attributes().flatten() {
                        if attribute.key.local_name().as_ref() == b"style" {
                            records[file].encoding =
                                String::from_utf8_lossy(&attribute.value).into_owned();
                        }
                    }
                }
            }
            Event::Text(text) => {
                let Some(&file) = files.last() else { continue };
                let value = text.decode()?.into_owned();
                let record = &mut records[file];
                match path
                    .iter()
                    .map(Vec::as_slice)
                    .collect::<Vec<_>>()
                    .as_slice()
                {
                    [b"name"] => record.name = value,
                    [b"type"] => record.kind = value,
                    [b"mode"] => record.mode = u32::from_str_radix(value.trim(), 8).ok(),
                    [b"mtime"] => record.mtime = parse_time(value.trim()),
                    [b"link"] => record.link = Some(value),
                    [b"data", b"offset"] => record.offset = value.trim().parse().unwrap_or(0),
                    [b"data", b"length"] => record.length = value.trim().parse().unwrap_or(0),
                    [b"data", b"size"] => record.size = value.trim().parse().unwrap_or(0),
                    _ => {}
                }
            }
            Event::End(element) => {
                if element.local_name().as_ref() == b"file" {
                    files.pop();
                    path.clear();
                } else {
                    path.pop();
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(records)
}

/// `2017-03-08T19:56:42Z` as an instant.
fn parse_time(text: &str) -> Option<SystemTime> {
    let digits = |range: std::ops::Range<usize>| text.get(range)?.parse::<i32>().ok();
    crate::platform::utc_time(
        digits(0..4)?,
        digits(5..7)?,
        digits(8..10)?,
        digits(11..13)?,
        digits(14..16)?,
        digits(17..19)?,
    )
}

fn codec_of(encoding: &str) -> Result<Option<Codec>> {
    Ok(match encoding {
        "" | "application/octet-stream" => None,
        // Despite the name, XAR's "gzip" data is a zlib stream.
        "application/x-gzip" => Some(Codec::Zlib),
        "application/x-bzip2" => Some(Codec::Bzip2),
        "application/x-xz" => Some(Codec::Xz),
        "application/x-lzma" => Some(Codec::Lzma),
        other => {
            return Err(CoreError::Unsupported {
                message: format!("XAR data encoded as {other}"),
            });
        }
    })
}

/// Opens the XAR archive, or the installer package, in `volumes`.
pub(crate) fn open(volumes: VolumeSet, encoding: Encoding) -> Result<(Box<dyn Backend>, Listing)> {
    let source = Source::open(&volumes.paths)?;
    let (records, heap) = read_toc(&source)?;

    // Full paths, from each record's chain of parents.
    let paths: Vec<String> = (0..records.len())
        .map(|i| {
            let mut parts = vec![records[i].name.as_str()];
            let mut parent = records[i].parent;
            while let Some(p) = parent {
                parts.push(records[p].name.as_str());
                parent = records[p].parent;
            }
            parts.reverse();
            parts.join("/")
        })
        .collect();
    let package = records
        .iter()
        .any(|r| r.kind == "file" && r.name == "Payload");

    let mut members = Vec::new();
    let mut pieces = Vec::new();
    let mut payloads = Vec::new();
    for (record, path) in records.iter().zip(&paths) {
        let codec = codec_of(&record.encoding)?;
        let data = Piece::Data {
            offset: heap + record.offset,
            len: record.length,
            codec,
        };
        if package
            && record.kind == "file"
            && (record.name == "Payload" || record.name == "Scripts")
        {
            payloads.push((path.clone(), data));
            continue;
        }
        let name = names::decode(path.as_bytes(), encoding);
        let (kind, piece) = match record.kind.as_str() {
            "directory" => (Kind::Dir, Piece::None),
            "symlink" => (
                Kind::Link,
                Piece::Link(record.link.clone().unwrap_or_default().into_bytes()),
            ),
            "file" => (Kind::File, data),
            // Hard links and devices.
            _ => (Kind::Link, Piece::None),
        };
        members.push(Member {
            rel_path: names::safe_relative_path(&name, false),
            display: String::from_utf8_lossy(&name).into_owned(),
            entry_id: None,
            kind,
            size: record.size,
            packed_size: record.length,
            mtime: record.mtime,
            mode: record.mode,
            encrypted: false,
            hidden: false,
        });
        pieces.push(piece);
    }
    let ranges = Ranges {
        source: source.clone(),
        pieces,
    };
    let format = if package { "Installer package" } else { "XAR" };
    let listing = |members, solid| Listing {
        members,
        volumes: volumes.paths.clone(),
        format: format.to_string(),
        solid,
        comment: None,
        sizes_exact: true,
        packed_size: Some(source.len()),
    };
    if payloads.is_empty() {
        return Ok((Box::new(ranges), listing(members, false)));
    }

    let mut composite = Composite::default();
    composite.push(Box::new(ranges), members, "");
    for (path, piece) in payloads {
        let holder = Arc::new(Ranges {
            source: source.clone(),
            pieces: vec![piece],
        });
        let opener: Opener = Arc::new(move || payload_reader(holder.reader(0)?));
        let (payload, members) = cpio::open(opener, encoding)?;
        composite.push(Box::new(payload), members, &path);
    }
    let members = std::mem::take(&mut composite.members);
    Ok((Box::new(composite), listing(members, true)))
}

/// A package payload or scripts archive as cpio bytes. Current packages use
/// pbzx; older ones gzip, or nothing.
fn payload_reader(mut inner: Box<dyn Read + Send>) -> Result<Box<dyn Read + Send>> {
    let mut head = [0u8; 4];
    let n = read_up_to(&mut inner, &mut head).map_err(|e| read_error(e, false))?;
    let stream: Box<dyn Read + Send> = Box::new(io::Cursor::new(head[..n].to_vec()).chain(inner));
    Ok(match &head[..n] {
        b"pbzx" => Box::new(Pbzx::new(stream).map_err(|e| read_error(e, false))?),
        [0x1f, 0x8b, ..] => Box::new(flate2::read::MultiGzDecoder::new(stream)),
        _ => stream,
    })
}

/// pbzx, Apple's chunked xz: a header, then chunks that are each an xz
/// stream, or stored as is when xz didn't make them smaller.
struct Pbzx<R> {
    inner: R,
    chunk: Vec<u8>,
    pos: usize,
}

impl<R: Read> Pbzx<R> {
    fn new(mut inner: R) -> io::Result<Self> {
        let mut header = [0u8; 12];
        inner.read_exact(&mut header)?;
        Ok(Self {
            inner,
            chunk: Vec::new(),
            pos: 0,
        })
    }

    /// Decodes the next chunk; false at the end.
    fn fill(&mut self) -> io::Result<bool> {
        let mut sizes = [0u8; 16];
        if read_up_to(&mut self.inner, &mut sizes)? < 16 {
            return Ok(false);
        }
        let length = u64::from_be_bytes(sizes[8..16].try_into().unwrap());
        if length > 1 << 30 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "damaged pbzx chunk",
            ));
        }
        let mut data = vec![0u8; length as usize];
        self.inner.read_exact(&mut data)?;
        self.chunk = if data.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0]) {
            let mut decoded = Vec::new();
            lzma_rust2::XzReader::new(&data[..], true).read_to_end(&mut decoded)?;
            decoded
        } else {
            data
        };
        self.pos = 0;
        Ok(true)
    }
}

impl<R: Read> Read for Pbzx<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.pos == self.chunk.len() {
            if !self.fill()? {
                return Ok(0);
            }
        }
        let n = buf.len().min(self.chunk.len() - self.pos);
        buf[..n].copy_from_slice(&self.chunk[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}
