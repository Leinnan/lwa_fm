//! Decoders for compressed streams, shared by tar archives and single
//! compressed files.

use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::sync::mpsc::{Receiver, sync_channel};

use crate::formats::detect::Codec;
use crate::formats::lzw::LzwReader;
use crate::formats::read_up_to;
use crate::source::Source;

/// A reader of the decompressed bytes of `source`, from its start.
pub(crate) fn decoder(codec: Codec, mut source: Source) -> io::Result<Box<dyn Read + Send>> {
    source.seek(SeekFrom::Start(0))?;
    Ok(match codec {
        Codec::Gzip => Box::new(flate2::read::MultiGzDecoder::new(source)),
        Codec::Bzip2 => Box::new(bzip2::read::MultiBzDecoder::new(source)),
        // Files written with several blocks (`xz -T0`) decode in parallel.
        Codec::Xz => Box::new(lzma_rust2::XzReaderMt::new(source, true, threads())?),
        Codec::Lzma => Box::new(
            lzma_rust2::LzmaReader::new_mem_limit(BufReader::new(source), u32::MAX, None)
                .map_err(io::Error::other)?,
        ),
        Codec::Lzip => Box::new(lzma_rust2::LzipReader::new(BufReader::new(source))),
        Codec::Zstd => Box::new(zstd::stream::read::Decoder::new(source)?),
        Codec::Lz4 => Box::new(lz4_flex::frame::FrameDecoder::new(BufReader::new(source))),
        Codec::Compress => Box::new(LzwReader::new(BufReader::new(source))?),
        Codec::Brotli => Box::new(brotli_decompressor::Decompressor::new(source, 64 << 10)),
        Codec::Zlib => Box::new(flate2::read::ZlibDecoder::new(source)),
    })
}

fn threads() -> u32 {
    std::thread::available_parallelism().map_or(1, |n| n.get() as u32)
}

/// The decompressed size, when the format records it.
pub(crate) fn uncompressed_size(codec: Codec, source: &Source) -> Option<u64> {
    let mut source = source.clone();
    match codec {
        Codec::Xz => lzma_rust2::xz_uncompressed_size(&mut source).ok(),
        Codec::Lzip => {
            // A single member's trailer holds its data size and member size.
            let len = source.len();
            let mut trailer = [0u8; 20];
            source.seek(SeekFrom::End(-20)).ok()?;
            source.read_exact(&mut trailer).ok()?;
            let data = u64::from_le_bytes(trailer[4..12].try_into().ok()?);
            let member = u64::from_le_bytes(trailer[12..20].try_into().ok()?);
            (member == len).then_some(data)
        }
        Codec::Lzma => {
            let mut header = [0u8; 13];
            source.read_exact(&mut header).ok()?;
            let size = u64::from_le_bytes(header[5..13].try_into().ok()?);
            (size != u64::MAX).then_some(size)
        }
        Codec::Zstd => {
            let mut head = [0u8; 18];
            let n = source.read(&mut head).ok()?;
            zstd::zstd_safe::get_frame_content_size(&head[..n])
                .ok()
                .flatten()
        }
        _ => None,
    }
}

/// The size gzip records, modulo 2^32, as an estimate; it is exact for files
/// under 4 GiB with a single member.
pub(crate) fn gzip_size_hint(source: &Source) -> Option<u64> {
    let mut source = source.clone();
    let mut trailer = [0u8; 4];
    source.seek(SeekFrom::End(-4)).ok()?;
    source.read_exact(&mut trailer).ok()?;
    Some(u64::from(u32::from_le_bytes(trailer)))
}

const CHUNK: usize = 1 << 20;

/// Decompresses on a thread of its own, up to four chunks ahead of the
/// reader, so decoding overlaps whatever consumes the bytes: parsing tar
/// headers and writing files. Dropping it stops the thread after the chunk
/// in progress.
pub(crate) struct ReadAhead {
    chunks: Receiver<io::Result<Vec<u8>>>,
    current: Vec<u8>,
    pos: usize,
}

impl ReadAhead {
    pub fn new(mut inner: Box<dyn Read + Send>) -> Self {
        let (sender, chunks) = sync_channel(4);
        std::thread::Builder::new()
            .name("uunpack-decode".into())
            // Some decoders keep large tables on the stack.
            .stack_size(16 << 20)
            .spawn(move || {
                loop {
                    let mut chunk = vec![0u8; CHUNK];
                    let message = match read_up_to(&mut inner, &mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            chunk.truncate(n);
                            Ok(chunk)
                        }
                        Err(e) => Err(e),
                    };
                    let failed = message.is_err();
                    if sender.send(message).is_err() || failed {
                        break;
                    }
                }
            })
            .expect("spawn decoder thread");
        Self {
            chunks,
            current: Vec::new(),
            pos: 0,
        }
    }
}

impl Read for ReadAhead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos == self.current.len() {
            match self.chunks.recv() {
                Ok(chunk) => {
                    self.current = chunk?;
                    self.pos = 0;
                }
                // The decoder finished.
                Err(_) => return Ok(0),
            }
        }
        let n = buf.len().min(self.current.len() - self.pos);
        buf[..n].copy_from_slice(&self.current[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}
