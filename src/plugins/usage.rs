//! How much disk a folder really takes: `dirfleet.fs.usage` and the inspector.
//!
//! Adding up file lengths overstates a Cargo `target` about twofold, since
//! Cargo hardlinks its outputs. This counts allocated blocks and each file
//! once, staying on one filesystem and not following symlinks, as `du` does.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub bytes: u64,
    /// What deleting the folder would free: `bytes`, less files that also
    /// have links outside it. Incremental caches share many with `deps`.
    pub freed: u64,
    pub files: u64,
    /// Newest file's modification time, seconds since 1970, outside the
    /// skipped folders.
    pub newest: Option<i64>,
    /// Entries that could not be read.
    pub errors: u64,
    /// `bytes` of the root's folders named in [`Options::subtotals`].
    pub parts: BTreeMap<String, u64>,
}

/// What a walk reports besides the totals.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Folder names whose files don't count towards `newest`, wherever they are.
    pub newest_skip: Vec<String>,
    /// Folders directly under the root to add up separately, in the same
    /// walk: walking `target` again would take most of the time twice.
    pub subtotals: Vec<String>,
}

/// A file with several links: how many were met so far, how many it has and
/// its size on disk.
pub(super) struct Linked {
    met: u64,
    links: u64,
    bytes: u64,
}

pub(super) type LinkedFiles = HashMap<(u64, u64), Linked>;

/// Notes a file with several links. Returns whether it is the first meeting,
/// the only one that counts towards the total.
#[cfg(unix)]
pub(super) fn first_link(linked: &mut LinkedFiles, id: (u64, u64), links: u64, bytes: u64) -> bool {
    use std::collections::hash_map::Entry;
    match linked.entry(id) {
        Entry::Occupied(mut entry) => {
            entry.get_mut().met += 1;
            false
        }
        Entry::Vacant(entry) => {
            entry.insert(Linked {
                met: 1,
                links,
                bytes,
            });
            true
        }
    }
}

/// `bytes`, less the files that also have links outside the walk.
pub(super) fn freed(bytes: u64, linked: &LinkedFiles) -> u64 {
    let kept: u64 = linked
        .values()
        .filter(|file| file.met < file.links)
        .map(|file| file.bytes)
        .sum();
    bytes.saturating_sub(kept)
}

/// A file's share of the total, or `None` when it was already counted.
#[cfg(unix)]
fn disk_bytes(metadata: &std::fs::Metadata, linked: &mut LinkedFiles) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    let bytes = metadata.blocks() * 512;
    // Only linked files can repeat, so most files never enter the map.
    if metadata.is_file()
        && metadata.nlink() > 1
        && !first_link(
            linked,
            (metadata.dev(), metadata.ino()),
            metadata.nlink(),
            bytes,
        )
    {
        return None;
    }
    Some(bytes)
}

/// Stable Rust has no file identity here, so hardlinks count once each.
#[cfg(not(unix))]
fn disk_bytes(metadata: &std::fs::Metadata, _linked: &mut LinkedFiles) -> Option<u64> {
    Some(if metadata.is_file() {
        metadata.len()
    } else {
        0
    })
}

/// Walks `root`. `Ok(None)` means `cancel` was set.
pub fn measure(
    root: &Path,
    options: &Options,
    cancel: &AtomicBool,
) -> std::io::Result<Option<Usage>> {
    #[cfg(target_os = "macos")]
    if let Some(usage) = super::usage_bulk::measure(root, options, cancel) {
        return usage;
    }
    walk(root, options, cancel)
}

/// The walk for every platform, with a system call per file.
fn walk(root: &Path, options: &Options, cancel: &AtomicBool) -> std::io::Result<Option<Usage>> {
    std::fs::symlink_metadata(root)?;
    let mut usage = Usage::default();
    let mut linked = HashMap::new();
    // The depth of the skipped folder being walked: its contents come
    // directly after it, and are all deeper.
    let mut skipping: Option<usize> = None;
    // The subtotal being added to, while inside one of the root's folders.
    let mut part: Option<&str> = None;
    let walk = walkdir::WalkDir::new(root)
        .follow_root_links(false)
        .same_file_system(true);
    for entry in walk {
        if cancel.load(Ordering::Acquire) {
            return Ok(None);
        }
        let Ok(entry) = entry else {
            usage.errors += 1;
            continue;
        };
        let Ok(metadata) = entry.metadata() else {
            usage.errors += 1;
            continue;
        };
        if skipping.is_some_and(|depth| entry.depth() <= depth) {
            skipping = None;
        }
        let name = || entry.file_name().to_str();
        if skipping.is_none()
            && metadata.is_dir()
            && entry.depth() > 0
            && options
                .newest_skip
                .iter()
                .any(|skipped| Some(skipped.as_str()) == name())
        {
            skipping = Some(entry.depth());
        }
        if entry.depth() == 1 {
            part = options
                .subtotals
                .iter()
                .find(|subtotal| metadata.is_dir() && Some(subtotal.as_str()) == name())
                .map(String::as_str);
        }
        let Some(bytes) = disk_bytes(&metadata, &mut linked) else {
            continue;
        };
        usage.bytes += bytes;
        if let Some(part) = part {
            *usage.parts.entry(part.to_owned()).or_default() += bytes;
        }
        if metadata.is_dir() {
            continue;
        }
        usage.files += 1;
        if skipping.is_none()
            && metadata.is_file()
            && let Ok(duration) = metadata.modified().and_then(|time| {
                time.duration_since(std::time::UNIX_EPOCH)
                    .map_err(std::io::Error::other)
            })
        {
            let modified = duration.as_secs().cast_signed();
            usage.newest = Some(usage.newest.map_or(modified, |newest| newest.max(modified)));
        }
    }
    usage.freed = freed(usage.bytes, &linked);
    Ok(Some(usage))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dirfleet_usage_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn run(root: &Path, skip: &[&str]) -> Usage {
        let options = Options {
            newest_skip: skip.iter().map(ToString::to_string).collect(),
            ..Default::default()
        };
        measure(root, &options, &AtomicBool::new(false))
            .expect("walks")
            .expect("not cancelled")
    }

    #[cfg(unix)]
    #[test]
    fn hardlinks_count_once_and_symlinks_are_not_walked() {
        let dir = temp_dir("links");
        std::fs::write(dir.join("a.bin"), vec![1u8; 1 << 20]).expect("writes");
        let single = run(&dir, &[]);
        assert_eq!(single.files, 1);
        assert!(single.bytes >= 1 << 20, "{single:?}");

        std::fs::hard_link(dir.join("a.bin"), dir.join("b.bin")).expect("links");
        let linked = run(&dir, &[]);
        assert_eq!(linked.files, 1, "a linked file counts once");
        assert!(
            linked.bytes < single.bytes + (1 << 19),
            "{single:?} {linked:?}"
        );

        let outside = temp_dir("links_outside");
        std::fs::write(outside.join("big.bin"), vec![1u8; 1 << 20]).expect("writes");
        std::os::unix::fs::symlink(&outside, dir.join("elsewhere")).expect("links");
        let with_symlink = run(&dir, &[]);
        assert!(
            with_symlink.bytes < linked.bytes + (1 << 19),
            "the link's target isn't counted: {linked:?} {with_symlink:?}"
        );
        // A symlink as the root is the link itself, as with `du`.
        assert_eq!(run(&dir.join("elsewhere"), &[]).files, 1);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn freed_leaves_out_files_linked_from_outside() {
        let dir = temp_dir("freed");
        let inside = dir.join("inside");
        std::fs::create_dir_all(&inside).expect("creates");
        std::fs::write(inside.join("own.bin"), vec![1u8; 1 << 20]).expect("writes");
        std::fs::write(inside.join("shared.bin"), vec![1u8; 1 << 20]).expect("writes");
        std::fs::hard_link(inside.join("own.bin"), inside.join("own_again.bin")).expect("links");

        let alone = run(&inside, &[]);
        assert_eq!(alone.freed, alone.bytes, "all links are inside");

        std::fs::hard_link(inside.join("shared.bin"), dir.join("outside.bin")).expect("links");
        let shared = run(&inside, &[]);
        assert_eq!(shared.bytes, alone.bytes, "still takes the same space");
        assert!(
            shared.freed <= shared.bytes - (1 << 20),
            "deleting wouldn't free the shared file: {shared:?}"
        );
        // From the parent both links are met, so all of it would go.
        let parent = run(&dir, &[]);
        assert_eq!(parent.freed, parent.bytes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn newest_leaves_out_the_skipped_folders() {
        let dir = temp_dir("newest");
        std::fs::create_dir_all(dir.join("target/debug")).expect("creates");
        std::fs::create_dir_all(dir.join("src")).expect("creates");
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        let new = std::time::UNIX_EPOCH + std::time::Duration::from_secs(2_000_000);
        let write = |path: &str, time| {
            let file = std::fs::File::create(dir.join(path)).expect("creates");
            file.set_modified(time).expect("sets the time");
        };
        write("src/main.rs", old);
        write("target/debug/app", new);
        write("zzz.txt", old);

        assert_eq!(run(&dir, &[]).newest, Some(2_000_000));
        let usage = run(&dir, &["target"]);
        assert_eq!(usage.newest, Some(1_000_000));
        assert_eq!(
            usage.files, 3,
            "skipped folders still count towards the size"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn subtotals_come_from_the_same_walk() {
        let dir = temp_dir("subtotals");
        std::fs::create_dir_all(dir.join("target/debug")).expect("creates");
        std::fs::create_dir_all(dir.join("src/target")).expect("creates");
        std::fs::write(dir.join("target/debug/app"), vec![1u8; 1 << 20]).expect("writes");
        std::fs::write(dir.join("src/target/nested"), vec![1u8; 1 << 19]).expect("writes");
        std::fs::write(dir.join("zzz"), vec![1u8; 1 << 18]).expect("writes");
        let options = Options {
            subtotals: vec!["target".into(), "missing".into()],
            ..Default::default()
        };
        let usage = measure(&dir, &options, &AtomicBool::new(false))
            .expect("walks")
            .expect("not cancelled");
        let target = run(&dir.join("target"), &[]);
        assert_eq!(usage.parts.get("target"), Some(&target.bytes));
        assert_eq!(usage.parts.get("missing"), None);
        assert!(usage.bytes > target.bytes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Whichever walker runs, the numbers must be the same.
    #[cfg(target_os = "macos")]
    #[test]
    fn batched_walker_matches_the_plain_one() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = temp_dir("batched");
        let outside = temp_dir("batched_outside");
        std::fs::create_dir_all(dir.join("target/debug/deps")).expect("creates");
        std::fs::create_dir_all(dir.join("src/nested/deep")).expect("creates");
        std::fs::create_dir_all(dir.join("empty")).expect("creates");
        for index in 0..3000 {
            let path = dir.join(format!("target/debug/deps/file_{index}.rlib"));
            std::fs::write(path, vec![1u8; 100 + index]).expect("writes");
        }
        for name in [
            "src/main.rs",
            "src/nested/deep/a b.txt",
            "src/nested/żółw.rs",
        ] {
            std::fs::write(dir.join(name), vec![2u8; 70_000]).expect("writes");
        }
        std::fs::File::create(dir.join("src/old.rs"))
            .expect("creates")
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000))
            .expect("sets the time");
        std::fs::hard_link(
            dir.join("target/debug/deps/file_7.rlib"),
            dir.join("target/debug/app"),
        )
        .expect("links");
        std::fs::write(dir.join("src/shared.bin"), vec![3u8; 1 << 20]).expect("writes");
        std::fs::hard_link(dir.join("src/shared.bin"), outside.join("link.bin")).expect("links");
        std::os::unix::fs::symlink(&outside, dir.join("elsewhere")).expect("links");
        std::os::unix::fs::symlink("nowhere", dir.join("src/dangling")).expect("links");
        let locked = dir.join("locked");
        std::fs::create_dir_all(&locked).expect("creates");
        std::fs::write(locked.join("hidden"), "x").expect("writes");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("locks");

        let options = Options {
            newest_skip: vec!["target".into()],
            subtotals: vec!["target".into()],
        };
        let cancel = AtomicBool::new(false);
        let plain = walk(&dir, &options, &cancel)
            .expect("walks")
            .expect("not cancelled");
        let batched = crate::plugins::usage_bulk::measure(&dir, &options, &cancel)
            .expect("supported here")
            .expect("walks")
            .expect("not cancelled");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("unlocks");
        assert_eq!(batched, plain);
        assert!(plain.errors >= 1, "the locked folder is noticed: {plain:?}");
        assert!(plain.freed < plain.bytes, "{plain:?}");
        assert!(
            crate::plugins::usage_bulk::measure(&dir, &options, &AtomicBool::new(true))
                .expect("supported")
                .expect("walks")
                .is_none()
        );
        // Not a folder: the plain walker handles these.
        assert!(
            crate::plugins::usage_bulk::measure(&dir.join("elsewhere"), &options, &cancel)
                .is_none()
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// The two walkers in turns on `DIRFLEET_USAGE_PATHS`, so a busy machine
    /// slows both alike. Read-only.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "needs DIRFLEET_USAGE_PATHS"]
    fn real_walkers_compared() {
        let paths = std::env::var("DIRFLEET_USAGE_PATHS").expect("DIRFLEET_USAGE_PATHS");
        let options = Options::default();
        let cancel = AtomicBool::new(false);
        for path in paths.split(':').map(Path::new) {
            for round in 0..3 {
                let started = std::time::Instant::now();
                let plain = walk(path, &options, &cancel).expect("walks");
                let plain_time = started.elapsed();
                let started = std::time::Instant::now();
                let batched = crate::plugins::usage_bulk::measure(path, &options, &cancel)
                    .expect("supported")
                    .expect("walks");
                println!(
                    "{} round {round}: plain {plain_time:.2?}, batched {:.2?}",
                    path.display(),
                    started.elapsed()
                );
                assert_eq!(plain, batched);
            }
        }
    }

    #[test]
    fn missing_and_cancelled() {
        let dir = temp_dir("cancel");
        assert!(
            measure(
                &dir.join("nope"),
                &Options::default(),
                &AtomicBool::new(false)
            )
            .is_err()
        );
        assert_eq!(
            measure(&dir, &Options::default(), &AtomicBool::new(true)).expect("walks"),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
