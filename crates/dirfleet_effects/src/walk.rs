//! Walks folders several levels deep for searches, in parallel.
//!
//! Partial results are handed over every [`PARTIAL_INTERVAL`], so large
//! searches fill in as they go. Directory symlinks are followed once each
//! (loop protection).
//!
//! A walk can also look inside the files it finds ([`ContentScan`]): it then
//! hands over only the files that match, each with its matching line. And
//! it can leave out what `.gitignore` files ignore.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use dirfleet_core::app::MatchLine;
use dirfleet_core::listing::{EntryType, Listing, ListingBuilder};
use ignore::Match;
use ignore::gitignore::Gitignore;
use rayon::Scope;

use crate::content::ContentScan;
use crate::listing_io::PushRaw;
use crate::paths::{canonical_form, canonical_string, normalize_path};

/// What a deep walk found.
pub struct WalkResult {
    pub listing: Arc<Listing>,
    /// Canonical targets of the directory symlinks the walk descended into.
    /// A recursive watch on the root does not follow them, so each needs its
    /// own watch. Empty for flat reads.
    pub linked_dirs: Vec<PathBuf>,
    /// The matching line of each entry of `listing`, when file contents
    /// were searched; empty otherwise.
    pub matched: Arc<Vec<MatchLine>>,
    /// How many files were opened to look inside.
    pub scanned: u64,
}

/// What a walk found so far.
pub struct Found {
    pub listing: Arc<Listing>,
    /// As [`WalkResult::matched`].
    pub matched: Arc<Vec<MatchLine>>,
    pub scanned: u64,
}

/// What a walk does besides listing.
#[derive(Default, Clone, Copy)]
pub struct WalkOptions<'a> {
    /// Look inside files, and keep only the files that match.
    pub content: Option<&'a ContentScan>,
    /// Leave out what `.gitignore` files ignore, and `.git` folders.
    pub skip_ignored: bool,
    /// What is found will be read: links are not followed, since a file
    /// reached twice would pass for two, and files kept online are left
    /// out, since reading them would fetch them.
    pub for_reading: bool,
    /// Links are left out, to files and to folders alike: what is found is
    /// told apart by its path, and a link would stand for what it points
    /// at.
    pub skip_links: bool,
    /// Told how many entries were found so far, in place of `on_partial`
    /// and its listing of them all.
    pub on_count: Option<&'a (dyn Fn(usize) + Sync)>,
}

/// `FILE_ATTRIBUTE_OFFLINE`, `_RECALL_ON_OPEN` and `_RECALL_ON_DATA_ACCESS`:
/// the file's bytes are kept elsewhere and come only when asked for.
pub(crate) const ATTR_KEPT_ONLINE: u32 = 0x1000 | 0x4_0000 | 0x40_0000;

/// The `.gitignore` files that hold for a folder: the nearest, then the
/// ones above it.
struct Ignores {
    own: Gitignore,
    above: Option<Arc<Self>>,
}

impl Ignores {
    /// `above` with `dir`'s own `.gitignore` before it, when it has one.
    fn with(dir: &Path, above: Option<Arc<Self>>) -> Option<Arc<Self>> {
        let file = dir.join(".gitignore");
        if !file.is_file() {
            return above;
        }
        // Lines that can't be read are skipped; the rest hold.
        let (own, _) = Gitignore::new(&file);
        Some(Arc::new(Self { own, above }))
    }

    /// The files that hold for `root` from above it: those up to the top of
    /// the repository it is in. Outside a repository, none.
    fn above(root: &Path) -> Option<Arc<Self>> {
        if root.join(".git").exists() {
            return None;
        }
        let mut dirs = Vec::new();
        let mut inside = false;
        for dir in root.ancestors().skip(1) {
            dirs.push(dir);
            if dir.join(".git").exists() {
                inside = true;
                break;
            }
        }
        if !inside {
            return None;
        }
        dirs.iter()
            .rev()
            .fold(None, |above, dir| Self::with(dir, above))
    }

    /// Whether `path` is ignored. The nearest file that names it decides.
    fn ignored(&self, path: &Path, is_dir: bool) -> bool {
        let mut level = Some(self);
        while let Some(ignores) = level {
            match ignores.own.matched(path, is_dir) {
                Match::Ignore(_) => return true,
                Match::Whitelist(_) => return false,
                Match::None => level = ignores.above.as_deref(),
            }
        }
        false
    }
}

/// How often a deep walk publishes partial results to the UI.
const PARTIAL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(300);

/// Shared state of one parallel recursive walk.
struct Walk<'a> {
    max_depth: usize,
    show_hidden: bool,
    cancel: &'a AtomicBool,
    options: WalkOptions<'a>,
    /// One listing per visited directory, with its entries' matching lines
    /// when contents are searched; concatenated at the end.
    parts: std::sync::Mutex<Vec<(Listing, Vec<MatchLine>)>>,
    entries: std::sync::atomic::AtomicUsize,
    scanned: AtomicU64,
    /// Canonical targets of followed directory symlinks (loop protection).
    followed: std::sync::Mutex<HashSet<PathBuf>>,
    last_partial: std::sync::Mutex<std::time::Instant>,
    on_partial: &'a (dyn Fn(Found) + Sync),
}

impl Walk<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// List `dir` (whose entries sit at `depth`) and recurse into its
    /// sub-directories in parallel. Entry metadata, attributes and link-ness
    /// all come from the bulk directory read; the only extra syscall is the
    /// `canonicalize` that guards against symlink loops.
    fn visit<'scope>(
        &'scope self,
        scope: &Scope<'scope>,
        dir: &Path,
        depth: usize,
        above: Option<Arc<Ignores>>,
    ) {
        if self.cancelled() {
            return;
        }
        // The folder's own `.gitignore` is read before its entries are.
        let ignores = if self.options.skip_ignored {
            Ignores::with(dir, above)
        } else {
            None
        };
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
            let is_dir = meta.entry_type == EntryType::Directory;
            let reading = self.options.for_reading;
            if ((reading || self.options.skip_links) && entry.is_symlink())
                || (reading && !is_dir && entry.attributes & ATTR_KEPT_ONLINE != 0)
            {
                return;
            }
            if self.options.skip_ignored {
                let name = entry.name.to_os_string();
                if (is_dir && name == ".git")
                    || ignores
                        .as_ref()
                        .is_some_and(|ignores| ignores.ignored(&dir.join(&name), is_dir))
                {
                    return;
                }
            }
            if is_dir && depth < self.max_depth {
                let child = dir.join(entry.name.to_os_string());
                if self.should_descend(&child, entry.is_symlink()) {
                    let ignores = ignores.clone();
                    scope.spawn(move |scope| self.visit(scope, &child, depth + 1, ignores));
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
        // Looking inside files keeps only the files that match.
        let (listing, matched) = match self.options.content {
            Some(scan) => scan.keep(dir, &dir_name, &listing, self.cancel, &self.scanned),
            None => (listing, Vec::new()),
        };
        if listing.is_empty() {
            return;
        }
        self.entries.fetch_add(listing.len(), Ordering::Relaxed);
        if let Ok(mut parts) = self.parts.lock() {
            parts.push((listing, matched));
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
        if let Some(on_count) = self.options.on_count {
            on_count(self.entries.load(Ordering::Relaxed));
            return;
        }
        (self.on_partial)(self.snapshot());
    }

    fn snapshot(&self) -> Found {
        let (listing, matched) = self
            .parts
            .lock()
            .map(|parts| {
                let listing = Listing::concat(parts.iter().map(|(listing, _)| listing));
                let matched = if self.options.content.is_some() {
                    parts
                        .iter()
                        .flat_map(|(_, matched)| matched.iter().cloned())
                        .collect()
                } else {
                    Vec::new()
                };
                (listing, matched)
            })
            .unwrap_or_default();
        Found {
            listing: Arc::new(listing),
            matched: Arc::new(matched),
            scanned: self.scanned.load(Ordering::Relaxed),
        }
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
    options: WalkOptions<'_>,
    on_partial: &(dyn Fn(Found) + Sync),
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
        options,
        parts: std::sync::Mutex::new(Vec::new()),
        entries: std::sync::atomic::AtomicUsize::new(0),
        scanned: AtomicU64::new(0),
        followed: std::sync::Mutex::new(HashSet::default()),
        last_partial: std::sync::Mutex::new(std::time::Instant::now()),
        on_partial,
    };
    rayon::scope(|scope| {
        for root in &roots {
            let walk = &walk;
            let root = root.clone();
            let above = if options.skip_ignored {
                Ignores::above(&root)
            } else {
                None
            };
            scope.spawn(move |scope| walk.visit(scope, &root, 1, above));
        }
    });
    if walk.cancelled() {
        return WalkResult {
            listing: Arc::default(),
            linked_dirs: Vec::new(),
            matched: Arc::default(),
            scanned: 0,
        };
    }
    let found = walk.snapshot();
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
        listing: found.listing,
        linked_dirs,
        matched: found.matched,
        scanned: found.scanned,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dirfleet_core::search::Search;

    fn names(listing: &Listing) -> Vec<String> {
        let mut names: Vec<String> = listing
            .iter()
            .map(|entry| entry.get_splitted_path().1.to_owned())
            .collect();
        names.sort();
        names
    }

    fn walked(root: &Path, options: WalkOptions<'_>, show_hidden: bool) -> WalkResult {
        let cancel = AtomicBool::new(false);
        walk(
            &[root.to_path_buf()],
            4,
            show_hidden,
            &cancel,
            options,
            &|_| {},
        )
    }

    #[test]
    fn a_walk_that_looks_inside_files_hands_over_only_the_files_that_match() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("src/deep")).expect("folders");
        std::fs::write(dir.path().join("readme.md"), "intro\nTODO: write\n").expect("write");
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").expect("write");
        std::fs::write(dir.path().join("src/deep/lib.rs"), "// todo later\n").expect("write");
        std::fs::write(dir.path().join("src/todo.txt"), "nothing here\n").expect("write");
        let mut search = Search {
            value: "content:todo".to_owned(),
            ..Search::default()
        };
        search.commit_text();
        let scan = ContentScan::new(&search, 0).expect("looks inside files");
        let options = WalkOptions {
            content: Some(&scan),
            ..WalkOptions::default()
        };

        let found = walked(dir.path(), options, false);
        // No folders, and no file only its name says "todo".
        assert_eq!(names(&found.listing), ["lib.rs", "readme.md"]);
        assert_eq!(found.scanned, 4);
        // Each entry has its line, in the listing's own order.
        assert_eq!(found.matched.len(), found.listing.len());
        for (entry, line) in found.listing.iter().zip(found.matched.iter()) {
            let expected = match entry.get_splitted_path().1 {
                "readme.md" => (2, "TODO: write"),
                _ => (1, "// todo later"),
            };
            assert_eq!((line.line, line.text.as_str()), expected);
        }
        // An ordinary walk carries no lines.
        let plain = walked(dir.path(), WalkOptions::default(), false);
        assert_eq!(plain.listing.len(), 6);
        assert!(plain.matched.is_empty());
    }

    #[test]
    fn what_gitignore_files_ignore_is_left_out_of_a_walk() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().join("repo");
        for folder in [".git", "target", "sub"] {
            std::fs::create_dir_all(root.join(folder)).expect("folders");
        }
        std::fs::write(root.join(".gitignore"), "target/\n*.log\n!keep.log\n").expect("write");
        std::fs::write(root.join("sub/.gitignore"), "secret.txt\n").expect("write");
        for file in [
            ".git/config",
            "a.txt",
            "x.log",
            "keep.log",
            "target/out.txt",
            "sub/secret.txt",
            "sub/b.txt",
            "sub/y.log",
        ] {
            std::fs::write(root.join(file), "x").expect("write");
        }
        let skipping = WalkOptions {
            skip_ignored: true,
            ..WalkOptions::default()
        };

        let found = walked(&root, skipping, true);
        assert_eq!(
            names(&found.listing),
            [
                ".gitignore",
                ".gitignore",
                "a.txt",
                "b.txt",
                "keep.log",
                "sub"
            ]
        );
        // A folder inside the repository is held to the files above it too.
        let below = walked(&root.join("sub"), skipping, true);
        assert_eq!(names(&below.listing), [".gitignore", "b.txt"]);
        // Without the option everything is there.
        let all = walked(&root, WalkOptions::default(), true);
        assert!(names(&all.listing).contains(&"x.log".to_owned()));
        assert!(names(&all.listing).contains(&"config".to_owned()));
    }
}
