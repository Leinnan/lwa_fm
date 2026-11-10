//! Whether a plugin's activation holds in a folder, which means looking at
//! what the folder (or a parent) contains.
use std::path::{Path, PathBuf};

use dirfleet_core::app::{ActivatedPlugins, CheckActivation};
use dirfleet_core::plugins::manifest::Activation;

/// The core's check: which of the plugins' activations hold in the folder.
pub fn activated(check: &CheckActivation) -> ActivatedPlugins {
    let dir = Path::new(&check.dir);
    ActivatedPlugins(
        check
            .plugins
            .iter()
            .filter(|(_, activation)| activation.matches(dir))
            .map(|(id, _)| id.clone())
            .collect(),
    )
}

/// [`Activation`]'s checks against the file system.
pub trait ActivationExt {
    /// Whether the plugin applies to `dir`. No conditions means everywhere.
    fn matches(&self, dir: &Path) -> bool;
    /// The folder of `dir` (or a parent) that made it match, for
    /// `ancestor_contains`; `dir` itself otherwise.
    fn root_for(&self, dir: &Path) -> PathBuf;
}

impl ActivationExt for Activation {
    fn matches(&self, dir: &Path) -> bool {
        if self.everywhere() {
            return true;
        }
        if self.contains_any.iter().any(|name| dir.join(name).exists()) {
            return true;
        }
        if dir.ancestors().any(|ancestor| {
            self.ancestor_contains
                .iter()
                .any(|name| ancestor.join(name).exists())
        }) {
            return true;
        }
        self.glob.as_ref().is_some_and(|pattern| {
            glob::Pattern::new(pattern).is_ok_and(|pattern| {
                std::fs::read_dir(dir).is_ok_and(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .any(|entry| pattern.matches(&entry.file_name().to_string_lossy()))
                })
            })
        })
    }

    fn root_for(&self, dir: &Path) -> PathBuf {
        dir.ancestors()
            .find(|ancestor| {
                self.ancestor_contains
                    .iter()
                    .any(|name| ancestor.join(name).exists())
            })
            .unwrap_or(dir)
            .to_path_buf()
    }
}
