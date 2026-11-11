//! Unix ar archives (`.a` static libraries, the outer layer of `.deb`), in
//! the System V/GNU and BSD variants, and Debian packages built on them.

use std::io::{Read, Seek, SeekFrom};
use std::time::{Duration, UNIX_EPOCH};

use super::composite::{Composite, Piece, Ranges};
use super::detect;
use super::{Listing, read_error, read_up_to, tar};
use crate::archive::{Kind, Member};
use crate::error::{CoreError, Result};
use crate::names::{self, Encoding};
use crate::source::Source;
use crate::volumes::VolumeSet;

pub(crate) const MAGIC: &[u8] = b"!<arch>\n";
const HEADER: u64 = 60;

/// One member: its name, where its data is, and its metadata.
struct Entry {
    name: Vec<u8>,
    offset: u64,
    size: u64,
    mtime: u64,
    mode: u32,
}

fn field(header: &[u8], range: std::ops::Range<usize>, radix: u32) -> u64 {
    let text = String::from_utf8_lossy(&header[range]);
    u64::from_str_radix(text.trim(), radix).unwrap_or(0)
}

fn corrupt(message: &str) -> CoreError {
    CoreError::Corrupt {
        message: message.to_string(),
    }
}

/// The members of the ar archive in `source`, without the symbol tables
/// and the GNU long-name table.
fn entries(source: &Source) -> Result<Vec<Entry>> {
    let mut source = source.clone();
    let len = source.len();
    let mut pos = MAGIC.len() as u64;
    let mut long_names = Vec::new();
    let mut entries = Vec::new();
    while pos + HEADER <= len {
        let mut header = [0u8; HEADER as usize];
        source
            .seek(SeekFrom::Start(pos))
            .map_err(|e| read_error(e, false))?;
        source
            .read_exact(&mut header)
            .map_err(|e| read_error(e, false))?;
        if &header[58..60] != b"`\n" {
            return Err(corrupt("damaged ar member header"));
        }
        let size = field(&header, 48..58, 10);
        let mut offset = pos + HEADER;
        let raw = String::from_utf8_lossy(&header[..16])
            .trim_end()
            .to_string();
        let mut data_size = size;
        let name: Vec<u8> = if let Some(len) = raw.strip_prefix("#1/") {
            // BSD: the name is the first bytes of the data.
            let len: u64 = len.parse().map_err(|_| corrupt("damaged ar member name"))?;
            let mut name = vec![0u8; len.min(size) as usize];
            source
                .read_exact(&mut name)
                .map_err(|e| read_error(e, false))?;
            offset += len;
            data_size = size.saturating_sub(len);
            name.split(|&b| b == 0).next().unwrap_or(&[]).to_vec()
        } else if raw == "//" {
            // GNU: the table of names too long for the header.
            long_names = vec![0u8; size as usize];
            read_up_to(&mut source, &mut long_names).map_err(|e| read_error(e, false))?;
            Vec::new()
        } else if let Some(index) = raw.strip_prefix('/').and_then(|i| i.parse::<usize>().ok()) {
            let rest = long_names.get(index..).unwrap_or(&[]);
            let end = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
            rest[..end]
                .strip_suffix(b"/")
                .unwrap_or(&rest[..end])
                .to_vec()
        } else {
            raw.strip_suffix('/').unwrap_or(&raw).as_bytes().to_vec()
        };
        // Symbol tables: "/" (System V), "__.SYMDEF" (BSD).
        let symbols = raw == "/" || raw == "/SYM64/" || name.starts_with(b"__.SYMDEF");
        if !name.is_empty() && !symbols {
            entries.push(Entry {
                name,
                offset,
                size: data_size,
                mtime: field(&header, 16..28, 10),
                mode: field(&header, 40..48, 8) as u32,
            });
        }
        // Members are padded to an even offset.
        pos += HEADER + size + size % 2;
    }
    Ok(entries)
}

/// A plain ar archive: each member is a file.
pub(crate) fn open(volumes: VolumeSet, encoding: Encoding) -> Result<(Ranges, Listing)> {
    let source = Source::open(&volumes.paths)?;
    let entries = entries(&source)?;
    let members = entries
        .iter()
        .map(|entry| {
            let name = names::decode(&entry.name, encoding);
            Member {
                rel_path: names::safe_relative_path(&name, false),
                display: String::from_utf8_lossy(&name).into_owned(),
                entry_id: None,
                kind: Kind::File,
                size: entry.size,
                packed_size: entry.size,
                mtime: Some(UNIX_EPOCH + Duration::from_secs(entry.mtime)),
                mode: Some(entry.mode & 0o7777).filter(|&m| m != 0),
                encrypted: false,
                hidden: false,
            }
        })
        .collect();
    let pieces = entries
        .iter()
        .map(|entry| Piece::Data {
            offset: entry.offset,
            len: entry.size,
            codec: None,
        })
        .collect();
    let listing = Listing {
        members,
        volumes: volumes.paths,
        format: "ar".to_string(),
        solid: false,
        comment: None,
        sizes_exact: true,
        packed_size: None,
    };
    Ok((Ranges { source, pieces }, listing))
}

/// Whether the ar archive in `source` is a Debian package.
pub(crate) fn is_deb(source: &Source) -> bool {
    entries(source).is_ok_and(|entries| {
        entries
            .first()
            .is_some_and(|entry| entry.name == b"debian-binary")
    })
}

/// A Debian package, shown as `dpkg-deb -R` unpacks it: the installed files,
/// and the control files under `DEBIAN`.
pub(crate) fn open_deb(volumes: VolumeSet, encoding: Encoding) -> Result<(Composite, Listing)> {
    let source = Source::open(&volumes.paths)?;
    let mut composite = Composite::default();
    let mut version = None;
    for entry in entries(&source)? {
        let name = String::from_utf8_lossy(&entry.name).into_owned();
        let part = source.slice(entry.offset, entry.size);
        if name == "debian-binary" {
            let mut text = String::new();
            part.clone().read_to_string(&mut text).ok();
            version = Some(text.trim().to_string());
            continue;
        }
        let prefix = match name.split('.').next() {
            Some("data") => "",
            Some("control") => "DEBIAN",
            _ => continue,
        };
        let mut head = [0u8; 16];
        let n = read_up_to(&mut part.clone(), &mut head).map_err(|e| read_error(e, false))?;
        let codec = match detect::detect(std::path::Path::new(&name), &head[..n], &[]) {
            Some(detect::Format::Compressed(codec)) => Some(codec),
            Some(detect::Format::Tar) | None => None,
            Some(_) => return Err(corrupt("unexpected member in a Debian package")),
        };
        let (tar, members) = tar::from_source(part, codec, encoding)?;
        composite.push(Box::new(tar), members, prefix);
    }
    let members = std::mem::take(&mut composite.members);
    let listing = Listing {
        members,
        volumes: volumes.paths,
        format: match version {
            Some(version) => format!("Debian package {version}"),
            None => "Debian package".to_string(),
        },
        solid: true,
        comment: None,
        sizes_exact: true,
        packed_size: Some(source.len()),
    };
    Ok((composite, listing))
}
