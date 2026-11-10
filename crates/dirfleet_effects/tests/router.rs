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
