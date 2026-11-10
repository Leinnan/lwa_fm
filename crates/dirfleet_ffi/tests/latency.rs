//! The spike's go/no-go numbers on a 100k-entry folder open in 10 windows,
//! measured in Rust: `view()` alone, and a viewport change through the byte
//! bridge a native shell uses (encode event, update, view, bytes out).
//!
//! `cargo test --release -p dirfleet_ffi --test latency -- --ignored --nocapture`
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use crux_core::Core;
use crux_core::bridge::{BincodeFfiFormat, FfiFormat};
use crux_core::effects::{EffectRouter, Routes};
use dirfleet_core::app::{DirFleet, Event, ViewModel};
use dirfleet_effects::Handlers;
use dirfleet_ffi::{CoreFfi, CruxShell};

const ENTRIES: usize = 100_000;
const RUNS: usize = 1000;
const WINDOWS: u32 = 10;

/// The flat folder `perf_tests` also uses, created once in the temp folder.
fn fixture() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lwa_fm_perf_flat_{ENTRIES}"));
    if !dir.join(".complete").exists() {
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create fixture");
        for index in 0..ENTRIES {
            std::fs::write(dir.join(format!("file_{index:06}.txt")), b"x").expect("write fixture");
        }
        std::fs::write(dir.join(".complete"), b"").expect("mark fixture");
    }
    dir
}

fn percentiles(mut times: Vec<Duration>) -> (Duration, Duration, Duration) {
    times.sort_unstable();
    let at = |p: f64| times[((times.len() - 1) as f64 * p) as usize];
    (at(0.5), at(0.95), at(0.99))
}

fn encode<T: serde::Serialize>(value: &T) -> Vec<u8> {
    let mut bytes = Vec::new();
    BincodeFfiFormat::serialize(&mut bytes, value).expect("encode");
    bytes
}

fn decode_view(bytes: &[u8]) -> ViewModel {
    BincodeFfiFormat::deserialize(bytes).expect("decode view model")
}

/// Whether every window has listed most of the folder.
fn all_listed(view: &ViewModel) -> bool {
    view.windows.len() == WINDOWS as usize
        && view
            .windows
            .iter()
            .all(|window| window.active.total_rows as usize > ENTRIES / 2)
}

/// The viewport change of run `run`: scattered rows, in each window in turn.
/// Tab ids start at 1, one tab per window.
fn viewport(run: usize) -> Event {
    Event::ViewportChanged {
        tab: u32::try_from(run).expect("run fits") % WINDOWS + 1,
        first_row: u32::try_from(run * 97 % ENTRIES).expect("row fits"),
        rows: 40,
    }
}

fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A native shell that only counts renders.
struct Renders(AtomicUsize);

impl CruxShell for Renders {
    fn process_effects(&self, _bytes: Vec<u8>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Clone)]
struct RustRoutes {
    handlers: Arc<Handlers>,
}

impl Routes<DirFleet> for RustRoutes {
    fn new(router: Weak<EffectRouter<DirFleet, Self>>) -> Self {
        Self {
            handlers: Arc::new(Handlers::new(&router, None)),
        }
    }
}

#[test]
#[ignore = "spike measurement; run explicitly in release"]
fn view_and_bridge_latency_on_a_100k_folder() {
    let folder = fixture().to_string_lossy().into_owned();

    // view() as an in-process Rust shell (egui) calls it.
    let router = EffectRouter::new(Core::<DirFleet>::new(), |routes: RustRoutes| {
        move |effect| {
            let _ = routes.handlers.handle(effect);
        }
    });
    for _ in 0..WINDOWS {
        router.update(Event::NewWindow {
            path: Some(folder.clone()),
        });
    }
    wait_until("the listings", || all_listed(&router.view()));
    let mut times = Vec::with_capacity(RUNS);
    for run in 0..RUNS {
        router.update(viewport(run));
        let started = Instant::now();
        let view = std::hint::black_box(router.view());
        times.push(started.elapsed());
        // 40 in sight: those, a screen each way and half a screen's step.
        assert!(view.windows.iter().all(|w| w.active.rows.len() <= 140));
    }
    let (p50, p95, p99) = percentiles(times);
    eprintln!("spike view() typed            p50 {p50:?} p95 {p95:?} p99 {p99:?}");

    // The byte bridge a native shell uses.
    let renders = Arc::new(Renders(AtomicUsize::new(0)));
    let core = CoreFfi::new(renders.clone());
    for _ in 0..WINDOWS {
        let _ = core.update(&encode(&Event::NewWindow {
            path: Some(folder.clone()),
        }));
    }
    wait_until("the bridged listings", || {
        all_listed(&decode_view(&core.view()))
    });
    let mut times = Vec::with_capacity(RUNS);
    let mut bytes = 0;
    for run in 0..RUNS {
        let started = Instant::now();
        let _ = core.update(&encode(&viewport(run)));
        let view = core.view();
        times.push(started.elapsed());
        bytes = view.len();
    }
    let (p50, p95, p99) = percentiles(times);
    eprintln!("spike bridge update+view      p50 {p50:?} p95 {p95:?} p99 {p99:?} ({bytes} bytes)");
    assert!(
        renders.0.load(Ordering::SeqCst) > 0,
        "renders reached the shell"
    );
}

/// One window on the 100k folder with every row selected: what a view and
/// the events of a scroll, a hover and a click cost then, with the inspector
/// closed and open.
#[test]
#[ignore = "measurement; run explicitly in release"]
fn view_and_events_with_everything_selected() {
    let folder = fixture().to_string_lossy().into_owned();
    let router = EffectRouter::new(Core::<DirFleet>::new(), |routes: RustRoutes| {
        move |effect| {
            let _ = routes.handlers.handle(effect);
        }
    });
    router.update(Event::NewWindow { path: Some(folder) });
    wait_until("the listing", || {
        router
            .view()
            .windows
            .first()
            .is_some_and(|window| window.active.total_rows as usize > ENTRIES / 2)
    });
    let tab = router.view().windows[0].active.id;
    let measure = |name: &str, runs: usize, act: &dyn Fn(usize)| {
        let mut times = Vec::with_capacity(runs);
        for run in 0..runs {
            let started = Instant::now();
            act(run);
            times.push(started.elapsed());
        }
        let (p50, p95, p99) = percentiles(times);
        eprintln!("selected {name:<28} p50 {p50:?} p95 {p95:?} p99 {p99:?}");
    };
    for inspector in [false, true] {
        router.update(Event::SetInspector { shown: inspector });
        router.update(Event::SelectAll { tab });
        let selected = router.view().windows[0].active.selected_count;
        assert!(selected as usize > ENTRIES / 2, "everything is selected");
        eprintln!("inspector {}", if inspector { "open" } else { "closed" });
        measure("view()", 200, &|_| {
            std::hint::black_box(router.view());
        });
        measure("viewport + view()", 200, &|run| {
            router.update(Event::ViewportChanged {
                tab,
                first_row: u32::try_from(run * 97 % ENTRIES).expect("row fits"),
                rows: 40,
            });
            std::hint::black_box(router.view());
        });
        measure("hover", 200, &|run| {
            router.update(Event::HoverRow {
                tab,
                row: u32::try_from(run % 40).expect("row fits"),
                inside: true,
            });
        });
        measure("click + view(), select all", 20, &|run| {
            router.update(Event::Click {
                tab,
                row: u32::try_from(run).expect("row fits"),
                command: false,
                shift: false,
            });
            std::hint::black_box(router.view());
            router.update(Event::SelectAll { tab });
        });
    }
}

/// One window on the 100k folder: how long reversing the order takes, and
/// coming Back into the folder with every row selected.
#[test]
#[ignore = "measurement; run explicitly in release"]
fn reversing_and_coming_back_on_a_100k_folder() {
    let folder = fixture().to_string_lossy().into_owned();
    let router = EffectRouter::new(Core::<DirFleet>::new(), |routes: RustRoutes| {
        move |effect| {
            let _ = routes.handlers.handle(effect);
        }
    });
    router.update(Event::NewWindow { path: Some(folder) });
    let shown = || router.view().windows[0].active.clone();
    wait_until("the listing", || {
        router
            .view()
            .windows
            .first()
            .is_some_and(|window| window.active.total_rows as usize > ENTRIES / 2)
    });
    let tab = shown().id;
    // Whatever the fixture's names are: the first row changes and comes back.
    let first = || shown().rows.first().map(|row| row.name.clone());
    let top = first();
    let started = Instant::now();
    router.update(Event::SetReverse { tab, reverse: true });
    wait_until("the reversed order", || first() != top);
    eprintln!("100k reverse                  {:?}", started.elapsed());
    router.update(Event::SetReverse {
        tab,
        reverse: false,
    });
    wait_until("the order again", || first() == top);

    router.update(Event::SelectAll { tab });
    let total = shown().total_rows;
    router.update(Event::Navigate {
        tab,
        path: std::env::temp_dir().to_string_lossy().into_owned(),
    });
    wait_until("another folder", || shown().total_rows != total);
    let started = Instant::now();
    router.update(Event::Back { tab });
    wait_until("the folder again, selected", || {
        let tab = shown();
        tab.total_rows == total && tab.selected_count == total
    });
    eprintln!("100k back, all selected       {:?}", started.elapsed());
}
