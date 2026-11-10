//! Event scripts (`tests/scripts/*.json`) run through the byte bridge, with
//! each snapshot of the view model compared to `<script>.golden`. The macOS
//! shell's tests replay the same steps against the same golden files, so both
//! shells are checked against one record, not against each other.
//!
//! A script builds a fixture folder, then lists steps: `{"event": <Event>}`,
//! any core `Event` as serde JSON (`{root}` in its strings becomes the
//! fixture's root), or `{"snapshot": true}`, which waits until no pane of any
//! window is loading and records the view model. New features need no runner changes.
//!
//! Next to each script, `<script>.events.bin` holds its steps ready to replay,
//! so other shells need no knowledge of event shapes. Each step is one byte:
//! `E` followed by a little-endian `u32` length and that many bytes of the
//! bincode-encoded event, or `S` for a snapshot. This test fails when the
//! file is out of date with its script.
//!
//! `UPDATE_GOLDEN=1 cargo test -p dirfleet_ffi --test scripts` rewrites the
//! `.golden` and `.events.bin` files.
//!
//! Unix only: the golden files hold `/`-separated paths.
#![cfg(unix)]
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crux_core::bridge::{BincodeFfiFormat, FfiFormat};
use dirfleet_core::app::{Event, ViewModel};
use dirfleet_ffi::{CoreFfi, CruxShell};
use serde::Deserialize;

#[derive(Deserialize)]
struct Script {
    fixture: Fixture,
    steps: Vec<Step>,
}

#[derive(Deserialize)]
struct Fixture {
    root: String,
    modified: u64,
    entries: Vec<FixtureEntry>,
}

#[derive(Deserialize)]
struct FixtureEntry {
    path: String,
    #[serde(default)]
    dir: bool,
    #[serde(default)]
    size: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Step {
    Event(serde_json::Value),
    Snapshot(serde::de::IgnoredAny),
}

/// A step as every shell replays it.
enum Replay {
    /// A bincode-encoded `Event`.
    Event(Vec<u8>),
    Snapshot,
}

struct NoShell;
impl CruxShell for NoShell {
    fn process_effects(&self, _bytes: Vec<u8>) {}
}

fn scripts_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/scripts")
}

/// Builds the fixture from scratch, with every modification time fixed.
fn build(fixture: &Fixture) {
    let root = Path::new(&fixture.root);
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root).expect("create fixture root");
    for entry in &fixture.entries {
        let path = root.join(&entry.path);
        if entry.dir {
            std::fs::create_dir_all(&path).expect("create fixture folder");
        } else {
            std::fs::write(&path, vec![b'x'; entry.size]).expect("write fixture file");
        }
    }
    let time = SystemTime::UNIX_EPOCH + Duration::from_secs(fixture.modified);
    // Children first: setting a folder's time must come after its contents change.
    let mut paths: Vec<_> = fixture.entries.iter().map(|e| root.join(&e.path)).collect();
    paths.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    paths.push(root.to_path_buf());
    for path in paths {
        File::open(&path)
            .and_then(|file| file.set_modified(time))
            .expect("set fixture time");
    }
}

fn encode(event: &Event) -> Vec<u8> {
    let mut bytes = Vec::new();
    BincodeFfiFormat::serialize(&mut bytes, event).expect("encode event");
    bytes
}

/// Replaces `{root}` in every string of `value`.
#[allow(
    clippy::literal_string_with_formatting_args,
    reason = "a script placeholder, not a format string"
)]
fn substitute(value: &mut serde_json::Value, root: &str) {
    match value {
        serde_json::Value::String(text) => *text = text.replace("{root}", root),
        serde_json::Value::Array(items) => items.iter_mut().for_each(|v| substitute(v, root)),
        serde_json::Value::Object(fields) => fields.values_mut().for_each(|v| substitute(v, root)),
        _ => {}
    }
}

/// The script's steps with their events decoded and encoded for the bridge.
fn compile(script: &Script) -> Vec<Replay> {
    script
        .steps
        .iter()
        .map(|step| match step {
            Step::Event(value) => {
                let mut value = value.clone();
                substitute(&mut value, &script.fixture.root);
                let event: Event = serde_json::from_value(value.clone())
                    .unwrap_or_else(|error| panic!("not a core event: {value}: {error}"));
                Replay::Event(encode(&event))
            }
            Step::Snapshot(_) => Replay::Snapshot,
        })
        .collect()
}

/// The `.events.bin` form of `steps`.
fn events_file(steps: &[Replay]) -> Vec<u8> {
    let mut out = Vec::new();
    for step in steps {
        match step {
            Replay::Event(bytes) => {
                out.push(b'E');
                let length = u32::try_from(bytes.len()).expect("event fits");
                out.extend_from_slice(&length.to_le_bytes());
                out.extend_from_slice(bytes);
            }
            Replay::Snapshot => out.push(b'S'),
        }
    }
    out
}

fn run(steps: &[Replay]) -> Vec<Vec<u8>> {
    let core = CoreFfi::new(Arc::new(NoShell));
    let mut snapshots = Vec::new();
    for step in steps {
        match step {
            Replay::Event(bytes) => {
                let _ = core.update(bytes);
            }
            Replay::Snapshot => {
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    let bytes = core.view();
                    let view: ViewModel =
                        BincodeFfiFormat::deserialize(&bytes).expect("decode view");
                    let settled = view
                        .windows
                        .iter()
                        .flat_map(|window| &window.panes)
                        .all(|pane| !pane.shown.loading);
                    if settled {
                        snapshots.push(bytes);
                        break;
                    }
                    assert!(Instant::now() < deadline, "timed out loading: {view:?}");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }
    snapshots
}

/// Length-prefixed snapshots, one after another.
fn pack(snapshots: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for snapshot in snapshots {
        out.extend_from_slice(
            &u32::try_from(snapshot.len())
                .expect("snapshot fits")
                .to_le_bytes(),
        );
        out.extend_from_slice(snapshot);
    }
    out
}

#[test]
fn scripts_match_their_golden_view_models() {
    let mut checked = 0;
    for entry in std::fs::read_dir(scripts_dir()).expect("scripts folder") {
        let path = entry.expect("script entry").path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let script: Script = serde_json::from_slice(&std::fs::read(&path).expect("read script"))
            .expect("parse script");
        let steps = compile(&script);
        let events = path.with_extension("events.bin");
        let golden = path.with_extension("golden");
        build(&script.fixture);
        let actual = pack(&run(&steps));
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::write(&events, events_file(&steps)).expect("write events");
            std::fs::write(&golden, &actual).expect("write golden");
        }
        assert!(
            std::fs::read(&events).ok() == Some(events_file(&steps)),
            "{} is out of date with {}; run with UPDATE_GOLDEN=1",
            events.display(),
            path.display()
        );
        let expected = std::fs::read(&golden).expect("golden file; run with UPDATE_GOLDEN=1 once");
        assert!(
            actual == expected,
            "{} differs from {}",
            path.display(),
            golden.display()
        );
        checked += 1;
    }
    assert!(checked > 0, "no scripts found");
}
