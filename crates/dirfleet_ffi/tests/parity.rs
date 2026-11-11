//! The parity ledger (`docs/crux-migration/parity.toml`) checked against the
//! code, so it can't drift into wishful thinking: every test it names must
//! exist, every feature marked done must name a core test and a Swift test,
//! and every core event must belong to some feature. The native Windows shell
//! has a status of its own per feature (`windows`), and the unit tests it
//! names must exist too.
//!
//! `just parity` runs it and prints how far the Mac and Windows apps have come.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use dirfleet_core::app::Event;
use facet::{Facet, Shape, Type, UserType};
use serde::Deserialize;

#[derive(Deserialize)]
struct Ledger {
    feature: Vec<Feature>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Feature {
    id: String,
    area: u32,
    #[allow(dead_code, reason = "for people reading the ledger")]
    summary: String,
    #[serde(default)]
    #[allow(dead_code, reason = "for people reading the ledger")]
    egui_ref: String,
    status: Status,
    na_mac: Option<String>,
    #[serde(default)]
    egui_on_core: bool,
    #[serde(default)]
    events: Vec<String>,
    #[serde(default)]
    core_test: Vec<String>,
    #[serde(default)]
    egui_test: Vec<String>,
    #[serde(default)]
    swift_test: Vec<String>,
    #[serde(default = "open")]
    windows: Status,
    na_windows: Option<String>,
    #[serde(default)]
    windows_test: Vec<String>,
    #[allow(dead_code, reason = "for people reading the ledger")]
    notes: Option<String>,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "lowercase")]
enum Status {
    Open,
    Done,
    Na,
}

/// The Windows shell's status where the ledger does not say.
const fn open() -> Status {
    Status::Open
}

/// The enums whose every variant must belong to a feature, with the ledger
/// field that lists them. Add the core's modal and keymap command enums
/// here (with `modals` and `commands` fields on `Feature`) once they exist.
fn covered_enums() -> Vec<(&'static str, &'static Shape)> {
    vec![("events", Event::SHAPE)]
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn ledger() -> Ledger {
    let path = repo().join("docs/crux-migration/parity.toml");
    let text = std::fs::read_to_string(&path).expect("read the parity ledger");
    toml::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The variants of `shape`, leaving out internal ones (`#[facet(skip)]`).
fn variants(shape: &Shape) -> Vec<&'static str> {
    let Type::User(UserType::Enum(kind)) = shape.ty else {
        panic!("{} is not an enum", shape.type_identifier);
    };
    kind.variants
        .iter()
        .filter(|variant| !variant.has_builtin_attr("skip"))
        .map(|variant| variant.name)
        .collect()
}

/// Every file under `dir` with extension `ext`, skipping build output.
fn files(dir: &Path, ext: &str, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if matches!(
            name.to_str(),
            Some("target" | "build" | "generated" | "vendor" | ".git")
        ) {
            continue;
        }
        if path.is_dir() {
            files(&path, ext, found);
        } else if path.extension().is_some_and(|e| e == ext) {
            found.push(path);
        }
    }
}

/// Names of Rust functions marked `#[test]` (other attributes may sit between).
fn rust_tests() -> BTreeSet<String> {
    let mut sources = Vec::new();
    for dir in ["src", "crates", "tests", "examples", "windows"] {
        files(&repo().join(dir), "rs", &mut sources);
    }
    let mut names = BTreeSet::new();
    for path in sources {
        let text = std::fs::read_to_string(&path).expect("read a Rust source");
        let mut after_test = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with("#[test]") {
                after_test = true;
            } else if after_test && line.starts_with("#[") {
                // `#[ignore = …]` and the like.
            } else if after_test {
                if let Some(name) = function_name(line, "fn ") {
                    names.insert(name);
                }
                after_test = false;
            }
        }
    }
    names
}

/// Names of `XCTest` methods (`func test…`).
fn swift_tests() -> BTreeSet<String> {
    let mut sources = Vec::new();
    files(&repo().join("apple"), "swift", &mut sources);
    sources
        .iter()
        .flat_map(|path| {
            let text = std::fs::read_to_string(path).expect("read a Swift source");
            text.lines()
                .filter_map(|line| function_name(line.trim(), "func "))
                .filter(|name| name.starts_with("test"))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The identifier after `keyword` in `line`, which may start with modifiers.
fn function_name(line: &str, keyword: &str) -> Option<String> {
    let start = line.find(keyword)? + keyword.len();
    let name: String = line[start..]
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Everything wrong with `ledger`, given the tests that exist.
fn problems(ledger: &Ledger, rust: &BTreeSet<String>, swift: &BTreeSet<String>) -> Vec<String> {
    let mut problems = Vec::new();
    let mut ids = BTreeSet::new();
    let mut named: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for f in &ledger.feature {
        let id = &f.id;
        if !ids.insert(id.as_str()) {
            problems.push(format!("{id}: listed twice"));
        }
        if !(1..=23).contains(&f.area) {
            problems.push(format!(
                "{id}: area {} is not one of parity.md's 1–23",
                f.area
            ));
        }
        match (f.status, &f.na_mac) {
            (Status::Na, None) => problems.push(format!("{id}: \"na\" needs na_mac saying why")),
            (Status::Open | Status::Done, Some(_)) => {
                problems.push(format!("{id}: na_mac is only for \"na\" entries"));
            }
            _ => {}
        }
        if f.status == Status::Done && (f.core_test.is_empty() || f.swift_test.is_empty()) {
            problems.push(format!("{id}: \"done\" needs a core_test and a swift_test"));
        }
        if f.egui_on_core && f.core_test.is_empty() {
            problems.push(format!("{id}: egui_on_core needs a core_test"));
        }
        match (f.windows, &f.na_windows) {
            (Status::Na, None) => {
                problems.push(format!("{id}: windows \"na\" needs na_windows saying why"));
            }
            (Status::Open | Status::Done, Some(_)) => {
                problems.push(format!("{id}: na_windows is only for windows \"na\""));
            }
            _ => {}
        }
        for test in f
            .core_test
            .iter()
            .chain(&f.egui_test)
            .chain(&f.windows_test)
        {
            if !rust.contains(test) {
                problems.push(format!("{id}: no Rust test named {test}"));
            }
        }
        for test in &f.swift_test {
            if !swift.contains(test) {
                problems.push(format!("{id}: no Swift test named {test}"));
            }
        }
        named
            .entry("events")
            .or_default()
            .extend(f.events.iter().map(String::as_str));
    }
    for (field, shape) in covered_enums() {
        let variants = variants(shape);
        let listed = named.remove(field).unwrap_or_default();
        for variant in &variants {
            if !listed.contains(variant) {
                problems.push(format!(
                    "{}::{variant} belongs to no feature: add it to the `{field}` of one",
                    shape.type_identifier
                ));
            }
        }
        for name in listed {
            if !variants.contains(&name) {
                problems.push(format!(
                    "`{field}` names {name}, which {} doesn't have",
                    shape.type_identifier
                ));
            }
        }
    }
    problems
}

#[test]
fn the_parity_ledger_matches_the_code() {
    let ledger = ledger();
    let problems = problems(&ledger, &rust_tests(), &swift_tests());
    assert!(
        problems.is_empty(),
        "docs/crux-migration/parity.toml is out of date:\n  {}",
        problems.join("\n  ")
    );

    let count = |status| ledger.feature.iter().filter(|f| f.status == status).count();
    let wanted = ledger.feature.len() - count(Status::Na);
    let on_core = ledger.feature.iter().filter(|f| f.egui_on_core).count();
    eprintln!(
        "parity: the Mac app has {} of {wanted} features ({} open, {} not wanted on the Mac); \
         the egui app uses the core for {on_core}",
        count(Status::Done),
        count(Status::Open),
        count(Status::Na),
    );
    let windows = |status| {
        ledger
            .feature
            .iter()
            .filter(|f| f.windows == status)
            .count()
    };
    eprintln!(
        "parity: the Windows app has {} of {} features ({} open, {} not wanted on Windows)",
        windows(Status::Done),
        ledger.feature.len() - windows(Status::Na),
        windows(Status::Open),
        windows(Status::Na),
    );
}

#[test]
fn the_check_catches_a_missing_test_and_an_uncovered_event() {
    let mut ledger = ledger();
    let rust = rust_tests();
    let swift = swift_tests();
    let navigate = ledger
        .feature
        .iter_mut()
        .find(|f| f.events.iter().any(|e| e == "Navigate"))
        .expect("some feature introduced Navigate");
    navigate.events.retain(|e| e != "Navigate");
    navigate.swift_test.push("testThatDoesNotExist".into());
    navigate
        .windows_test
        .push("a_windows_test_that_does_not_exist".into());
    navigate.na_windows = Some("a reason without the status".into());
    let problems = problems(&ledger, &rust, &swift);
    assert!(
        problems
            .iter()
            .any(|p| p.contains("Event::Navigate belongs to no feature")),
        "{problems:?}"
    );
    assert!(
        problems
            .iter()
            .any(|p| p.contains("no Swift test named testThatDoesNotExist")),
        "{problems:?}"
    );
    assert!(
        problems
            .iter()
            .any(|p| p.contains("no Rust test named a_windows_test_that_does_not_exist")),
        "{problems:?}"
    );
    assert!(
        problems
            .iter()
            .any(|p| p.contains("na_windows is only for windows")),
        "{problems:?}"
    );
}
