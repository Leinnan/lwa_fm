//! The core and the Rust handlers together, behind crux's `EffectRouter`, as
//! a shell runs them: only `Render` reaches the "shell" here.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use crux_core::Core;
use crux_core::effects::{EffectRouter, Routes};
use dirfleet_core::app::{DirFleet, Event, TabViewModel};
use dirfleet_effects::Handlers;

#[derive(Clone)]
struct TestRoutes {
    handlers: Arc<Handlers>,
    renders: Arc<AtomicUsize>,
    router: Weak<EffectRouter<DirFleet, Self>>,
}

impl Routes<DirFleet> for TestRoutes {
    fn new(router: Weak<EffectRouter<DirFleet, Self>>) -> Self {
        Self {
            handlers: Arc::new(Handlers::new(&router, None)),
            renders: Arc::default(),
            router,
        }
    }
}

fn router() -> Arc<EffectRouter<DirFleet, TestRoutes>> {
    EffectRouter::new(Core::new(), |routes: TestRoutes| {
        move |effect| {
            if routes.handlers.handle(effect).is_some() {
                routes.renders.fetch_add(1, Ordering::SeqCst);
            }
        }
    })
}

fn wait_for(
    router: &EffectRouter<DirFleet, TestRoutes>,
    what: &str,
    done: impl Fn(&TabViewModel) -> bool,
) -> TabViewModel {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let view = router.view().windows.first().map(|w| w.active.clone());
        if let Some(view) = view.as_ref().filter(|view| done(view)) {
            return view.clone();
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {view:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn lists_refreshes_on_change_and_stops_watching_on_leave() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().canonicalize().expect("canonical temp dir");
    std::fs::create_dir(root.join("sub")).expect("create sub");
    std::fs::write(root.join("a.txt"), b"x").expect("write a.txt");

    let router = router();
    router.update(Event::NewWindow {
        path: Some(root.to_string_lossy().into_owned()),
    });
    let view = wait_for(&router, "the listing", |v| !v.loading);
    let tab = view.id;
    let names: Vec<_> = view.rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["sub", "a.txt"]);
    assert!(router.routes.renders.load(Ordering::SeqCst) >= 2);
    assert_eq!(router.routes.handlers.watch_count(), 1);

    std::fs::write(root.join("b.txt"), b"y").expect("write b.txt");
    wait_for(&router, "the new file", |v| {
        v.rows.iter().any(|r| r.name == "b.txt")
    });

    router.update(Event::OpenRow { tab, row: 0 });
    let view = wait_for(&router, "the subfolder", |v| v.title == "sub" && !v.loading);
    assert!(view.nav.back);
    assert_eq!(
        router.routes.handlers.watch_count(),
        1,
        "the parent's watch stopped when its folder was left"
    );
}

fn router_with_state(file: &std::path::Path) -> Arc<EffectRouter<DirFleet, TestRoutes>> {
    let file = file.to_path_buf();
    EffectRouter::new(Core::new(), move |routes: TestRoutes| {
        let handlers = Handlers::new(&routes.router, Some(file));
        move |effect| {
            if handlers.handle(effect).is_some() {
                routes.renders.fetch_add(1, Ordering::SeqCst);
            }
        }
    })
}

#[test]
fn windows_are_saved_and_come_back_on_the_next_start() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().canonicalize().expect("canonical temp dir");
    std::fs::create_dir(root.join("sub")).expect("create sub");
    let file = root.join("state").join("state.json");
    let start = || Event::Started {
        open: None,
        home: root.to_string_lossy().into_owned(),
    };

    let router = router_with_state(&file);
    router.update(start());
    let view = wait_for(&router, "home", |v| !v.loading && v.total_rows == 2);
    router.update(Event::OpenRow {
        tab: view.id,
        row: 1,
    });
    wait_for(&router, "the subfolder", |v| v.title == "sub");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !std::fs::read_to_string(&file).is_ok_and(|text| text.contains("sub")) {
        assert!(Instant::now() < deadline, "the subfolder was never saved");
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(router);

    let router = router_with_state(&file);
    router.update(start());
    wait_for(&router, "the saved subfolder", |v| {
        v.title == "sub" && !v.loading
    });
}

#[test]
fn duplicates_are_found_set_by_set_and_leave_the_results_when_deleted() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().canonicalize().expect("canonical temp dir");
    for folder in ["a", "b"] {
        std::fs::create_dir(root.join(folder)).expect("create folder");
    }
    let film = vec![7_u8; 1000];
    for file in ["a/film.bin", "b/film copy.bin", "b/third.bin"] {
        std::fs::write(root.join(file), &film).expect("write film");
    }
    std::fs::write(root.join("note.txt"), b"hi").expect("write note");
    std::fs::write(root.join("a/note again.txt"), b"hi").expect("write note");
    std::fs::write(root.join("alone.txt"), b"nothing like it").expect("write file");

    let router = router();
    router.update(Event::NewWindow {
        path: Some(root.to_string_lossy().into_owned()),
    });
    let folder = wait_for(&router, "the listing", |v| !v.loading);
    router.update(Event::FindDuplicates {
        tab: folder.id,
        selected: false,
    });
    // The tab of results is the one shown now.
    let found = wait_for(&router, "the duplicates", |v| {
        v.results
            .as_ref()
            .is_some_and(|results| !results.running && results.sets == 2)
    });
    let tab = found.id;
    assert_ne!(tab, folder.id);
    let groups: Vec<(&str, u32)> = found
        .groups
        .iter()
        .map(|group| (group.label.as_str(), group.count))
        .collect();
    assert_eq!(groups, [("1.0 KB each", 3), ("2 B each", 2)]);
    let names = |view: &TabViewModel| -> Vec<String> {
        view.rows.iter().map(|row| row.name.clone()).collect()
    };
    assert_eq!(
        names(&found),
        [
            "film.bin",
            "film copy.bin",
            "third.bin",
            "note again.txt",
            "note.txt"
        ]
    );

    // One film deleted for good: it leaves the results, its set stays.
    let delete = |row: u32| {
        router.update(Event::Click {
            tab,
            row,
            command: false,
            shift: false,
        });
        router.update(Event::DeleteSelection { tab });
    };
    delete(0);
    let after = wait_for(&router, "the deleted film to leave", |v| v.total_rows == 4);
    assert!(!root.join("a/film.bin").exists());
    assert_eq!(
        names(&after),
        ["film copy.bin", "third.bin", "note again.txt", "note.txt"]
    );
    // One note deleted: the other is no duplicate any more.
    delete(3);
    let after = wait_for(&router, "the notes to leave", |v| {
        v.total_rows == 2 && v.results.as_ref().is_some_and(|results| results.sets == 1)
    });
    assert_eq!(names(&after), ["film copy.bin", "third.bin"]);
    assert!(root.join("a/note again.txt").exists());
}

#[test]
fn two_folders_are_compared_and_brought_to_agree_by_copying() {
    use dirfleet_core::app::{ConflictChoice, Side};
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().canonicalize().expect("canonical temp dir");
    let write = |file: &str, bytes: &[u8], modified: u64| {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().expect("a folder")).expect("create folder");
        std::fs::write(&path, bytes).expect("write file");
        std::fs::File::options()
            .write(true)
            .open(&path)
            .and_then(|file| {
                file.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(modified))
            })
            .expect("set the time");
    };
    write("a/same.txt", b"same", 1000);
    write("b/same.txt", b"same", 1000);
    write("a/only a.txt", b"left", 1000);
    write("a/sub/deep.txt", b"deep", 1000);
    write("b/only b.txt", b"right", 1000);
    write("a/changed.txt", b"new words", 5000);
    write("b/changed.txt", b"old", 1000);
    let text = |folder: &str| root.join(folder).to_string_lossy().into_owned();

    let router = router();
    router.update(Event::NewWindow {
        path: Some(text("a")),
    });
    let left = wait_for(&router, "the left folder", |v| !v.loading);
    router.update(Event::SplitNew {
        tab: left.id,
        side: Side::Right,
    });
    let right = wait_for(&router, "the new pane", |v| v.id != left.id && !v.loading);
    router.update(Event::Navigate {
        tab: right.id,
        path: text("b"),
    });
    let _ = wait_for(&router, "the right folder", |v| {
        !v.loading && v.rows.iter().any(|row| row.name == "only b.txt")
    });
    let window = router.view().windows[0].id;
    assert!(router.view().windows[0].compare.is_some());
    router.update(Event::CompareFolders { window });
    let differences = |view: &TabViewModel| {
        view.results
            .as_ref()
            .filter(|results| !results.running)
            .and_then(|results| results.compare.as_ref())
            .map(|compare| compare.differences)
    };
    let found = wait_for(&router, "what differs", |v| differences(v) == Some(4));
    let tab = found.id;
    let groups: Vec<(&str, u32)> = found
        .groups
        .iter()
        .map(|group| (group.label.as_str(), group.count))
        .collect();
    assert_eq!(
        groups,
        [("Only in a", 2), ("Only in b", 1), ("Newer in a", 1)]
    );
    let names = |view: &TabViewModel| -> Vec<String> {
        view.rows.iter().map(|row| row.name.clone()).collect()
    };
    assert_eq!(
        names(&found),
        ["deep.txt", "only a.txt", "only b.txt", "changed.txt"]
    );

    // Everything goes right: one file would be replaced, so it is asked.
    router.update(Event::SyncSelection {
        tab,
        to_left: false,
    });
    let asked = wait_for(&router, "the question", |v| {
        v.results
            .as_ref()
            .and_then(|results| results.compare.as_ref())
            .is_some_and(|compare| compare.asking.is_some())
    });
    assert!(!root.join("b/only a.txt").exists(), "{asked:?}");
    router.update(Event::AnswerSync {
        tab,
        choice: ConflictChoice::Replace,
    });
    // What was copied leaves the results without the folders being
    // compared again; what is only on the right stays.
    let after = wait_for(&router, "the copied files to leave", |v| v.total_rows == 1);
    assert_eq!(names(&after), ["only b.txt"]);
    assert_eq!(
        after.results.expect("results").summary,
        "1 difference: 1 only in b"
    );
    let read = |file: &str| std::fs::read(root.join(file)).expect("read");
    assert_eq!(read("b/only a.txt"), b"left");
    assert_eq!(read("b/sub/deep.txt"), b"deep");
    assert_eq!(read("b/changed.txt"), b"new words");
    assert!(!root.join("a/only b.txt").exists());
}

#[test]
fn files_are_tagged_found_by_their_tag_and_listed_under_it() {
    use dirfleet_core::app::TagColor;
    let dir = tempfile::tempdir().expect("temp dir");
    // As the app is given folders: without the long-path prefix.
    let root = dir.path().to_path_buf();
    for file in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(root.join(file), file).expect("write file");
    }
    let router = router();
    router.update(Event::NewWindow {
        path: Some(root.to_string_lossy().into_owned()),
    });
    let folder = wait_for(&router, "the listing", |v| v.rows.len() == 3);
    let tab = folder.id;
    let row = |name: &str| {
        folder
            .rows
            .iter()
            .find(|row| row.name == name)
            .expect("listed")
            .index
    };
    // The first and the last file get the first tag (red, to start with).
    for (name, add) in [("a.txt", false), ("c.txt", true)] {
        router.update(Event::Click {
            tab,
            row: row(name),
            command: add,
            shift: false,
        });
    }
    router.update(Event::ToggleTag { tab, tag: 1 });
    let tagged = wait_for(&router, "the dots", |v| {
        v.rows.iter().filter(|row| !row.tags.is_empty()).count() == 2
    });
    let dots = |view: &TabViewModel, name: &str| {
        view.rows
            .iter()
            .find(|row| row.name == name)
            .map(|row| row.tags.clone())
    };
    assert_eq!(dots(&tagged, "a.txt"), Some(vec![TagColor::Red]));
    assert_eq!(dots(&tagged, "b.txt"), Some(Vec::new()));
    assert_eq!(router.view().tags[0].count, 2);

    // Searched for by the tag's name, in any case.
    router.update(Event::SetSearchText {
        tab,
        text: "tag:RED".to_owned(),
    });
    let found = wait_for(&router, "the tagged files", |v| v.total_rows == 2);
    let names: Vec<&str> = found.rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(names, ["a.txt", "c.txt"]);
    router.update(Event::SetSearchText {
        tab,
        text: String::new(),
    });
    let _ = wait_for(&router, "the whole folder", |v| v.total_rows == 3);

    // The tag's own tab lists them; one deleted behind the app's back is
    // named as missing, and forgotten when told to.
    router.update(Event::OpenTag { tab, tag: 1 });
    let listed = wait_for(&router, "the tag's items", |v| {
        v.results
            .as_ref()
            .is_some_and(|results| results.tag.is_some())
            && v.total_rows == 2
    });
    assert_eq!(listed.title, "Red");
    std::fs::remove_file(root.join("c.txt")).expect("remove");
    router.update(Event::Refresh { tab: listed.id });
    let missing = |view: &TabViewModel| {
        view.results
            .as_ref()
            .and_then(|results| results.tag.as_ref())
            .map(|place| place.missing)
    };
    let after = wait_for(&router, "the missing file", |v| missing(v) == Some(1));
    assert_eq!(after.total_rows, 1);
    assert_eq!(
        after.results.expect("results").summary,
        "1 item. 1 more is no longer where it was"
    );
    router.update(Event::RemoveMissingTagged { tab: listed.id });
    let _ = wait_for(&router, "the missing file to be forgotten", |v| {
        missing(v) == Some(0) && v.total_rows == 1
    });
    assert_eq!(router.view().tags[0].count, 1);
}
