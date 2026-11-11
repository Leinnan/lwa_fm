use super::worker::{OpControl, execute};
use super::{ConflictPolicy, FileOp, Origin, Outcome, Undo, UndoStack, copy_name, validate_name};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// A fresh directory under the system temp dir, removed on drop.
struct Sandbox(PathBuf);

impl Sandbox {
    fn new(name: &str) -> Self {
        Self::new_in(&std::env::temp_dir(), name)
    }
    fn new_in(root: &Path, name: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = root.join(format!(
            "lwa_fm_fileops_{name}_{}_{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create sandbox");
        Self(dir)
    }
    fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
    fn write(&self, rel: &str, contents: &str) -> PathBuf {
        let path = self.path(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(&path, contents).expect("write file");
        path
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(op: FileOp) -> Outcome {
    execute(1, op, Origin::User, &OpControl::default(), &|_| {})
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).expect("read file")
}

#[test]
fn copies_nested_folders_and_names_conflicts() {
    let sandbox = Sandbox::new("copy");
    sandbox.write("src/tree/a.txt", "a");
    sandbox.write("src/tree/sub/b.txt", "b");
    sandbox.write("dst/tree/old.txt", "old");
    let dst = sandbox.path("dst");

    let outcome = run(FileOp::Copy {
        srcs: vec![sandbox.path("src/tree")],
        dst: dst.clone(),
        policy: ConflictPolicy::KeepBoth,
    });
    assert!(outcome.succeeded(), "{:?}", outcome.errors);
    assert_eq!(read(&dst.join("tree (2)/sub/b.txt")), "b");
    assert_eq!(outcome.created, vec![dst.join("tree (2)")]);

    // Replace merges folders and overwrites files.
    sandbox.write("src/tree/old.txt", "new");
    let outcome = run(FileOp::Copy {
        srcs: vec![sandbox.path("src/tree")],
        dst: dst.clone(),
        policy: ConflictPolicy::Replace,
    });
    assert!(outcome.succeeded());
    assert_eq!(read(&dst.join("tree/old.txt")), "new");
    assert_eq!(read(&dst.join("tree/a.txt")), "a");

    // Skip leaves the existing item untouched.
    sandbox.write("src/tree/old.txt", "skipped");
    run(FileOp::Copy {
        srcs: vec![sandbox.path("src/tree")],
        dst: dst.clone(),
        policy: ConflictPolicy::Skip,
    });
    assert_eq!(read(&dst.join("tree/old.txt")), "new");
}

#[test]
fn pasting_into_the_same_folder_makes_copies() {
    let sandbox = Sandbox::new("same");
    let file = sandbox.write("report.txt", "r");
    for _ in 0..2 {
        let outcome = run(FileOp::Copy {
            srcs: vec![file.clone()],
            dst: sandbox.0.clone(),
            policy: ConflictPolicy::Replace,
        });
        assert!(outcome.succeeded());
    }
    assert!(sandbox.path("report - Copy.txt").exists());
    assert!(sandbox.path("report - Copy (2).txt").exists());
    assert_eq!(
        copy_name(&sandbox.0, "report.txt", false),
        sandbox.path("report - Copy (3).txt")
    );
    let outcome = run(FileOp::Duplicate(vec![file]));
    assert_eq!(outcome.created, vec![sandbox.path("report - Copy (3).txt")]);
}

#[test]
fn a_folder_cannot_be_copied_into_itself() {
    let sandbox = Sandbox::new("self");
    sandbox.write("tree/inner/a.txt", "a");
    let outcome = run(FileOp::Copy {
        srcs: vec![sandbox.path("tree")],
        dst: sandbox.path("tree/inner"),
        policy: ConflictPolicy::KeepBoth,
    });
    assert_eq!(outcome.errors.len(), 1);
    assert!(!sandbox.path("tree/inner/tree").exists());
}

#[test]
fn cancelling_stops_a_copy_and_removes_the_partial_file() {
    let sandbox = Sandbox::new("cancel");
    let big = sandbox.path("big.bin");
    fs::write(&big, vec![7_u8; 4 * 1024 * 1024]).expect("write");
    fs::create_dir(sandbox.path("dst")).expect("dst");
    let control = OpControl::default();
    let outcome = execute(
        1,
        FileOp::Copy {
            srcs: vec![big],
            dst: sandbox.path("dst"),
            policy: ConflictPolicy::KeepBoth,
        },
        Origin::User,
        &control,
        &|event| {
            if matches!(event, super::FileOpEvent::Progress { .. }) {
                control.cancel();
            }
        },
    );
    assert!(outcome.cancelled);
    assert_eq!(outcome.errors, Vec::<String>::new());
    // An APFS clone is atomic: it either finished (and can be undone) or never
    // started. A byte copy is cut short and removed.
    let copy = sandbox.path("dst/big.bin");
    if copy.exists() {
        assert_eq!(outcome.created, vec![copy.clone()]);
        assert_eq!(fs::metadata(&copy).expect("copy").len(), 4 * 1024 * 1024);
    }
}

/// A copy held at its first report: what it is told, and how many reports
/// came. An APFS clone is done in one step, so there is nothing to hold.
#[cfg(not(target_os = "macos"))]
fn held_copy(
    sandbox: &Sandbox,
    control: &OpControl,
    reports: &AtomicU64,
    total: &AtomicU64,
) -> Outcome {
    const SIZE: usize = 8 * 1024 * 1024;
    let big = sandbox.path("big.bin");
    fs::write(&big, vec![7_u8; SIZE]).expect("write");
    fs::create_dir(sandbox.path("dst")).expect("dst");
    execute(
        1,
        FileOp::Copy {
            srcs: vec![big],
            dst: sandbox.path("dst"),
            policy: ConflictPolicy::KeepBoth,
        },
        Origin::User,
        control,
        &|event| {
            if let super::FileOpEvent::Progress { bytes_total, .. } = event {
                total.store(bytes_total, Ordering::Relaxed);
                if reports.fetch_add(1, Ordering::Relaxed) == 0 {
                    control.set_paused(true);
                }
            }
        },
    )
}

/// Waits for the copy's first report, then until no more come.
#[cfg(not(target_os = "macos"))]
fn wait_until_held(reports: &AtomicU64) -> u64 {
    while reports.load(Ordering::Relaxed) == 0 {
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(150));
    reports.load(Ordering::Relaxed)
}

#[cfg(not(target_os = "macos"))]
#[test]
fn a_paused_copy_waits_and_goes_on_when_resumed() {
    let sandbox = Sandbox::new("pause");
    let (control, reports, total) = (OpControl::default(), AtomicU64::new(0), AtomicU64::new(0));
    let outcome = std::thread::scope(|scope| {
        let copy = scope.spawn(|| held_copy(&sandbox, &control, &reports, &total));
        let held = wait_until_held(&reports);
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            reports.load(Ordering::Relaxed),
            held,
            "nothing is copied while it is paused"
        );
        assert!(!copy.is_finished());
        control.set_paused(false);
        copy.join().expect("the copy ends")
    });
    assert!(outcome.succeeded(), "{outcome:?}");
    let copied = fs::metadata(sandbox.path("dst/big.bin")).expect("copy");
    assert_eq!(copied.len(), 8 * 1024 * 1024);
    assert_eq!(
        total.load(Ordering::Relaxed),
        8 * 1024 * 1024,
        "a copy says how many bytes it has to write"
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn a_paused_copy_can_be_stopped() {
    let sandbox = Sandbox::new("pause_cancel");
    let (control, reports, total) = (OpControl::default(), AtomicU64::new(0), AtomicU64::new(0));
    let outcome = std::thread::scope(|scope| {
        let copy = scope.spawn(|| held_copy(&sandbox, &control, &reports, &total));
        wait_until_held(&reports);
        control.cancel();
        copy.join().expect("the copy ends")
    });
    assert!(outcome.cancelled);
    assert!(
        !sandbox.path("dst/big.bin").exists(),
        "the partial copy is removed"
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn a_checked_copy_is_read_back_and_one_that_differs_is_removed() {
    let sandbox = Sandbox::new("verify");
    let src = sandbox.write("a.bin", &"data".repeat(1000));
    fs::create_dir(sandbox.path("good")).expect("good");
    fs::create_dir(sandbox.path("bad")).expect("bad");
    let copy_to = |folder: &str| FileOp::Copy {
        srcs: vec![src.clone()],
        dst: sandbox.path(folder),
        policy: ConflictPolicy::KeepBoth,
    };

    let said_so = std::cell::Cell::new(false);
    let outcome = execute(
        1,
        copy_to("good"),
        Origin::User,
        &OpControl::with_verify(true),
        &|event| {
            if let super::FileOpEvent::Progress { current, .. } = event {
                said_so.set(said_so.get() || current == "Verifying a.bin");
            }
        },
    );
    assert!(outcome.succeeded(), "{outcome:?}");
    assert!(outcome.verified);
    assert!(said_so.get(), "progress says the copy is being read back");
    assert_eq!(read(&sandbox.path("good/a.bin")), "data".repeat(1000));

    // Unchecked, nothing says the copy was verified.
    let plain = run(copy_to("good"));
    assert!(plain.succeeded() && !plain.verified);

    // The copy changes on the disk before it is read back.
    let target = sandbox.path("bad/a.bin");
    let outcome = execute(
        2,
        copy_to("bad"),
        Origin::User,
        &OpControl::with_verify(true),
        &|event| {
            if matches!(&event, super::FileOpEvent::Progress { current, .. } if current.starts_with("Verifying"))
            {
                fs::write(&target, "something else").expect("tamper");
            }
        },
    );
    assert!(!outcome.verified);
    assert_eq!(outcome.errors.len(), 1, "{outcome:?}");
    assert!(outcome.errors[0].contains("not the same as the original"));
    assert!(!target.exists(), "the bad copy is removed");
    assert!(src.exists(), "the original stays");
}

#[test]
fn move_rename_and_their_undo_round_trip() {
    let sandbox = Sandbox::new("move");
    let file = sandbox.write("a/file.txt", "f");
    fs::create_dir(sandbox.path("b")).expect("b");
    let mut stack = UndoStack::default();

    let outcome = run(FileOp::Move {
        srcs: vec![file.clone()],
        dst: sandbox.path("b"),
        policy: ConflictPolicy::KeepBoth,
    });
    assert!(outcome.succeeded());
    assert!(sandbox.path("b/file.txt").exists() && !file.exists());
    stack.record(Origin::User, outcome.undo.expect("undo"));

    let undo = stack.pop_undo().expect("undo");
    let outcome = execute(
        2,
        FileOp::Undo(undo),
        Origin::Undo,
        &OpControl::default(),
        &|_| {},
    );
    assert!(file.exists());
    stack.record(Origin::Undo, outcome.undo.expect("redo"));
    let redo = stack.pop_redo().expect("redo");
    run(FileOp::Undo(redo));
    assert!(sandbox.path("b/file.txt").exists());

    let moved = sandbox.path("b/file.txt");
    let renamed = sandbox.path("b/renamed.txt");
    let outcome = run(FileOp::Rename {
        from: moved.clone(),
        to: renamed.clone(),
    });
    assert!(renamed.exists());
    let Some(undo @ Undo::Rename { .. }) = outcome.undo else {
        panic!("rename undo");
    };
    run(FileOp::Undo(undo));
    assert!(moved.exists() && !renamed.exists());
}

#[test]
fn batch_rename_is_one_undo_step() {
    let sandbox = Sandbox::new("batch");
    let a = sandbox.write("a.txt", "a");
    let b = sandbox.write("b.txt", "b");
    let outcome = run(FileOp::RenameMany(vec![
        (a.clone(), sandbox.path("photo (1).txt")),
        (b.clone(), sandbox.path("photo (2).txt")),
    ]));
    assert!(outcome.succeeded(), "{:?}", outcome.errors);
    assert!(sandbox.path("photo (2).txt").exists() && !a.exists());
    run(FileOp::Undo(outcome.undo.expect("undo")));
    assert!(a.exists() && b.exists());
}

#[test]
fn rename_refuses_to_overwrite() {
    let sandbox = Sandbox::new("rename");
    let a = sandbox.write("a.txt", "a");
    let b = sandbox.write("b.txt", "b");
    let outcome = run(FileOp::Rename {
        from: a.clone(),
        to: b.clone(),
    });
    assert_eq!(outcome.errors.len(), 1);
    assert_eq!(read(&b), "b");
    assert!(a.exists());
}

#[test]
fn new_folder_and_file_get_free_names() {
    let sandbox = Sandbox::new("new");
    let first = run(FileOp::NewFolder {
        parent: sandbox.0.clone(),
    });
    let second = run(FileOp::NewFolder {
        parent: sandbox.0.clone(),
    });
    assert_eq!(first.created, vec![sandbox.path("New folder")]);
    assert_eq!(second.created, vec![sandbox.path("New folder (2)")]);
    let file = run(FileOp::NewFile {
        parent: sandbox.0.clone(),
    });
    assert_eq!(file.created, vec![sandbox.path("New Text Document.txt")]);
}

#[test]
fn delete_removes_permanently() {
    let sandbox = Sandbox::new("delete");
    sandbox.write("tree/a.txt", "a");
    let file = sandbox.write("b.txt", "b");
    let outcome = run(FileOp::Delete(vec![sandbox.path("tree"), file.clone()]));
    assert!(outcome.succeeded());
    assert!(outcome.undo.is_none());
    assert!(!file.exists() && !sandbox.path("tree").exists());
}

#[test]
fn validates_names() {
    assert!(validate_name("report.txt").is_ok());
    assert!(validate_name("  ").is_err());
    assert!(validate_name("a/b").is_err());
    assert!(validate_name("..").is_err());
    if cfg!(windows) {
        assert!(validate_name("what?").is_err());
        assert!(validate_name("con.txt").is_err());
        assert!(validate_name("trailing.").is_err());
    }
}

/// Uses the real Recycle Bin, so it only runs on request.
#[test]
#[ignore = "touches the system Recycle Bin"]
fn trash_and_restore_round_trip() {
    // Some systems delete from the temp folder without recycling, so use the
    // build directory, which is on a normal drive.
    let sandbox = Sandbox::new_in(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .as_path(),
        "trash",
    );
    let file = sandbox.write("gone.txt", "g");
    let outcome = run(FileOp::Trash(vec![file.clone()]));
    assert!(outcome.succeeded(), "{:?}", outcome.errors);
    assert!(!file.exists());
    let outcome = run(FileOp::Undo(outcome.undo.expect("undo")));
    assert!(outcome.succeeded(), "{:?}", outcome.errors);
    assert!(file.exists());
}

/// Restores and purges through the Recycle Bin view's keys. Uses the real
/// Recycle Bin, so it only runs on request.
#[cfg(not(target_os = "macos"))]
#[test]
#[ignore = "touches the system Recycle Bin"]
fn recycle_bin_view_restores_and_purges() {
    use super::trash_bin;
    let sandbox = Sandbox::new_in(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .as_path(),
        "trash_view",
    );
    let file = sandbox.write("binned.txt", "b");
    let find = |file: &Path| {
        let listing = trash_bin::listing().expect("list the Recycle Bin");
        listing
            .iter()
            .map(|entry| crate::fileops::trash_bin::key_of(&entry))
            .find(|key| key.path == file)
    };

    assert!(run(FileOp::Trash(vec![file.clone()])).succeeded());
    let key = find(&file).expect("listed after trashing");
    let outcome = run(FileOp::RestoreTrashed(vec![key]));
    assert!(outcome.succeeded(), "{:?}", outcome.errors);
    assert!(outcome.trash_changed);
    assert!(file.exists());

    assert!(run(FileOp::Trash(vec![file.clone()])).succeeded());
    let key = find(&file).expect("listed after trashing again");
    let outcome = run(FileOp::PurgeTrashed(vec![key]));
    assert!(outcome.succeeded(), "{:?}", outcome.errors);
    assert!(find(&file).is_none());
    assert!(!file.exists());
}

#[cfg(unix)]
#[test]
fn copies_keep_links_and_attributes_inside_bundles() {
    let sandbox = Sandbox::new("bundle");
    sandbox.write("Kit.framework/Versions/A/Kit", "binary");
    std::os::unix::fs::symlink("A", sandbox.path("Kit.framework/Versions/Current"))
        .expect("symlink");
    #[cfg(target_os = "macos")]
    assert!(
        std::process::Command::new("xattr")
            .args(["-w", "com.example.tag", "kept"])
            .arg(sandbox.path("Kit.framework/Versions/A/Kit"))
            .status()
            .expect("xattr")
            .success()
    );
    // KeepBoth makes a fresh copy (cloned on APFS); Replace into the existing
    // copy walks it item by item.
    for policy in [ConflictPolicy::KeepBoth, ConflictPolicy::Replace] {
        let outcome = run(FileOp::Copy {
            srcs: vec![sandbox.path("Kit.framework")],
            dst: sandbox.path("out"),
            policy,
        });
        assert!(outcome.succeeded(), "{:?}", outcome.errors);
        let current = sandbox.path("out/Kit.framework/Versions/Current");
        assert!(
            current
                .symlink_metadata()
                .expect("link")
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&current).expect("read link"), Path::new("A"));
        #[cfg(target_os = "macos")]
        {
            let tag = std::process::Command::new("xattr")
                .args(["-p", "com.example.tag"])
                .arg(sandbox.path("out/Kit.framework/Versions/A/Kit"))
                .output()
                .expect("xattr");
            assert_eq!(String::from_utf8_lossy(&tag.stdout).trim(), "kept");
        }
    }
}

#[test]
fn respelling_a_name_is_not_a_conflict() {
    let sandbox = Sandbox::new("respell");
    let file = sandbox.write("notes.txt", "n");
    let other = sandbox.write("other.txt", "o");
    assert!(!super::is_respelling(&file, &other));
    let upper = sandbox.path("NOTES.txt");
    // Only a disk that ignores case sees "NOTES.txt" as the same file.
    assert_eq!(super::is_respelling(&file, &upper), upper.exists());
    let outcome = run(FileOp::Rename {
        from: file,
        to: upper,
    });
    assert!(outcome.succeeded(), "{:?}", outcome.errors);
    assert!(
        fs::read_dir(&sandbox.0)
            .expect("list")
            .flatten()
            .any(|entry| entry.file_name() == "NOTES.txt")
    );
}
