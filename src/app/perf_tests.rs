//! Performance baselines for the listing pipeline. These are ignored by
//! default; run them in release mode so numbers reflect shipped code:
//!
//! `cargo test --release perf_ -- --ignored --nocapture --test-threads=1`
//!
//! `LWA_PERF_N` overrides the flat-directory entry count (default 20k, kept
//! modest because creating files dominates setup time on Windows).
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use super::Search;
use super::directory_view_settings::DirectoryViewSettings;
use super::{DisplayType, Sort};
use crate::app::dir_handling::{filter_visible, read_directory, sort_order};
use crate::data::files::Listing;

pub fn perf_count(default: usize) -> usize {
    std::env::var("LWA_PERF_N")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// Median wall time of `runs` executions of `f`.
fn median<T>(runs: usize, mut f: impl FnMut() -> T) -> Duration {
    let mut times: Vec<Duration> = (0..runs)
        .map(|_| {
            let started = Instant::now();
            std::hint::black_box(f());
            started.elapsed()
        })
        .collect();
    times.sort_unstable();
    times[times.len() / 2]
}

fn report(name: &str, count: usize, time: Duration) {
    eprintln!("perf {name:<32} n={count:<8} {time:?}");
}

/// Reuse a fixture across runs: building 100k files is far slower than any
/// measured operation, so the tree is created once per count in the temp dir.
fn flat_fixture(count: usize) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lwa_fm_perf_flat_{count}"));
    let marker = dir.join(".complete");
    if !marker.exists() {
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create perf fixture");
        for index in 0..count {
            let name = match index % 4 {
                0 => format!("Report_{index:06}.txt"),
                1 => format!("photo-{index:06}.jpg"),
                2 => format!("zażółć_{index:06}.dat"),
                _ => format!("notes {index:06}.md"),
            };
            std::fs::write(dir.join(name), b"x").expect("write perf fixture file");
        }
        std::fs::write(&marker, b"").expect("write perf fixture marker");
    }
    dir
}

/// A 4-level tree: 5 dirs per level, 50 files per leaf-ish dir (~39k entries).
fn deep_fixture() -> PathBuf {
    fn build(dir: &Path, depth: usize) {
        std::fs::create_dir_all(dir).expect("create perf tree dir");
        for index in 0..50 {
            std::fs::write(dir.join(format!("file_{index:03}.txt")), b"x")
                .expect("write perf tree file");
        }
        if depth == 0 {
            return;
        }
        for index in 0..5 {
            build(&dir.join(format!("dir_{index}")), depth - 1);
        }
    }
    let dir = std::env::temp_dir().join("lwa_fm_perf_deep");
    let marker = dir.join(".complete");
    if !marker.exists() {
        let _ = std::fs::remove_dir_all(&dir);
        build(&dir, 4);
        std::fs::write(&marker, b"").expect("write perf tree marker");
    }
    dir
}

fn settings(sorting: Sort) -> DirectoryViewSettings {
    DirectoryViewSettings {
        sorting,
        display_type: DisplayType::default(),
        invert_sort: false,
    }
}

#[test]
#[ignore = "performance baseline; run explicitly"]
fn perf_listing_pipeline() {
    let count = perf_count(20_000);
    let dir = flat_fixture(count);

    report(
        "read_dir+build (uncached)",
        count,
        median(5, || Listing::read(&dir)),
    );
    let listing = Listing::read(&dir).expect("read perf fixture");
    report(
        "encode (cache write)",
        count,
        median(9, || listing.encode()),
    );
    let encoded = listing.encode().expect("encode perf listing");
    report(
        "decode (persistent cache hit)",
        count,
        median(9, || Listing::decode(&encoded)),
    );
    {
        let db_dir = std::env::temp_dir().join(format!("lwa_fm_perf_sled_{}", std::process::id()));
        let db = sled::open(&db_dir).expect("open perf sled");
        db.insert(b"listing", encoded.as_slice())
            .expect("sled insert");
        report(
            "sled get + decode",
            count,
            median(9, || {
                db.get(b"listing")
                    .ok()
                    .flatten()
                    .and_then(|bytes| Listing::decode(&bytes))
            }),
        );
        drop(db);
        let _ = std::fs::remove_dir_all(db_dir);
    }
    report(
        "concat (multi-root copy)",
        count,
        median(9, || Listing::concat(std::iter::once(&listing))),
    );

    for sort in [Sort::Name, Sort::Size, Sort::Modified] {
        report(
            &format!("sort {sort:?}"),
            count,
            median(9, || sort_order(&listing, &settings(sort))),
        );
    }

    let order = sort_order(&listing, &settings(Sort::Name));
    for (label, value) in [("filter ascii", "report"), ("filter unicode", "żółć")] {
        let search = Search {
            value: value.to_owned(),
            ..Default::default()
        };
        report(
            label,
            count,
            median(9, || filter_visible(&listing, &order, false, Some(&search))),
        );
    }

    report(
        "borrow all rows",
        count,
        median(9, || {
            listing
                .iter()
                .map(|entry| entry.file_name.len())
                .sum::<usize>()
        }),
    );
}

/// Raw enumeration cost, without building a listing: the bulk reader versus
/// `std::fs::read_dir`, and a listing build on top of each.
#[test]
#[ignore = "performance baseline; run explicitly"]
fn perf_enumerate() {
    use crate::fs_enum::{enumerate, enumerate_std};
    type Read = fn(&Path, &mut dyn FnMut(&crate::fs_enum::RawEntry<'_>)) -> std::io::Result<()>;
    let count = perf_count(20_000);
    let dir = flat_fixture(count);
    let count_only = |read: Read| {
        let mut seen = 0usize;
        read(&dir, &mut |_| seen += 1).expect("enumerate perf fixture");
        seen
    };
    report(
        "enumerate native count",
        count,
        median(9, || count_only(enumerate)),
    );
    report(
        "enumerate std count",
        count,
        median(9, || count_only(enumerate_std)),
    );
    report(
        "enumerate native + build",
        count,
        median(9, || {
            let mut builder = crate::data::files::ListingBuilder::with_capacity(count);
            let root: std::sync::Arc<str> = std::sync::Arc::from("root");
            let index = builder.intern_dir(&root);
            enumerate(&dir, &mut |entry| {
                builder.push_raw(index, &entry.name, entry.metadata());
            })
            .expect("enumerate perf fixture");
            builder.finish()
        }),
    );
}

#[test]
#[ignore = "performance baseline; run explicitly"]
fn perf_deep_walk() {
    let dir = deep_fixture();
    let cancel = AtomicBool::new(false);
    let roots = [dir];
    let count = read_directory(&roots, 5, false, &cancel).len();
    report(
        "read_directory depth 5",
        count,
        median(3, || read_directory(&roots, 5, false, &cancel)),
    );
    // The app walks on its own pool, not the global one; compare the former
    // fixed 2 threads with the current machine-sized pool.
    let app_threads = std::thread::available_parallelism().map_or(2, |n| n.get().clamp(2, 8));
    for threads in [2, app_threads] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .expect("build perf pool");
        report(
            &format!("read_directory depth 5, {threads} thr"),
            count,
            median(5, || {
                pool.install(|| read_directory(&roots, 5, false, &cancel))
            }),
        );
    }
}
