//! Archive bytes as one `Read + Seek` stream: a single file, or volumes
//! that are one stream cut into pieces (`name.7z.001`, `name.zip.002`, …).
//! Clones share the open files and read at explicit offsets, so parallel
//! workers never depend on each other's position.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;

use crate::error::{CoreError, Result};

/// Reads smaller than this are served from a buffer of this size, so that
/// parsing headers field by field doesn't cost a system call each.
const BUFFER: usize = 64 << 10;

struct Part {
    file: File,
    start: u64,
    len: u64,
}

pub(crate) struct Source {
    parts: Arc<[Part]>,
    /// Offset of this view in the underlying stream; slices start later.
    base: u64,
    len: u64,
    pos: u64,
    buffer: Vec<u8>,
    /// Stream offset of `buffer[0]`.
    buffer_start: u64,
}

impl Clone for Source {
    fn clone(&self) -> Self {
        Self {
            parts: self.parts.clone(),
            base: self.base,
            len: self.len,
            pos: self.pos,
            buffer: Vec::new(),
            buffer_start: 0,
        }
    }
}

impl Source {
    pub fn open(paths: &[PathBuf]) -> Result<Self> {
        let mut start = 0;
        let parts = paths
            .iter()
            .map(|path| {
                let file = File::open(path).map_err(|e| CoreError::io(path, e))?;
                let len = file.metadata().map_err(|e| CoreError::io(path, e))?.len();
                let part = Part { file, start, len };
                start += len;
                Ok(part)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            parts: parts.into(),
            base: 0,
            len: start,
            pos: 0,
            buffer: Vec::new(),
            buffer_start: 0,
        })
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    /// The `len` bytes at `offset` of this view, as a view of their own,
    /// positioned at its start. Clamped to the bytes there are.
    pub fn slice(&self, offset: u64, len: u64) -> Self {
        let offset = offset.min(self.len);
        Self {
            parts: self.parts.clone(),
            base: self.base + offset,
            len: len.min(self.len - offset),
            pos: 0,
            buffer: Vec::new(),
            buffer_start: 0,
        }
    }

    /// Reads at `pos` of this view from the part that holds it, never
    /// across parts.
    fn read_at(&self, buf: &mut [u8], pos: u64) -> io::Result<usize> {
        if pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let at = self.base + pos;
        let part = &self.parts[self.parts.partition_point(|p| p.start + p.len <= at)];
        let offset = at - part.start;
        let want = (buf.len() as u64)
            .min(part.len - offset)
            .min(self.len - pos) as usize;
        crate::platform::read_at(&part.file, &mut buf[..want], offset)
    }
}

impl Read for Source {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let buffered = self.pos >= self.buffer_start
            && self.pos < self.buffer_start + self.buffer.len() as u64;
        if !buffered {
            if buf.len() >= BUFFER {
                let n = self.read_at(buf, self.pos)?;
                self.pos += n as u64;
                return Ok(n);
            }
            let mut buffer = std::mem::take(&mut self.buffer);
            buffer.resize(BUFFER, 0);
            let n = self.read_at(&mut buffer, self.pos)?;
            buffer.truncate(n);
            self.buffer = buffer;
            self.buffer_start = self.pos;
            if n == 0 {
                return Ok(0);
            }
        }
        let offset = (self.pos - self.buffer_start) as usize;
        let n = buf.len().min(self.buffer.len() - offset);
        buf[..n].copy_from_slice(&self.buffer[offset..offset + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Source {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let pos = match to {
            SeekFrom::Start(pos) => Some(pos),
            SeekFrom::End(delta) => self.len.checked_add_signed(delta),
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
        };
        self.pos = pos
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"))?;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_across_parts() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = ["a.001", "a.002", "a.003"]
            .iter()
            .zip([&b"hello "[..], b"", b"world"])
            .map(|(name, data)| {
                let path = dir.path().join(name);
                std::fs::write(&path, data).unwrap();
                path
            })
            .collect();
        let mut source = Source::open(&paths).unwrap();
        assert_eq!(source.len(), 11);
        let mut all = String::new();
        source.read_to_string(&mut all).unwrap();
        assert_eq!(all, "hello world");
        source.seek(SeekFrom::End(-5)).unwrap();
        let mut tail = [0; 3];
        source.read_exact(&mut tail).unwrap();
        assert_eq!(&tail, b"wor");
        let mut clone = source.clone();
        clone.seek(SeekFrom::Start(4)).unwrap();
        clone.read_exact(&mut tail).unwrap();
        assert_eq!(&tail, b"o w");
        let mut slice = source.slice(3, 5);
        let mut middle = String::new();
        slice.read_to_string(&mut middle).unwrap();
        assert_eq!(middle, "lo wo");
        assert_eq!(source.slice(9, 100).len(), 2);
    }
}
