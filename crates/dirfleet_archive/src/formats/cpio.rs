//! cpio archives in all four variants: portable ASCII (odc, `070707`, the
//! one in macOS installer payloads), new ASCII (newc, `070701`) and its CRC
//! form (`070702`, both in RPM payloads), and the old binary format in
//! either byte order. Read front to back, so it serves compressed payloads.

use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use super::composite::Opener;
use super::{Backend, Job, Link, Listing, Sink, copy, read_error, read_target};
use crate::archive::{Kind, Member};
use crate::error::{CoreError, Result};
use crate::names::{self, Encoding};
use crate::source::Source;
use crate::volumes::VolumeSet;

const TRAILER: &[u8] = b"TRAILER!!!";
const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Variant {
    Odc,
    Newc,
    BinaryLe,
    BinaryBe,
}

pub(crate) struct Header {
    pub name: Vec<u8>,
    pub mode: u32,
    pub mtime: u64,
    pub size: u64,
    /// Device and inode, which hard links share.
    pub inode: (u64, u64),
    pub links: u64,
}

/// Whether `head` starts a cpio archive.
pub(crate) fn is_cpio(head: &[u8]) -> bool {
    head.starts_with(b"070707")
        || head.starts_with(b"070701")
        || head.starts_with(b"070702")
        || head.starts_with(&[0xc7, 0x71])
        || head.starts_with(&[0x71, 0xc7])
}

/// Reads entries one by one; each entry's data must be read, or is skipped,
/// before the next header.
pub(crate) struct CpioReader<R> {
    inner: R,
    variant: Option<Variant>,
    /// Data left of the current entry, and padding after it.
    remaining: u64,
    padding: u64,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

fn number(field: &[u8], radix: u32) -> io::Result<u64> {
    let text = std::str::from_utf8(field).map_err(|_| invalid("corrupt cpio header"))?;
    u64::from_str_radix(text.trim_end_matches('\0').trim(), radix)
        .map_err(|_| invalid("corrupt cpio header"))
}

impl<R: Read> CpioReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            variant: None,
            remaining: 0,
            padding: 0,
        }
    }

    fn skip(&mut self, mut count: u64) -> io::Result<()> {
        let mut buf = [0u8; 16 << 10];
        while count > 0 {
            let want = count.min(buf.len() as u64) as usize;
            let n = self.inner.read(&mut buf[..want])?;
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            count -= n as u64;
        }
        Ok(())
    }

    /// The next entry, or `None` after the trailer.
    pub fn next_header(&mut self) -> io::Result<Option<Header>> {
        let rest = self.remaining + self.padding;
        self.skip(rest)?;
        self.remaining = 0;
        self.padding = 0;

        let mut magic = [0u8; 6];
        if super::read_up_to(&mut self.inner, &mut magic)? < 2 {
            return Ok(None);
        }
        let variant = match &magic {
            b"070707" => Variant::Odc,
            b"070701" | b"070702" => Variant::Newc,
            [0xc7, 0x71, ..] => Variant::BinaryLe,
            [0x71, 0xc7, ..] => Variant::BinaryBe,
            _ => return Err(invalid("not a cpio archive, or a damaged one")),
        };
        self.variant.get_or_insert(variant);
        let (header, name_size, name_pad, data_pad) = match variant {
            Variant::Odc => {
                let mut h = [0u8; 70];
                self.inner.read_exact(&mut h)?;
                let field = |at: usize, len: usize| number(&h[at - 6..at - 6 + len], 8);
                let size = field(65, 11)?;
                let name_size = field(59, 6)?;
                let header = Header {
                    name: Vec::new(),
                    mode: field(18, 6)? as u32,
                    mtime: field(48, 11)?,
                    size,
                    inode: (field(6, 6)?, field(12, 6)?),
                    links: field(36, 6)?,
                };
                (header, name_size, 0, 0)
            }
            Variant::Newc => {
                let mut h = [0u8; 104];
                self.inner.read_exact(&mut h)?;
                let field = |i: usize| number(&h[i * 8..i * 8 + 8], 16);
                let size = field(6)?;
                let name_size = field(11)?;
                let header = Header {
                    name: Vec::new(),
                    mode: field(1)? as u32,
                    mtime: field(5)?,
                    size,
                    inode: ((field(7)? << 32) | field(8)?, field(0)?),
                    links: field(4)?,
                };
                // Header and name are padded to four bytes, and so is the data.
                let name_pad = (4 - (110 + name_size) % 4) % 4;
                (header, name_size, name_pad, (4 - size % 4) % 4)
            }
            Variant::BinaryLe | Variant::BinaryBe => {
                // 13 16-bit words: magic dev ino mode uid gid nlink rdev
                // mtime(2) namesize filesize(2), 32-bit values high word first.
                let mut h = [0u8; 26];
                h[..6].copy_from_slice(&magic);
                self.inner.read_exact(&mut h[6..])?;
                let word = |i: usize| {
                    let bytes = [h[i * 2], h[i * 2 + 1]];
                    u64::from(if variant == Variant::BinaryLe {
                        u16::from_le_bytes(bytes)
                    } else {
                        u16::from_be_bytes(bytes)
                    })
                };
                let size = (word(11) << 16) | word(12);
                let name_size = word(10);
                let header = Header {
                    name: Vec::new(),
                    mode: word(3) as u32,
                    mtime: (word(8) << 16) | word(9),
                    size,
                    inode: (word(1), word(2)),
                    links: word(6),
                };
                (header, name_size, name_size % 2, size % 2)
            }
        };
        if name_size == 0 || name_size > 64 << 10 {
            return Err(invalid("corrupt cpio header"));
        }
        let mut name = vec![0u8; name_size as usize];
        self.inner.read_exact(&mut name)?;
        self.skip(name_pad)?;
        while name.last() == Some(&0) {
            name.pop();
        }
        if name == TRAILER {
            return Ok(None);
        }
        self.remaining = header.size;
        self.padding = data_pad;
        Ok(Some(Header { name, ..header }))
    }
}

/// The current entry's data.
impl<R: Read> Read for CpioReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let want = (buf.len() as u64).min(self.remaining) as usize;
        if want == 0 {
            return Ok(0);
        }
        let n = self.inner.read(&mut buf[..want])?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.remaining -= n as u64;
        Ok(n)
    }
}

/// A cpio archive read through `open`, which yields its decoded bytes.
pub(crate) struct Cpio {
    open: Opener,
    /// For hard-link groups (newc stores the data with the last link), the
    /// member holding the data, by member index.
    link_data: HashMap<usize, usize>,
}

/// A cpio archive file.
pub(crate) fn open_file(volumes: VolumeSet, encoding: Encoding) -> Result<(Cpio, Listing)> {
    let source = Source::open(&volumes.paths)?;
    let packed = source.len();
    let opener: Opener = Arc::new(move || -> Result<Box<dyn Read + Send>> {
        let mut source = source.clone();
        source
            .seek(SeekFrom::Start(0))
            .map_err(|e| read_error(e, false))?;
        Ok(Box::new(source))
    });
    let (cpio, members) = open(opener, encoding)?;
    let listing = Listing {
        members,
        volumes: volumes.paths,
        format: "cpio".to_string(),
        solid: false,
        comment: None,
        sizes_exact: true,
        packed_size: Some(packed),
    };
    Ok((cpio, listing))
}

/// Lists the archive `open` yields.
pub(crate) fn open(open: Opener, encoding: Encoding) -> Result<(Cpio, Vec<Member>)> {
    let mut reader = CpioReader::new(open()?);
    let mut members = Vec::new();
    let mut inodes: HashMap<(u64, u64), Vec<usize>> = HashMap::new();
    while let Some(header) = reader.next_header().map_err(|e| read_error(e, false))? {
        let name = names::decode(&header.name, encoding);
        let kind = match header.mode & S_IFMT {
            S_IFDIR => Kind::Dir,
            S_IFREG => Kind::File,
            // Symlinks, and devices and pipes, which are skipped.
            _ => Kind::Link,
        };
        if kind == Kind::File && header.links > 1 {
            inodes.entry(header.inode).or_default().push(members.len());
        }
        members.push(Member {
            rel_path: names::safe_relative_path(&name, false),
            display: String::from_utf8_lossy(&name).into_owned(),
            entry_id: None,
            kind,
            size: header.size,
            packed_size: 0,
            mtime: Some(UNIX_EPOCH + Duration::from_secs(header.mtime)),
            mode: Some(header.mode & 0o7777),
            encrypted: false,
            hidden: false,
        });
    }
    let mut link_data = HashMap::new();
    for group in inodes.values() {
        if let Some(&data) = group.iter().find(|&&i| members[i].size > 0) {
            let others: Vec<usize> = group
                .iter()
                .copied()
                .filter(|&i| i != data && members[i].size == 0)
                .collect();
            for other in others {
                link_data.insert(other, data);
                // Listed with the size of the file it shares.
                members[other].size = members[data].size;
            }
        }
    }
    Ok((Cpio { open, link_data }, members))
}

impl Backend for Cpio {
    fn extract(&self, job: &Job<'_>, sink: &dyn Sink) -> Result<()> {
        let Some(last) = job.last_wanted() else {
            return Ok(());
        };
        let mut reader = CpioReader::new((self.open)()?);
        let mut copies = Vec::new();
        let mut index = 0;
        while let Some(header) = reader.next_header().map_err(|e| read_error(e, false))? {
            let current = index;
            index += 1;
            if !job.wanted[current] {
                if current > last && copies.is_empty() {
                    break;
                }
                continue;
            }
            job.check_cancelled()?;
            if let Some(&data) = self.link_data.get(&current) {
                // The data comes with another link, maybe later.
                copies.push((current, data));
                continue;
            }
            match job.members[current].kind {
                Kind::Dir => drop(sink.open(current)?),
                Kind::File => copy(&mut reader, &mut sink.open(current)?, job, false)?,
                Kind::Link if header.mode & S_IFMT == S_IFLNK => {
                    let target = read_target(&mut reader, false)?;
                    sink.link(current, Link::Symlink(&target))?;
                }
                Kind::Link => sink.link(current, Link::Unsupported)?,
            }
        }
        for (link, data) in copies {
            match job.members[data].rel_path.as_ref() {
                Some(target) if job.wanted[data] => {
                    let target = target.to_string_lossy().into_owned();
                    sink.link(link, Link::Copy(target.as_bytes()))?;
                }
                // The file with the data wasn't extracted; nothing to copy.
                _ => sink.link(link, Link::Unsupported)?,
            }
        }
        if index == 0 && job.last_wanted().is_some() {
            return Err(CoreError::Corrupt {
                message: "the cpio archive is empty".to_string(),
            });
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    /// Archives from macOS's own cpio, in each format it writes.
    #[test]
    fn reads_system_cpio() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("d")).unwrap();
        std::fs::write(dir.path().join("d/a.txt"), b"alpha").unwrap();
        std::fs::write(dir.path().join("b.bin"), vec![7u8; 5000]).unwrap();
        for format in ["odc", "newc", "bin"] {
            let output = Command::new("sh")
                .args([
                    "-c",
                    &format!("printf 'd\\nd/a.txt\\nb.bin\\n' | cpio -o -H {format} 2>/dev/null"),
                ])
                .current_dir(dir.path())
                .stdout(Stdio::piped())
                .output()
                .unwrap();
            assert!(is_cpio(&output.stdout), "{format}");
            let mut reader = CpioReader::new(&output.stdout[..]);
            let mut seen = Vec::new();
            while let Some(header) = reader.next_header().unwrap() {
                let mut data = Vec::new();
                reader.read_to_end(&mut data).unwrap();
                seen.push((String::from_utf8(header.name).unwrap(), data.len()));
            }
            assert_eq!(
                seen,
                [
                    ("d".into(), 0),
                    ("d/a.txt".into(), 5),
                    ("b.bin".into(), 5000)
                ],
                "{format}"
            );
        }
    }
}
