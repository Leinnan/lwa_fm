//! AppleDouble files, the `__MACOSX/…/._name` companions Finder and ditto
//! put in ZIP archives to carry what ZIP can't: extended attributes, Finder
//! info and the resource fork. Archive Utility hides them and restores what
//! they hold; so does uunpack.

/// The attributes an AppleDouble file holds, as (name, value) pairs.
pub(crate) type Attributes = Vec<(String, Vec<u8>)>;

const MAGIC: u32 = 0x0005_1607;
const RESOURCE_FORK: u32 = 2;
const FINDER_INFO: u32 = 9;
const FINDER_INFO_SIZE: usize = 32;
/// Apple stores extended attributes in the Finder info entry, after the
/// Finder info and two bytes of padding, under this magic.
const ATTR_MAGIC: u32 = u32::from_be_bytes(*b"ATTR");

/// Where the companion of `name` lives: `dir/file` → `__MACOSX/dir/._file`.
/// Returns the companion's target for a companion name, `None` otherwise.
pub(crate) fn target_of(name: &str) -> Option<String> {
    let rest = name.strip_prefix("__MACOSX/")?;
    let (dir, file) = match rest.rsplit_once('/') {
        Some((dir, file)) => (Some(dir), file),
        None => (None, rest),
    };
    let file = file.strip_prefix("._")?;
    if file.is_empty() {
        return None;
    }
    Some(match dir {
        Some(dir) => format!("{dir}/{file}"),
        None => file.to_string(),
    })
}

/// Whether `name` is anything inside `__MACOSX`, companions and their folders.
pub(crate) fn is_companion_area(name: &str) -> bool {
    name == "__MACOSX" || name.starts_with("__MACOSX/")
}

fn be32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn be16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

/// The attributes in an AppleDouble file. Malformed parts are skipped: what
/// can't be read is simply not restored.
pub(crate) fn parse(data: &[u8]) -> Attributes {
    let mut attributes = Vec::new();
    if be32(data, 0) != Some(MAGIC) {
        return attributes;
    }
    let entries = be16(data, 24).unwrap_or(0) as usize;
    for i in 0..entries {
        let at = 26 + i * 12;
        let (Some(id), Some(offset), Some(length)) =
            (be32(data, at), be32(data, at + 4), be32(data, at + 8))
        else {
            break;
        };
        let (offset, length) = (offset as usize, length as usize);
        let Some(entry) = data.get(offset..offset + length) else {
            continue;
        };
        match id {
            RESOURCE_FORK if !entry.is_empty() => {
                attributes.push(("com.apple.ResourceFork".to_string(), entry.to_vec()));
            }
            FINDER_INFO => {
                let info = &entry[..entry.len().min(FINDER_INFO_SIZE)];
                if info.len() == FINDER_INFO_SIZE && info.iter().any(|&b| b != 0) {
                    attributes.push(("com.apple.FinderInfo".to_string(), info.to_vec()));
                }
                extended_attributes(data, offset + FINDER_INFO_SIZE + 2, &mut attributes);
            }
            _ => {}
        }
    }
    attributes
}

/// Reads the `ATTR` block at `at`; entry offsets are from the file's start.
fn extended_attributes(data: &[u8], at: usize, out: &mut Attributes) {
    if be32(data, at) != Some(ATTR_MAGIC) {
        return;
    }
    let count = be16(data, at + 34).unwrap_or(0) as usize;
    let mut entry = at + 36;
    for _ in 0..count {
        let (Some(offset), Some(length)) = (be32(data, entry), be32(data, entry + 4)) else {
            return;
        };
        let Some(&name_len) = data.get(entry + 10) else {
            return;
        };
        let name_len = name_len as usize;
        let Some(name) = data.get(entry + 11..entry + 11 + name_len) else {
            return;
        };
        let name = String::from_utf8_lossy(name.split(|&b| b == 0).next().unwrap_or(&[]));
        if let Some(value) = data.get(offset as usize..offset as usize + length as usize)
            // The archive's own quarantine is replaced by the archive file's.
            && !name.is_empty()
            && name != "com.apple.quarantine"
        {
            out.push((name.into_owned(), value.to_vec()));
        }
        // Entries are padded to four bytes.
        entry += (11 + name_len + 3) & !3;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn companion_names() {
        assert_eq!(
            target_of("__MACOSX/docs/._a.txt").as_deref(),
            Some("docs/a.txt")
        );
        assert_eq!(target_of("__MACOSX/._top.txt").as_deref(), Some("top.txt"));
        assert_eq!(target_of("__MACOSX/docs/a.txt"), None);
        assert_eq!(target_of("docs/._a.txt"), None);
        assert!(is_companion_area("__MACOSX/docs/"));
        assert!(!is_companion_area("__MACOSXY/a"));
    }

    #[test]
    fn rejects_other_data() {
        assert!(parse(b"not apple double").is_empty());
    }
}
