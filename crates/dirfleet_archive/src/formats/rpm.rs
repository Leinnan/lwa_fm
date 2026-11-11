//! RPM packages: a lead, a signature header and a main header, followed by
//! the payload, a compressed cpio archive of the installed files. The
//! headers are read for the package's name; the payload is what is listed.

use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

use super::composite::Opener;
use super::detect::{self, Codec};
use super::{Listing, cpio, read_error, read_up_to};
use crate::codecs;
use crate::error::{CoreError, Result};
use crate::names::Encoding;
use crate::source::Source;
use crate::volumes::VolumeSet;

pub(crate) const MAGIC: [u8; 4] = [0xed, 0xab, 0xee, 0xdb];
const HEADER_MAGIC: [u8; 3] = [0x8e, 0xad, 0xe8];
const LEAD: u64 = 96;

const TAG_NAME: u32 = 1000;
const TAG_VERSION: u32 = 1001;
const TAG_RELEASE: u32 = 1002;
const TAG_SUMMARY: u32 = 1004;
const TAG_PAYLOAD_COMPRESSOR: u32 = 1125;

fn corrupt(message: &str) -> CoreError {
    CoreError::Corrupt {
        message: message.to_string(),
    }
}

/// A header structure: index entries of (tag, type, offset, count) into a
/// data store. Returns the string tags this module uses, and the header's
/// length.
fn header(source: &mut Source, at: u64) -> Result<(Vec<(u32, String)>, u64)> {
    let mut fixed = [0u8; 16];
    source
        .seek(SeekFrom::Start(at))
        .map_err(|e| read_error(e, false))?;
    source
        .read_exact(&mut fixed)
        .map_err(|e| read_error(e, false))?;
    if fixed[..3] != HEADER_MAGIC {
        return Err(corrupt("damaged RPM header"));
    }
    let entries = u32::from_be_bytes(fixed[8..12].try_into().unwrap()) as usize;
    let store = u32::from_be_bytes(fixed[12..16].try_into().unwrap()) as usize;
    if entries > 1 << 16 || store > 64 << 20 {
        return Err(corrupt("damaged RPM header"));
    }
    let mut index = vec![0u8; entries * 16];
    let mut data = vec![0u8; store];
    source
        .read_exact(&mut index)
        .map_err(|e| read_error(e, false))?;
    source
        .read_exact(&mut data)
        .map_err(|e| read_error(e, false))?;
    let mut strings = Vec::new();
    for entry in index.as_chunks::<16>().0 {
        let word = |i: usize| u32::from_be_bytes(entry[i * 4..i * 4 + 4].try_into().unwrap());
        let (tag, kind, offset) = (word(0), word(1), word(2) as usize);
        // Type 6 is a string, 9 an internationalized string table.
        if (kind == 6 || kind == 9) && offset < data.len() {
            let text = &data[offset..];
            let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
            strings.push((tag, String::from_utf8_lossy(&text[..end]).into_owned()));
        }
    }
    Ok((strings, 16 + (entries * 16 + store) as u64))
}

pub(crate) fn open(volumes: VolumeSet, encoding: Encoding) -> Result<(cpio::Cpio, Listing)> {
    let mut source = Source::open(&volumes.paths)?;
    let (_, signature) = header(&mut source, LEAD)?;
    // The signature header is padded to eight bytes.
    let main_at = LEAD + signature.div_ceil(8) * 8;
    let (tags, main) = header(&mut source, main_at)?;
    let tag = |wanted: u32| {
        tags.iter()
            .find(|(t, _)| *t == wanted)
            .map(|(_, v)| v.as_str())
    };
    let payload_at = main_at + main;
    let payload = source.slice(payload_at, source.len().saturating_sub(payload_at));

    let mut head = [0u8; 16];
    let n = read_up_to(&mut payload.clone(), &mut head).map_err(|e| read_error(e, false))?;
    let codec = match detect::detect(std::path::Path::new(""), &head[..n], &[]) {
        Some(detect::Format::Compressed(codec)) => Some(codec),
        // LZMA streams have no signature; the header names them.
        _ if tag(TAG_PAYLOAD_COMPRESSOR) == Some("lzma") => Some(Codec::Lzma),
        _ if cpio::is_cpio(&head[..n]) => None,
        _ => {
            return Err(CoreError::Unsupported {
                message: format!(
                    "RPM payload compressed with {}",
                    tag(TAG_PAYLOAD_COMPRESSOR).unwrap_or("an unknown method")
                ),
            });
        }
    };
    let opener: Opener = Arc::new(move || -> Result<Box<dyn Read + Send>> {
        match codec {
            Some(codec) => {
                codecs::decoder(codec, payload.clone()).map_err(|e| read_error(e, false))
            }
            None => Ok(Box::new(payload.clone())),
        }
    });
    let (payload, members) = cpio::open(opener, encoding)?;

    let name = [tag(TAG_NAME), tag(TAG_VERSION), tag(TAG_RELEASE)]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("-");
    let comment = match (name.is_empty(), tag(TAG_SUMMARY)) {
        (false, Some(summary)) => Some(format!("{name}: {summary}")),
        (false, None) => Some(name),
        (true, summary) => summary.map(str::to_string),
    };
    let listing = Listing {
        members,
        volumes: volumes.paths,
        format: "RPM package".to_string(),
        solid: true,
        comment,
        sizes_exact: true,
        packed_size: Some(source.len()),
    };
    Ok((payload, listing))
}
