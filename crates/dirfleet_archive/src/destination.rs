use std::path::{Path, PathBuf};

use crate::archive::ArchiveHandle;

#[derive(Debug, Clone)]
pub struct DestinationPlan {
    /// Folder to pass as `ExtractRequest::output_dir`.
    pub output_dir: String,
    /// What to show in Finder afterwards: the single top-level item, or the new folder.
    pub reveal_path: String,
    /// Whether `output_dir` is a new folder named after the archive.
    pub creates_folder: bool,
}

impl ArchiveHandle {
    /// Picks where to extract inside `parent_dir` so files never land loose:
    /// an archive with a single top-level item goes straight into `parent_dir`,
    /// anything else into a new folder named after the archive.
    pub fn plan_destination(&self, parent_dir: String) -> DestinationPlan {
        let parent = PathBuf::from(parent_dir);
        let mut top_level = self.entries.iter().filter(|e| e.parent_id.is_none());
        if let (Some(only), None) = (top_level.next(), top_level.next()) {
            let target = parent.join(&only.name);
            if target.symlink_metadata().is_err() {
                return DestinationPlan {
                    output_dir: parent.display().to_string(),
                    reveal_path: target.display().to_string(),
                    creates_folder: false,
                };
            }
        }
        let folder = unique_path(&parent, &crate::names::folder_name(&self.info().name), "");
        DestinationPlan {
            output_dir: folder.display().to_string(),
            reveal_path: folder.display().to_string(),
            creates_folder: true,
        }
    }
}

/// `dir/stem.ext`, or the first of `dir/stem 2.ext`, `dir/stem 3.ext`, … that
/// does not exist yet. `ext` includes its leading dot, or is empty.
pub(crate) fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{stem}{ext}"));
    if first.symlink_metadata().is_err() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{stem} {n}{ext}")))
        .find(|p| p.symlink_metadata().is_err())
        .expect("unbounded range")
}

/// A free sibling name for `path`, keeping its extension: `a.txt` → `a 2.txt`.
pub(crate) fn unique_sibling(path: &Path) -> PathBuf {
    let dir = path.parent().unwrap_or(Path::new(""));
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let is_dir = path.is_dir();
    match name.rfind('.') {
        Some(dot) if dot > 0 && !is_dir => unique_path(dir, &name[..dot], &name[dot..]),
        _ => unique_path(dir, &name, ""),
    }
}
