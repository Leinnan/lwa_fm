//! Finds the sibling files of a multi-volume archive.
//!
//! RAR uses two naming schemes: `name.part1.rar`, `name.part2.rar`, … (RAR 3+,
//! with any zero padding) and `name.rar`, `name.r00`, `name.r01`, … (legacy).
//! Other formats are split into numbered pieces: `name.7z.001`, `name.7z.002`,
//! … Opening any volume of a set resolves to the whole set, starting at the
//! first.

use std::path::{Path, PathBuf};

use crate::error::{CoreError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Scheme {
    Single,
    /// `{base}.{part}{n:0width$}.{ext}`, numbered from 1.
    Parts {
        base: String,
        part: String,
        width: usize,
        ext: String,
    },
    /// `{base}.{ext}` followed by `{base}.{r}00`, `{base}.{r}01`, …
    Legacy {
        base: String,
        ext: String,
        r: char,
    },
    /// `{base}.{n:0width$}`, numbered from 1: one byte stream in pieces.
    Numbered {
        base: String,
        width: usize,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct VolumeSet {
    dir: PathBuf,
    scheme: Scheme,
    pub paths: Vec<PathBuf>,
}

impl VolumeSet {
    /// Resolves `path` to its volume set. Fails if the first volume is missing;
    /// missing trailing volumes are detected later, from the split flags.
    pub fn discover(path: &Path) -> Result<Self> {
        let dir = path.parent().unwrap_or(Path::new("")).to_path_buf();
        let scheme = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|name| scheme_for(&dir, name))
            .unwrap_or(Scheme::Single);

        let mut set = Self {
            dir,
            scheme,
            paths: Vec::new(),
        };
        if set.scheme == Scheme::Single {
            set.paths.push(path.to_path_buf());
            return Ok(set);
        }
        for index in 0.. {
            let candidate = set.dir.join(set.name(index));
            if !candidate.is_file() {
                break;
            }
            set.paths.push(candidate);
        }
        if set.paths.is_empty() {
            return Err(CoreError::MissingVolume { name: set.name(0) });
        }
        Ok(set)
    }

    /// File name of the volume that follows the last one found.
    pub fn next_volume_name(&self) -> String {
        match self.scheme {
            Scheme::Single => String::new(),
            _ => self.name(self.paths.len()),
        }
    }

    /// File name of the volume at zero-based `index`.
    fn name(&self, index: usize) -> String {
        match &self.scheme {
            Scheme::Single => String::new(),
            Scheme::Parts {
                base,
                part,
                width,
                ext,
            } => format!("{base}.{part}{:0width$}.{ext}", index + 1),
            Scheme::Legacy { base, ext, .. } if index == 0 => format!("{base}.{ext}"),
            Scheme::Legacy { base, r, .. } => format!("{base}.{r}{:02}", index - 1),
            Scheme::Numbered { base, width } => format!("{base}.{:0width$}", index + 1),
        }
    }
}

/// Display name for an archive without volume and archive suffixes:
/// `Photos.part01.rar` → `Photos`, `Comic.cbr` → `Comic`.
pub(crate) fn archive_stem(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut stem = name.as_str();
    if let (base, Some(ext)) = split_ext(stem)
        && is_piece_number(ext)
    {
        stem = base;
    }
    if let (base, Some(ext)) = split_ext(stem)
        && (is_known_ext(ext) || is_legacy_volume_ext(ext))
    {
        stem = base;
    }
    // `name.tar.gz`, `name.part01.rar`
    if let (base, Some(ext)) = split_ext(stem)
        && (ext.eq_ignore_ascii_case("tar") || part_number(ext).is_some())
    {
        stem = base;
    }
    if stem.is_empty() {
        "Archive".to_string()
    } else {
        stem.to_string()
    }
}

fn scheme_for(dir: &Path, name: &str) -> Scheme {
    let (stem, Some(ext)) = split_ext(name) else {
        return Scheme::Single;
    };
    if is_piece_number(ext) {
        return Scheme::Numbered {
            base: stem.to_string(),
            width: ext.len(),
        };
    }
    if is_rar_ext(ext) {
        if let (base, Some(suffix)) = split_ext(stem)
            && let Some((part, digits)) = part_number(suffix)
        {
            return Scheme::Parts {
                base: base.to_string(),
                part: part.to_string(),
                width: digits.len(),
                ext: ext.to_string(),
            };
        }
        let r = if ext.chars().all(|c| c.is_ascii_uppercase()) {
            'R'
        } else {
            'r'
        };
        if dir.join(format!("{stem}.{r}00")).is_file() {
            return Scheme::Legacy {
                base: stem.to_string(),
                ext: ext.to_string(),
                r,
            };
        }
        return Scheme::Single;
    }
    if is_legacy_volume_ext(ext) {
        let r = ext.chars().next().unwrap_or('r');
        let rar = if r == 'R' { "RAR" } else { "rar" };
        return Scheme::Legacy {
            base: stem.to_string(),
            ext: rar.to_string(),
            r,
        };
    }
    Scheme::Single
}

fn split_ext(name: &str) -> (&str, Option<&str>) {
    match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], Some(&name[dot + 1..])),
        _ => (name, None),
    }
}

fn is_rar_ext(ext: &str) -> bool {
    ext.eq_ignore_ascii_case("rar") || ext.eq_ignore_ascii_case("cbr")
}

fn is_known_ext(ext: &str) -> bool {
    crate::formats::detect::EXTENSIONS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(ext))
}

/// `001`, `002`, …: a piece of a split file.
fn is_piece_number(ext: &str) -> bool {
    ext.len() >= 3 && ext.bytes().all(|b| b.is_ascii_digit())
}

/// Whether a file of this name is something uunpack opens: an archive, a
/// compressed file, a RAR volume or a piece of a split archive.
pub(crate) fn is_archive_name(name: &str) -> bool {
    match split_ext(name) {
        (_, Some(ext)) => is_known_ext(ext) || is_legacy_volume_ext(ext) || is_piece_number(ext),
        _ => false,
    }
}

/// `r00` … `r99`.
fn is_legacy_volume_ext(ext: &str) -> bool {
    let bytes = ext.as_bytes();
    bytes.len() == 3
        && bytes[0].eq_ignore_ascii_case(&b'r')
        && bytes[1..].iter().all(u8::is_ascii_digit)
}

/// Splits `part07` into (`part`, `07`).
fn part_number(suffix: &str) -> Option<(&str, &str)> {
    if suffix.len() <= 4 || !suffix.is_char_boundary(4) {
        return None;
    }
    let (word, digits) = suffix.split_at(4);
    (word.eq_ignore_ascii_case("part") && digits.bytes().all(|b| b.is_ascii_digit()))
        .then_some((word, digits))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, names: &[&str]) {
        for name in names {
            std::fs::write(dir.join(name), b"").unwrap();
        }
    }

    fn names(set: &VolumeSet) -> Vec<String> {
        set.paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn part_scheme_from_any_volume() {
        let dir = tempfile::tempdir().unwrap();
        touch(
            dir.path(),
            &["a.part01.rar", "a.part02.rar", "a.part03.rar"],
        );
        for opened in ["a.part01.rar", "a.part03.rar"] {
            let set = VolumeSet::discover(&dir.path().join(opened)).unwrap();
            assert_eq!(
                names(&set),
                ["a.part01.rar", "a.part02.rar", "a.part03.rar"]
            );
            assert_eq!(set.next_volume_name(), "a.part04.rar");
        }
    }

    #[test]
    fn missing_first_part_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), &["a.part2.rar"]);
        let err = VolumeSet::discover(&dir.path().join("a.part2.rar")).unwrap_err();
        assert!(matches!(err, CoreError::MissingVolume { name } if name == "a.part1.rar"));
    }

    #[test]
    fn legacy_scheme_from_rar_and_r_volumes() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), &["b.rar", "b.r00", "b.r01"]);
        for opened in ["b.rar", "b.r01"] {
            let set = VolumeSet::discover(&dir.path().join(opened)).unwrap();
            assert_eq!(names(&set), ["b.rar", "b.r00", "b.r01"]);
            assert_eq!(set.next_volume_name(), "b.r02");
        }
    }

    #[test]
    fn numbered_pieces_from_any_piece() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), &["d.7z.001", "d.7z.002", "d.7z.003"]);
        for opened in ["d.7z.001", "d.7z.003"] {
            let set = VolumeSet::discover(&dir.path().join(opened)).unwrap();
            assert_eq!(names(&set), ["d.7z.001", "d.7z.002", "d.7z.003"]);
        }
        assert!(is_archive_name("d.7z.002"));
        assert!(is_archive_name("a.tar.zst"));
        assert!(!is_archive_name("notes.txt"));
    }

    #[test]
    fn plain_archive_is_single() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), &["c.rar"]);
        let set = VolumeSet::discover(&dir.path().join("c.rar")).unwrap();
        assert_eq!(names(&set), ["c.rar"]);
    }

    #[test]
    fn stems() {
        assert_eq!(archive_stem(Path::new("/x/Photos.part01.rar")), "Photos");
        assert_eq!(archive_stem(Path::new("Comic.cbr")), "Comic");
        assert_eq!(archive_stem(Path::new("old.r03")), "old");
        assert_eq!(archive_stem(Path::new("v1.2.rar")), "v1.2");
        assert_eq!(archive_stem(Path::new("noext")), "noext");
        assert_eq!(archive_stem(Path::new("Photos.zip")), "Photos");
        assert_eq!(archive_stem(Path::new("src.tar.gz")), "src");
        assert_eq!(archive_stem(Path::new("disk.7z.001")), "disk");
        assert_eq!(archive_stem(Path::new("notes.txt.gz")), "notes.txt");
    }
}
