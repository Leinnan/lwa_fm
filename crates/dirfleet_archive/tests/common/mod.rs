//! Helpers shared by the format test suites.

#![allow(dead_code)]

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use dirfleet_archive::{
    ArchiveHandle, CancelToken, ConflictPolicy, CoreError, ExtractRequest, ExtractSummary,
    ProgressObserver,
};

pub fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
        .display()
        .to_string()
}

#[derive(Default)]
pub struct Recorder {
    pub calls: Mutex<Vec<(u64, u64)>>,
    pub cancel_on_first: Option<Arc<CancelToken>>,
}

impl ProgressObserver for Recorder {
    fn on_progress(&self, done: u64, total: u64, _file: String) {
        self.calls.lock().unwrap().push((done, total));
        if let Some(token) = &self.cancel_on_first {
            token.cancel();
        }
    }
}

pub fn request(output: &Path) -> ExtractRequest {
    ExtractRequest {
        output_dir: output.display().to_string(),
        selection: None,
        password: None,
        conflict: ConflictPolicy::Overwrite,
        restore_metadata: true,
    }
}

pub fn extract(
    handle: &ArchiveHandle,
    request: ExtractRequest,
) -> Result<ExtractSummary, CoreError> {
    handle.extract(request, Arc::new(Recorder::default()), CancelToken::new())
}

pub fn id_of(handle: &ArchiveHandle, path: &str) -> u32 {
    handle.entries().iter().find(|e| e.path == path).unwrap().id
}

/// Every file below `dir`, relative, sorted.
pub fn tree(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(root).unwrap().display().to_string();
            #[cfg(windows)]
            let rel = rel.replace('\\', "/");
            if path.is_dir() && !path.is_symlink() {
                out.push(format!("{rel}/"));
                walk(root, &path, out);
            } else {
                out.push(rel);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}
