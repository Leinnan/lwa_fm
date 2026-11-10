//! Walks folders several levels deep for searches, in parallel.
//!
//! Partial results are handed over every [`PARTIAL_INTERVAL`], so large
//! searches fill in as they go. Directory symlinks are followed once each
//! (loop protection).
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dirfleet_core::listing::{EntryType, Listing, ListingBuilder};
use rayon::Scope;

use crate::listing_io::PushRaw;
use crate::paths::{canonical_form, canonical_string, normalize_path};

/// What a deep walk found.
pub struct WalkResult {
    pub listing: Arc<Listing>,
    /// Canonical targets of the directory symlinks the walk descended into.
    /// A recursive watch on the root does not follow them, so each needs its
    /// own watch. Empty for flat reads.
    pub linked_dirs: Vec<PathBuf>,
}

/// How often a deep walk publishes partial results to the UI.
const PARTIAL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(300);

/// Shared state of one parallel recursive walk.
struct Walk<'a> {
    max_depth: usize,
    show_hidden: bool,
    cancel: &'a AtomicBool,
    /// One listing per visited directory; concatenated at the end.
    parts: std::sync::Mutex<Vec<Listing>>,
    entries: std::sync::atomic::AtomicUsize,
    /// Canonical targets of followed directory symlinks (loop protection).
    followed: std::sync::Mutex<HashSet<PathBuf>>,
    last_partial: std::sync::Mutex<std::time::Instant>,
    on_partial: &'a (dyn Fn(Arc<Listing>) + Sync),
}

impl Walk<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// List `dir` (whose entries sit at `depth`) and recurse into its
    /// sub-directories in parallel. Entry metadata, attributes and link-ness
    /// all come from the bulk directory read; the only extra syscall is the
    /// `canonicalize` that guards against symlink loops.
    fn visit<'scope>(&'scope self, scope: &Scope<'scope>, dir: &Path, depth: usize) {
        if self.cancelled() {
            return;
        }
        let dir_name: Arc<str> = Arc::from(canonical_string(dir));
        let mut builder = ListingBuilder::with_capacity(64);
        let dir_index = builder.intern_dir(&dir_name);
        let pushed = std::cell::Cell::new(0usize);
        let mut sink = |entry: &crate::fs_enum::RawEntry<'_>| {
            if !self.show_hidden
                && (entry.is_hidden_attr()
                    || entry.name.starts_with_ascii(b'.')
                    || (cfg!(windows) && entry.name.starts_with_ascii(b'$')))
            {
                return;
            }
            let meta = entry.metadata();
            if meta.entry_type == EntryType::Directory && depth < self.max_depth {
                let child = dir.join(entry.name.to_os_string());
                if self.should_descend(&child, entry.is_symlink()) {
                    scope.spawn(move |scope| self.visit(scope, &child, depth + 1));
                }
            }
            PushRaw::push_raw(&mut builder, dir_index, &entry.name, meta);
            pushed.set(pushed.get() + 1);
        };
        // Fall back to `read_dir` only when the bulk read produced nothing,
        // so a mid-way failure never lists a directory twice.
        if crate::fs_enum::enumerate(dir, &mut sink).is_err()
            && pushed.get() == 0
            && crate::fs_enum::enumerate_std(dir, &mut sink).is_err()
        {
            return;
        }
        drop(sink);
        let listing = builder.finish();
        if listing.is_empty() {
            return;
        }
        self.entries.fetch_add(listing.len(), Ordering::Relaxed);
        if let Ok(mut parts) = self.parts.lock() {
            parts.push(listing);
        }
        self.maybe_publish();
    }

    fn should_descend(&self, path: &Path, is_symlink: bool) -> bool {
        if !is_symlink {
            return true;
        }
        let Ok(target) = std::fs::canonicalize(path) else {
            return false;
        };
        self.followed
            .lock()
            .is_ok_and(|mut followed| followed.insert(target))
    }

    fn maybe_publish(&self) {
        let Ok(mut last) = self.last_partial.try_lock() else {
            return;
        };
        if last.elapsed() < PARTIAL_INTERVAL {
            return;
        }
        *last = std::time::Instant::now();
        drop(last);
        let snapshot = self.snapshot();
        (self.on_partial)(Arc::new(snapshot));
    }

    fn snapshot(&self) -> Listing {
        self.parts
            .lock()
            .map(|parts| Listing::concat(parts.iter()))
            .unwrap_or_default()
    }
}

/// Walks `roots` `depth` levels below each (1 lists the roots' children
/// only), calling `on_partial` with what was found so far every
/// [`PARTIAL_INTERVAL`]. Returns an empty listing when `cancel` is set.
pub fn walk(
    roots: &[PathBuf],
    depth: usize,
    show_hidden: bool,
    cancel: &AtomicBool,
    on_partial: &(dyn Fn(Arc<Listing>) + Sync),
) -> WalkResult {
    // Normalise each root once; entry paths below it are then derived
    // lexically instead of paying a `canonicalize` per entry.
    let mut roots: Vec<PathBuf> = roots.iter().map(|path| normalize_path(path)).collect();
    roots.sort();
    roots.dedup();
    // A root nested inside another root is already covered by its walk.
    let roots: Vec<PathBuf> = roots
        .iter()
        .filter(|root| {
            !roots
                .iter()
                .any(|other| other != *root && root.starts_with(other))
        })
        .cloned()
        .collect();

    let walk = Walk {
        // `depth` counts levels below the root, matching the former
        // `walkdir` `max_depth(depth + 1)`.
        max_depth: depth + 1,
        show_hidden,
        cancel,
        parts: std::sync::Mutex::new(Vec::new()),
        entries: std::sync::atomic::AtomicUsize::new(0),
        followed: std::sync::Mutex::new(HashSet::default()),
        last_partial: std::sync::Mutex::new(std::time::Instant::now()),
        on_partial,
    };
    rayon::scope(|scope| {
        for root in &roots {
            let walk = &walk;
            let root = root.clone();
            scope.spawn(move |scope| walk.visit(scope, &root, 1));
        }
    });
    if walk.cancelled() {
        return WalkResult {
            listing: Arc::default(),
            linked_dirs: Vec::new(),
        };
    }
    let listing = Arc::new(walk.snapshot());
    let mut linked_dirs: Vec<PathBuf> = walk
        .followed
        .into_inner()
        .unwrap_or_default()
        .iter()
        .map(|target| canonical_form(target))
        .collect();
    linked_dirs.sort();
    linked_dirs.dedup();
    WalkResult {
        listing,
        linked_dirs,
    }
}
