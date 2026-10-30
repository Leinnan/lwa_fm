//! File name rules: validation and free-name generation.

use std::path::{Path, PathBuf};

/// Splits "report.txt" into ("report", ".txt"). Folders and dot-files keep
/// the whole name as the stem.
fn split_name(name: &str, is_dir: bool) -> (&str, &str) {
    if is_dir {
        return (name, "");
    }
    match name.rfind('.') {
        Some(0) | None => (name, ""),
        Some(dot) => name.split_at(dot),
    }
}

/// The first free name in `dir` of the form `stem`, `stem (2)`, `stem (3)`…
/// with the extension kept at the end.
pub fn unique_name(dir: &Path, name: &str, is_dir: bool) -> PathBuf {
    let candidate = dir.join(name);
    if !exists(&candidate) {
        return candidate;
    }
    let (stem, ext) = split_name(name, is_dir);
    (2..u32::MAX)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|path| !exists(path))
        .unwrap_or(candidate)
}

/// The first free "stem - Copy" name in `dir`, used when an item is pasted or
/// duplicated into its own folder.
pub fn copy_name(dir: &Path, name: &str, is_dir: bool) -> PathBuf {
    let (stem, ext) = split_name(name, is_dir);
    let first = dir.join(format!("{stem} - Copy{ext}"));
    if !exists(&first) {
        return first;
    }
    (2..u32::MAX)
        .map(|n| dir.join(format!("{stem} - Copy ({n}){ext}")))
        .find(|path| !exists(path))
        .unwrap_or(first)
}

fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

/// Checks a single file name typed by the user. Returns a message that can
/// be shown next to the text field.
pub fn validate_name(name: &str) -> Result<(), &'static str> {
    if name.trim().is_empty() {
        return Err("The name can't be empty.");
    }
    if name == "." || name == ".." {
        return Err("This name is reserved.");
    }
    if name.contains(['/', '\\']) {
        return Err("The name can't contain / or \\.");
    }
    if cfg!(windows) {
        if name.contains([':', '*', '?', '"', '<', '>', '|']) || name.chars().any(char::is_control)
        {
            return Err("The name can't contain \\ / : * ? \" < > |.");
        }
        if name.ends_with(['.', ' ']) {
            return Err("The name can't end with a dot or a space.");
        }
        let stem = name.split('.').next().unwrap_or(name).trim_end();
        let reserved = [
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ];
        if reserved.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
            return Err("This name is reserved by Windows.");
        }
    } else if name.contains('\0') {
        return Err("The name can't contain a NUL character.");
    }
    Ok(())
}

/// Whether renaming `from` to the sibling name `to` only changes how the name
/// is written (letter case, Unicode composition) on a disk that treats both
/// spellings as one name, so `to` "existing" is `from` itself.
pub fn is_respelling(from: &Path, to: &Path) -> bool {
    use unicode_normalization::UnicodeNormalization;
    let fold = |path: &Path| -> Option<String> {
        let name = path.file_name()?.to_string_lossy();
        Some(name.nfc().flat_map(char::to_lowercase).collect())
    };
    // Hard links share an identity too, but have unrelated names.
    if from == to || from.parent() != to.parent() || fold(from) != fold(to) {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        from.symlink_metadata()
            .ok()
            .zip(to.symlink_metadata().ok())
            .is_some_and(|(a, b)| a.dev() == b.dev() && a.ino() == b.ino())
    }
    #[cfg(not(unix))]
    {
        true
    }
}
