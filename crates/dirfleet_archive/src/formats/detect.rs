//! Recognizes formats by their content, falling back to the file name only
//! for formats without a signature.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Rar,
    Zip,
    SevenZip,
    Tar,
    /// A compressed stream: a single file, or a tar archive inside.
    Compressed(Codec),
    Cpio,
    /// ar, or a Debian package built on it.
    Ar,
    Rpm,
    /// XAR, or a macOS installer package built on it.
    Xar,
    Cab,
    /// A disk or disc image, attached with hdiutil.
    Image,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Codec {
    Gzip,
    Bzip2,
    Xz,
    Lzma,
    Lzip,
    Zstd,
    Lz4,
    /// Unix `compress` (LZW), `.Z`.
    Compress,
    Brotli,
    /// zlib streams, as XAR stores members it calls gzip.
    Zlib,
}

impl Codec {
    pub fn name(self) -> &'static str {
        match self {
            Codec::Gzip => "gzip",
            Codec::Bzip2 => "bzip2",
            Codec::Xz => "xz",
            Codec::Lzma => "lzma",
            Codec::Lzip => "lzip",
            Codec::Zstd => "zstd",
            Codec::Lz4 => "LZ4",
            Codec::Compress => "compress",
            Codec::Brotli => "Brotli",
            Codec::Zlib => "zlib",
        }
    }

    /// Extensions a compressed file of this codec ends with.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Codec::Gzip => &["gz", "gzip"],
            Codec::Bzip2 => &["bz2", "bz"],
            Codec::Xz => &["xz"],
            Codec::Lzma => &["lzma"],
            Codec::Lzip => &["lz"],
            Codec::Zstd => &["zst", "zstd"],
            Codec::Lz4 => &["lz4"],
            Codec::Compress => &["Z"],
            Codec::Brotli => &["br"],
            Codec::Zlib => &[],
        }
    }
}

/// Extensions of the ZIP family, which may begin with other data (a
/// self-extractor), so the name is trusted when the start isn't a ZIP header.
const ZIP_EXTENSIONS: &[&str] = &[
    "zip", "cbz", "jar", "apk", "epub", "ipa", "xpi", "aar", "war",
];

/// Every extension the app can open, for document types and open panels.
pub(crate) const EXTENSIONS: &[&str] = &[
    "rar",
    "cbr",
    "zip",
    "cbz",
    "jar",
    "apk",
    "epub",
    "ipa",
    "xpi",
    "7z",
    "cb7",
    "tar",
    "cbt",
    "tgz",
    "taz",
    "tbz",
    "tbz2",
    "tb2",
    "txz",
    "tlz",
    "tzst",
    "tz",
    "gz",
    "gzip",
    "bz2",
    "bz",
    "xz",
    "lzma",
    "lz",
    "zst",
    "zstd",
    "lz4",
    "Z",
    "br",
    "cpio",
    "a",
    "ar",
    "deb",
    "udeb",
    "rpm",
    "xar",
    "pkg",
    "mpkg",
    "cab",
    // Disk images, and the formats only the libarchive fallback reads, are
    // not opened on Windows.
    #[cfg(not(windows))]
    "dmg",
    #[cfg(not(windows))]
    "iso",
    #[cfg(not(windows))]
    "cdr",
    #[cfg(not(windows))]
    "toast",
    #[cfg(not(windows))]
    "sparseimage",
    #[cfg(not(windows))]
    "lzh",
    #[cfg(not(windows))]
    "lha",
    #[cfg(not(windows))]
    "warc",
];

/// Disk image extensions; raw images have no signature.
const IMAGE_EXTENSIONS: &[&str] = &["dmg", "iso", "cdr", "toast", "sparseimage", "img"];

/// How much of a file's start `detect` looks at: an ISO 9660 signature sits
/// 32 KiB in.
pub(crate) const HEAD: usize = 0x8800;
/// How much of a file's end it looks at: a UDIF disk image's trailer.
pub(crate) const TAIL: usize = 512;

/// The format of an archive starting with `head`, the first [`HEAD`] bytes
/// of the file at `path`, and ending with `tail`, its last [`TAIL`] bytes.
pub(crate) fn detect(path: &Path, head: &[u8], tail: &[u8]) -> Option<Format> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let lower = ext.to_ascii_lowercase();
    if head.starts_with(b"Rar!\x1a\x07") || head.starts_with(b"RE~^") {
        return Some(Format::Rar);
    }
    if head.starts_with(b"7z\xbc\xaf\x27\x1c") {
        return Some(Format::SevenZip);
    }
    if head.starts_with(b"PK\x03\x04")
        || head.starts_with(b"PK\x05\x06")
        || head.starts_with(b"PK\x07\x08")
    {
        return Some(Format::Zip);
    }
    let codec = if head.starts_with(&[0x1f, 0x8b]) {
        Some(Codec::Gzip)
    } else if head.starts_with(b"BZh") && head.get(3).is_some_and(|b| (b'1'..=b'9').contains(b)) {
        Some(Codec::Bzip2)
    } else if head.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0]) {
        Some(Codec::Xz)
    } else if head.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        Some(Codec::Zstd)
    } else if head.starts_with(&[0x04, 0x22, 0x4d, 0x18]) {
        Some(Codec::Lz4)
    } else if head.starts_with(&[0x1f, 0x9d]) {
        Some(Codec::Compress)
    } else if head.starts_with(b"LZIP") {
        Some(Codec::Lzip)
    } else {
        None
    };
    if let Some(codec) = codec {
        return Some(Format::Compressed(codec));
    }
    if is_tar(head) {
        return Some(Format::Tar);
    }
    if head.starts_with(super::ar::MAGIC) {
        return Some(Format::Ar);
    }
    if head.starts_with(&super::rpm::MAGIC) {
        return Some(Format::Rpm);
    }
    if head.starts_with(super::xar::MAGIC) {
        return Some(Format::Xar);
    }
    if head.starts_with(super::cab::MAGIC) {
        return Some(Format::Cab);
    }
    if super::cpio::is_cpio(head) {
        return Some(Format::Cpio);
    }
    let volume_descriptor = head.get(0x8001..0x8006);
    if volume_descriptor == Some(b"CD001") || volume_descriptor == Some(b"BEA01") {
        return Some(Format::Image);
    }
    if tail.len() == TAIL && tail.starts_with(b"koly") {
        return Some(Format::Image);
    }
    // No signature: raw LZMA and Brotli streams, self-extracting ZIPs.
    match lower.as_str() {
        "lzma" | "tlz" => Some(Format::Compressed(Codec::Lzma)),
        "br" => Some(Format::Compressed(Codec::Brotli)),
        "rar" | "cbr" => Some(Format::Rar),
        e if IMAGE_EXTENSIONS.contains(&e) => Some(Format::Image),
        e if ZIP_EXTENSIONS.contains(&e) => Some(Format::Zip),
        e if e.len() == 3 && e.starts_with('r') && e[1..].bytes().all(|b| b.is_ascii_digit()) => {
            Some(Format::Rar)
        }
        _ => None,
    }
}

/// Whether `block` starts with a tar header: the POSIX magic, or a valid
/// header checksum for archives older than POSIX.
pub(crate) fn is_tar(block: &[u8]) -> bool {
    if block.len() < 512 {
        return false;
    }
    if &block[257..262] == b"ustar" {
        return true;
    }
    let field = &block[148..156];
    let digits: Vec<u8> = field
        .iter()
        .copied()
        .skip_while(|b| *b == b' ')
        .take_while(|b| (b'0'..=b'7').contains(b))
        .collect();
    let Some(stored) = std::str::from_utf8(&digits)
        .ok()
        .and_then(|s| u32::from_str_radix(s, 8).ok())
    else {
        return false;
    };
    let sum: u32 = block[..512]
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                u32::from(b' ')
            } else {
                u32::from(b)
            }
        })
        .sum();
    // An all-zero block sums to 256 and isn't an archive.
    block[0] != 0 && sum == stored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_win_over_names() {
        let p = Path::new("x.rar");
        assert_eq!(detect(p, b"PK\x03\x04rest", &[]), Some(Format::Zip));
        assert_eq!(
            detect(p, b"7z\xbc\xaf\x27\x1c", &[]),
            Some(Format::SevenZip)
        );
        assert_eq!(
            detect(Path::new("x.bin"), &[0x1f, 0x8b, 8], &[]),
            Some(Format::Compressed(Codec::Gzip))
        );
        assert_eq!(
            detect(Path::new("x.bin"), b"BZh9", &[]),
            Some(Format::Compressed(Codec::Bzip2))
        );
        assert_eq!(detect(Path::new("x.bin"), b"BZhx", &[]), None);
    }

    #[test]
    fn names_for_formats_without_signatures() {
        assert_eq!(
            detect(Path::new("a.lzma"), b"\x5d\0\0", &[]),
            Some(Format::Compressed(Codec::Lzma))
        );
        assert_eq!(
            detect(Path::new("a.exe.zip"), b"MZ", &[]),
            Some(Format::Zip)
        );
        assert_eq!(detect(Path::new("a.r07"), b"", &[]), Some(Format::Rar));
        assert_eq!(detect(Path::new("a.txt"), b"hello", &[]), None);
    }

    #[test]
    fn tar_headers() {
        let mut block = [0u8; 512];
        block[..5].copy_from_slice(b"a.txt");
        assert!(!is_tar(&block));
        block[148..156].copy_from_slice(b"        ");
        let sum: u32 = block.iter().map(|&b| u32::from(b)).sum();
        block[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        assert!(is_tar(&block));
        block[257..262].copy_from_slice(b"ustar");
        assert!(is_tar(&block));
        assert!(!is_tar(&[0u8; 512]));
    }
}
