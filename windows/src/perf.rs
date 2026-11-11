//! What the shell waits on the core for in a large folder, measured without a
//! window: the real core and its handlers on a folder made for the test.
//!
//! A measurement and not a check of behavior, so it only runs when asked
//! (`just windows-perf`). The limits are loose: they catch the core sending
//! every row again, which costs hundreds of milliseconds per view model, and
//! not a slow machine.
use std::time::{Duration, Instant};

use dirfleet_core::app::Event;

use crate::core_link::CoreLink;
use crate::store::{Store, TabRows};

/// How many rows a window of ordinary height shows.
const IN_SIGHT: u32 = 40;
/// The longest a jump may take before its rows are there.
const JUMP_LIMIT: Duration = Duration::from_millis(250);

/// Takes view models into `store` until `ready` says the rows are the ones
/// waited for, and returns how long taking and splitting the last one took.
fn wait(core: &CoreLink, store: &mut Store, ready: impl Fn(&TabRows) -> bool) -> Duration {
    let give_up = Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(view) = core.take_view() {
            let taken = Instant::now();
            store.update(view);
            let spent = taken.elapsed();
            let rows = store
                .windows
                .first()
                .and_then(|window| window.panes.first())
                .map(|pane| &pane.rows);
            if rows.is_some_and(|rows| ready(rows)) {
                return spent;
            }
        }
        assert!(
            Instant::now() < give_up,
            "the rows never came: the core stopped answering"
        );
        std::thread::sleep(Duration::from_micros(200));
    }
}

fn measure(count: u32) {
    let folder = tempfile::tempdir().expect("a folder for the test");
    for index in 0..count {
        std::fs::write(folder.path().join(format!("file-{index:06}.txt")), b"")
            .expect("a file in the test's folder");
    }
    let core = CoreLink::new(None);
    let mut store = Store::default();
    let started = Instant::now();
    core.send(Event::Started {
        open: Some(folder.path().to_string_lossy().into_owned()),
        home: folder.path().to_string_lossy().into_owned(),
    });
    wait(&core, &mut store, |rows| rows.total_rows == count);
    let listed = started.elapsed();
    let tab = store.windows[0].panes[0].rows.tab;
    let sent = store.windows[0].panes[0].rows.rows.len();

    let mut jumps = Vec::new();
    let mut splits = Vec::new();
    for first_row in [count / 2, count - IN_SIGHT, count / 4, 0, count / 3] {
        let asked = Instant::now();
        core.send(Event::ViewportChanged {
            tab,
            first_row,
            rows: IN_SIGHT,
        });
        splits.push(wait(&core, &mut store, |rows| {
            let end = rows.first_row + rows.rows.len() as u32;
            rows.first_row <= first_row && first_row + IN_SIGHT <= end
        }));
        jumps.push(asked.elapsed());
    }
    let slowest = jumps.iter().max().copied().unwrap_or_default();
    let most_rows = store.windows[0].panes[0].rows.rows.len().max(sent);
    eprintln!(
        "PERF {count} entries: listed in {:.0} ms, {sent} rows sent first; a jump takes up to \
         {:.1} ms, of which taking and splitting the view model {:.2} ms",
        listed.as_secs_f64() * 1000.0,
        slowest.as_secs_f64() * 1000.0,
        splits
            .iter()
            .max()
            .copied()
            .unwrap_or_default()
            .as_secs_f64()
            * 1000.0,
    );
    assert!(
        most_rows < 2000,
        "the core sent {most_rows} rows of {count}: it should send only those near the viewport"
    );
    assert!(
        slowest < JUMP_LIMIT,
        "a jump in {count} entries took {slowest:?}"
    );
}

#[test]
#[ignore = "a measurement; `just windows-perf` runs it"]
fn perf_of_a_folder_of_ten_thousand() {
    measure(10_000);
}

#[test]
#[ignore = "a measurement; `just windows-perf` runs it"]
fn perf_of_a_folder_of_a_hundred_thousand() {
    measure(100_000);
}
