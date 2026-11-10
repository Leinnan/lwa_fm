//! Path normalisation: roots are resolved through the file system once,
//! everything below them lexically.
use std::path::{Path, PathBuf};

pub trait PathHelper {
    fn to_full_path_string(&self) -> String;
}

/// Resolve `path` through the filesystem (symlinks, on-disk casing).
///
/// The result is in [`canonical_form`]. This costs a `canonicalize` syscall, so it is
/// reserved for roots: navigation targets and watch roots. Paths derived from
/// an already-normalised root (directory entries, watcher events) only need
/// the syscall-free [`canonical_form`].
#[must_use]
pub fn normalize_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).map_or_else(|_| canonical_form(path), |p| canonical_form(&p))
}

/// Lexical normalisation without syscalls.
///
/// Absolutises against the current directory, resolves `.`/`..`, unifies separators, strips the Windows
/// verbatim (`\\?\`) prefix and uppercases the drive letter. For a path below
/// a root produced by [`normalize_path`] this yields the same result as
/// [`normalize_path`] itself.
#[must_use]
pub fn canonical_form(path: &Path) -> PathBuf {
    use std::path::Component;
    let absolute;
    let path = if path.is_absolute() {
        path
    } else {
        absolute =
            std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path));
        &absolute
    };
    let mut out = PathBuf::with_capacity(path.as_os_str().len());
    let mut depth = 0usize;
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(canonical_prefix(prefix)),
            Component::RootDir => out.push(std::path::MAIN_SEPARATOR_STR),
            Component::CurDir => {}
            Component::ParentDir => {
                // Never pop past the root.
                if depth > 0 {
                    out.pop();
                    depth -= 1;
                }
            }
            Component::Normal(part) => {
                out.push(part);
                depth += 1;
            }
        }
    }
    out
}

#[cfg(windows)]
fn canonical_prefix(prefix: std::path::PrefixComponent<'_>) -> std::ffi::OsString {
    use std::path::Prefix;
    match prefix.kind() {
        Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
            format!("{}:", char::from(letter).to_ascii_uppercase()).into()
        }
        Prefix::VerbatimUNC(server, share) => {
            let mut unc = std::ffi::OsString::from(r"\\");
            unc.push(server);
            unc.push(r"\");
            unc.push(share);
            unc
        }
        _ => prefix.as_os_str().to_os_string(),
    }
}

#[cfg(not(windows))]
fn canonical_prefix(prefix: std::path::PrefixComponent<'_>) -> std::ffi::OsString {
    prefix.as_os_str().to_os_string()
}

#[must_use]
pub fn normalize_path_string(path: &Path) -> String {
    normalize_path(path).to_string_lossy().into_owned()
}

/// Syscall-free [`canonical_form`] as a `String`.
#[must_use]
pub fn canonical_string(path: &Path) -> String {
    canonical_form(path).to_string_lossy().into_owned()
}

impl<T: AsRef<Path>> PathHelper for T {
    fn to_full_path_string(&self) -> String {
        normalize_path_string(self.as_ref())
    }
}
