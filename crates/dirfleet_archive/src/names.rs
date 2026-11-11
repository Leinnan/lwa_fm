//! Member names: destination-safe relative paths and legacy code pages.

#[cfg(not(windows))]
use std::ffi::OsStr;
#[cfg(not(windows))]
use std::os::unix::ffi::OsStrExt;
#[cfg(not(windows))]
use std::path::Component;
use std::path::{Path, PathBuf};

#[cfg(any(windows, test))]
mod windows;

/// Code page of names in archives made before Unicode names were common.
/// The tables come from rars and serve every format.
pub(crate) use rars::filename::LegacyNameEncoding as Encoding;

/// The destination-relative path for a member name, or `None` when the name
/// is absolute, has a drive letter, climbs out with `..`, contains NUL, or is
/// empty. With `backslash`, `\` separates components as it does in archives
/// written on DOS and Windows.
///
/// On Windows the components are also changed into names Windows can
/// create; see [`windows::component`].
#[cfg(not(windows))]
pub(crate) fn safe_relative_path(name: &[u8], backslash: bool) -> Option<PathBuf> {
    let separator = |b: &u8| *b == b'/' || (backslash && *b == b'\\');
    if name.contains(&0)
        || name.first().is_some_and(separator)
        || (name.len() >= 2 && name[0].is_ascii_alphabetic() && name[1] == b':')
        || name.split(separator).any(|part| part == b"..")
    {
        return None;
    }
    let normalized: Vec<u8> = name
        .iter()
        .map(|&b| if backslash && b == b'\\' { b'/' } else { b })
        .collect();
    let mut out = PathBuf::new();
    for component in Path::new(OsStr::from_bytes(&normalized)).components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

#[cfg(windows)]
pub(crate) fn safe_relative_path(name: &[u8], backslash: bool) -> Option<PathBuf> {
    windows::components(name, backslash).map(PathBuf::from_iter)
}

/// A stored link target as the path to link to: its components made into
/// names Windows can create, as the members it points at were.
#[cfg(windows)]
pub(crate) fn link_target(target: &[u8]) -> PathBuf {
    windows::link_components(target).into_iter().collect()
}

/// A destination-relative path as shown to the user and reported to
/// callers: with `/` between components on every system.
pub(crate) fn display_rel(path: &Path) -> String {
    #[cfg(windows)]
    return path.to_string_lossy().replace('\\', "/");
    #[cfg(not(windows))]
    path.to_string_lossy().into_owned()
}

/// The path for [`display_rel`]'s form of it.
pub(crate) fn native_rel(shown: &str) -> PathBuf {
    #[cfg(windows)]
    let shown = shown.replace('/', "\\");
    PathBuf::from(shown)
}

/// A name for a new folder made from `name`, which is not a member name.
pub(crate) fn folder_name(name: &str) -> String {
    #[cfg(windows)]
    let name = &windows::component(name);
    name.to_string()
}

/// UTF-8 bytes for a name: kept when already valid, otherwise decoded from
/// `encoding`, falling back to lossy UTF-8. APFS only accepts UTF-8 names.
pub(crate) fn decode(raw: &[u8], encoding: Encoding) -> Vec<u8> {
    if std::str::from_utf8(raw).is_ok() {
        return raw.to_vec();
    }
    encoding
        .decode(raw)
        .unwrap_or_else(|_| String::from_utf8_lossy(raw).into_owned())
        .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(name: &str, backslash: bool) -> Option<String> {
        safe_relative_path(name.as_bytes(), backslash).map(|p| display_rel(&p))
    }

    #[test]
    fn safe_paths() {
        assert_eq!(rel("a/b.txt", false).as_deref(), Some("a/b.txt"));
        assert_eq!(rel("./a//b/", false).as_deref(), Some("a/b"));
        assert_eq!(rel("a\\b.txt", true).as_deref(), Some("a/b.txt"));
        // Windows can't have a backslash in a name.
        #[cfg(not(windows))]
        assert_eq!(rel("a\\b.txt", false).as_deref(), Some("a\\b.txt"));
        #[cfg(windows)]
        assert_eq!(rel("a\\b.txt", false).as_deref(), Some("a_b.txt"));
    }

    #[test]
    fn unsafe_paths() {
        for name in [
            "../x",
            "a/../../x",
            "/etc/passwd",
            "C:x",
            "",
            ".",
            "./",
            "a\0b",
        ] {
            assert_eq!(rel(name, true), None, "{name:?}");
        }
        assert_eq!(rel("..\\x", true), None);
        assert_eq!(rel("\\x", true), None);
    }

    #[test]
    fn legacy_names() {
        assert_eq!(decode(b"plain", Encoding::Cp852), b"plain");
        assert_eq!(decode(b"\x9d\xa2d\xab", Encoding::Cp852), "Łódź".as_bytes());
    }
}
