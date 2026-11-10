# Spike: one slice through both shells

Goal: before migrating the app, prove that a Crux core plus Rust effect handlers
can drive both an egui shell and a SwiftUI shell, fast enough for large folders.

## What was built

| Piece | Where |
| --- | --- |
| Crux app: one tab that lists a folder, opens subfolders, goes back, refreshes on change | `crates/dirfleet_core/src/app/` |
| Rust handlers: `ListDir` (request), `Watch` (stream), `Unwatch` (notify) | `crates/dirfleet_effects/` |
| BoltFFI bridge + Swift type generation | `crates/dirfleet_ffi/` |
| egui shell | `examples/core_spike.rs` (`cargo run --example core_spike [folder]`) |
| SwiftUI shell (macOS 14) | `apple/` (`just apple-build`, `just apple-test`) |
| Shared event script and golden view models | `tests/scripts/spike_navigation.{json,golden}` |

crux_core is pinned to 0.20.0 (the latest release), facet to 0.46.5 (the exact
version it requires) and BoltFFI to 0.29.3 (the version its examples use;
`boltffi_cli` must match).

Both shells use crux's `EffectRouter`. `ListDir`, `Watch` and `Unwatch` resolve
in Rust on handler threads; only `Render` reaches a shell. The Swift shell never
sees a file system effect.

## Gate results (macOS 26, Apple Silicon, release builds)

100,000-entry folder, 1000 viewport changes to scattered positions, 40 visible
rows (80 sent with overscan):

| Measurement | p50 | p95 | Gate (p95) |
| --- | --- | --- | --- |
| `view()`, typed, in process (egui path) | 9.8 µs | 14.0 µs | < 2 ms |
| Byte bridge: encode event, update, view bytes (Rust) | 14.5 µs | 23.3 µs | < 8 ms |
| Byte bridge from Swift, including decode into Swift types | 0.06–0.09 ms | 0.08–0.13 ms | < 8 ms |

The view model for a viewport is about 5.6 KB however large the folder, because
`view()` sends only the rows near the viewport (design rule 5).

Sources: `crates/dirfleet_ffi/tests/latency.rs`
(`cargo test --release -p dirfleet_ffi --test latency -- --ignored --nocapture`)
and `apple/DirFleetTests/BridgeLatencyTests.swift`.

| Other criterion | Result | Evidence |
| --- | --- | --- |
| The watcher stops when the core leaves a folder (crux #622 workaround) | Yes | `crates/dirfleet_effects/tests/router.rs` (10/10 runs) |
| A change on disk refreshes the listing | Yes | same test |
| Both shells show the same rows | Yes, byte for byte | `scripts.rs` and `ScriptTests.swift` both match the committed golden; a one-byte change to the golden makes the Swift test fail |
| Both apps launch and draw the listing | Yes | screenshots of both windows, taken by window id |

**Verdict: pass.** Design rules 5 (windowed view models) and 7 (decoding in
Rust handlers) stand, and the migration continues.

## Deviations from the plan

- The egui half is a standalone example, not part of the existing `App`. The
  spike tests the plumbing; putting the real list view on the core is the
  first migration slice.
- `ListDir` reads with `std::fs::read_dir` and sorts with a lowercase compare,
  not the app's walker, sled cache and ICU collator. Those move with the
  listing slice.
- The cross-shell script check (verification 7) was started now, since it was
  the cheapest way to show both shells see the same view models.

## Findings for the migration

1. **Handlers must not block the caller.** `EffectRouter::update` routes effects
   on the calling thread, which is the UI thread in both shells. Creating the
   notify watcher there blocked it while FSEvents started; Xcode's Thread
   Performance Checker reported the priority inversion. Watchers are now
   created on their own thread. Every handler must hand work off the same way.
2. **Aborting a stream does not reach its handler** (crux #622). The core sends
   `Unwatch { id }` when it leaves a folder. Keep this pattern for every stream.
3. **A change can be missed** between the `Watch` effect and the OS watcher
   becoming ready, since the listing runs at the same time. The listing slice
   should have the handler report `Ready` and the core re-list once on it.
4. **Generated Swift includes every app effect**, also the ones handled in
   Rust. The Swift shell treats them as unreachable.
5. **Pinned toolchain:** `boltffi pack apple --release` takes about a minute
   (macOS arm64 only, set in `crates/dirfleet_ffi/boltffi.toml`).
6. **CI:** the script test only runs on Unix (its golden files hold
   `/`-separated paths). The Swift tests are not in CI yet.
