//! File system changes shared by the handlers.
use std::io;
use std::path::Path;

/// Removes a file, a link or a whole folder for good.
///
/// # Errors
/// When the item can't be read or removed.
pub fn remove_any(path: &Path) -> io::Result<()> {
    let meta = path.symlink_metadata()?;
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}
