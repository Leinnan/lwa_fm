//! Member names as Windows can create them. Windows refuses some characters,
//! drops dots and spaces from the end of a name without saying so, and takes
//! a few names for devices whatever folder they are in. These functions only
//! work on text, so they are tested on every system.

/// The components of the destination-relative path for a member name, or
/// `None` for the names `safe_relative_path` refuses everywhere.
pub(crate) fn components(name: &[u8], backslash: bool) -> Option<Vec<String>> {
    let separator = |b: &u8| *b == b'/' || (backslash && *b == b'\\');
    if name.contains(&0)
        || name.first().is_some_and(separator)
        || (name.len() >= 2 && name[0].is_ascii_alphabetic() && name[1] == b':')
        || name.split(separator).any(|part| part == b"..")
    {
        return None;
    }
    let parts: Vec<String> = name
        .split(separator)
        .filter(|part| !part.is_empty() && *part != b".")
        .map(|part| component(&String::from_utf8_lossy(part)))
        .collect();
    (!parts.is_empty()).then_some(parts)
}

/// The components of a symbolic link's target. `..` is kept, and both
/// slashes separate, as they will when Windows follows the link.
pub(crate) fn link_components(target: &[u8]) -> Vec<String> {
    target
        .split(|b| *b == b'/' || *b == b'\\')
        .filter(|part| !part.is_empty() && *part != b".")
        .map(|part| match part {
            b".." => "..".to_string(),
            part => component(&String::from_utf8_lossy(part)),
        })
        .collect()
}

/// One path component as a name Windows can create: forbidden characters,
/// and dots and spaces at the end, become `_`, and a device name gets a `_`
/// in front. A name that is already fine comes back unchanged, so applying
/// this twice is the same as applying it once.
pub(crate) fn component(part: &str) -> String {
    let mut name: String = part
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let kept = name.trim_end_matches(['.', ' ']).len();
    let dropped = name.len() - kept;
    name.truncate(kept);
    name.extend(std::iter::repeat_n('_', dropped));
    if is_device(&name) {
        name.insert(0, '_');
    }
    name
}

/// Whether Windows opens a device for `name`, with any extension: `NUL`,
/// `con.txt`, `COM1`, …
fn is_device(name: &str) -> bool {
    let base = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    if matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let port = base
        .strip_prefix("COM")
        .or_else(|| base.strip_prefix("LPT"));
    port.is_some_and(|number| {
        let mut digits = number.chars();
        matches!(
            (digits.next(), digits.next()),
            (Some('0'..='9' | '¹' | '²' | '³'), None)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(name: &str, backslash: bool) -> Option<String> {
        components(name.as_bytes(), backslash).map(|parts| parts.join("/"))
    }

    #[test]
    fn plain_names_are_kept() {
        assert_eq!(joined("a/b.txt", false).as_deref(), Some("a/b.txt"));
        assert_eq!(joined("./a//b/", false).as_deref(), Some("a/b"));
        assert_eq!(joined("a\\b.txt", true).as_deref(), Some("a/b.txt"));
        assert_eq!(
            joined("Łódź/.hidden", false).as_deref(),
            Some("Łódź/.hidden")
        );
        assert_eq!(
            joined("a.b.c/x y.tar.gz", false).as_deref(),
            Some("a.b.c/x y.tar.gz")
        );
    }

    #[test]
    fn unsafe_names_are_refused() {
        for name in ["../x", "a/../../x", "/etc/passwd", "C:x", "", ".", "a\0b"] {
            assert_eq!(joined(name, true), None, "{name:?}");
        }
        assert_eq!(joined("..\\x", true), None);
        assert_eq!(joined("\\x", true), None);
    }

    #[test]
    fn forbidden_characters_become_underscores() {
        assert_eq!(component("a:b.txt"), "a_b.txt");
        assert_eq!(component("q?*.txt"), "q__.txt");
        assert_eq!(component("<a>|\"b\""), "_a___b_");
        assert_eq!(component("tab\there"), "tab_here");
        // Without `backslash`, a backslash is part of the name.
        assert_eq!(joined("a\\b.txt", false).as_deref(), Some("a_b.txt"));
    }

    #[test]
    fn trailing_dots_and_spaces_become_underscores() {
        assert_eq!(component("x. "), "x__");
        assert_eq!(component("dir."), "dir_");
        assert_eq!(component("..."), "___");
        assert_eq!(component(" lead.txt"), " lead.txt");
    }

    #[test]
    fn device_names_get_a_prefix() {
        for name in [
            "CON",
            "con",
            "NUL.txt",
            "nul.tar.gz",
            "COM1",
            "lpt9.log",
            "AUX .txt",
            "COM¹",
        ] {
            let safe = component(name);
            assert!(safe.starts_with('_'), "{name:?} gave {safe:?}");
        }
        // The space at the end is replaced, which leaves no device name.
        assert_eq!(component("AUX "), "AUX_");
        for name in ["CONSOLE", "COM10", "NULL", "com", "aux1"] {
            assert_eq!(component(name), name);
        }
    }

    #[test]
    fn applying_twice_changes_nothing_more() {
        for name in ["a:b", "NUL", "x. ", "q?*", "ok.txt", "COM1.", "..."] {
            let once = component(name);
            assert_eq!(component(&once), once, "{name:?}");
        }
    }

    #[test]
    fn link_targets_keep_their_shape() {
        assert_eq!(link_components(b"../b/file"), ["..", "b", "file"]);
        assert_eq!(link_components(b"./a\\b"), ["a", "b"]);
        assert_eq!(link_components(b"a:b/NUL"), ["a_b", "_NUL"]);
    }
}
