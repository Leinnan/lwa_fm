# Video thumbnails and timeline previews

The implementation lives in `src/app/assets.rs` and `src/app/assets/`: `media.rs` (probing, sampling, overviews), `process.rs` (bounded child processes), `cache.rs` (disk cache), `decode.rs` (still images), `kinds.rs` (which files get previews), `failures.rs` (retry backoff) and `ffmpeg_install.rs` (getting FFmpeg).

## Pipeline

1. The UI builds request identities from directory-entry metadata without resolving paths or reading media files each frame. Scheduler priorities are rebuilt only when intent changes.
2. General asset workers only read what is already cached: our validated posters, Windows Shell posters we saved earlier, and (on Windows) the Shell's own thumbnail cache. Anything that needs generating transfers to the bounded video scheduler, so a slow third-party thumbnail handler or an offline share can't stall image thumbnails.
3. On the video worker, Windows first asks the Shell to generate a thumbnail, giving up after 3 s (the call runs on a helper thread and is abandoned on timeout; at most two stuck calls at a time). A Shell poster is cached next to, not instead of, FFmpeg posters, because it is not a frame at a known time and must not seed overviews. Otherwise, and on other platforms, FFmpeg makes the poster.
4. FFprobe chooses the default non-artwork video stream, with resolution and stream index as deterministic tie breakers. Stream duration and duration tags take precedence over container duration, preventing audio tails from shifting samples past the video.
5. FFmpeg posters seek to 5% of the video. Successful empty decodes retry at zero. Overview samples retain decoded timestamps from FFmpeg `showinfo`, skip empty results, and deduplicate timestamps. Sparse videos can produce fewer than twelve useful frames.
6. A suitable cached FFmpeg poster immediately seeds an overview. Short videos with known bounded decode work use one sequential batch; other videos use sparse seeks and yield to the scheduler after each sample. Once a coarse overview is available, visible posters outrank further refinement.
7. A bounded background writer encodes and publishes caches. Pending writes coalesce by destination. Under pressure, previews are dropped before posters. Publication rechecks the source under the same lock used for invalidation.

## Correctness and limits

- FFmpeg and FFprobe paths are resolved together. Executable metadata contributes to the backend revision; installation and periodic refresh clear stale capabilities, probes, posters, and UI failures when that identity changes. Updates hidden behind an unchanged launcher are not detected from launcher metadata alone.
- File and directory invalidations retire source generations and purge memory entries. Disk entries are not deleted on invalidation: they are keyed by source revision and size, so they can't be served stale, and `cache::maintain`'s LRU sweep reclaims the space. Bounded generation tables invalidate all older tasks on overflow. Unrelated sources remain reusable during ordinary invalidations.
- Sources without a revision (metadata unavailable) skip the disk cache entirely, since a stale entry couldn't be told apart.
- Cache file names and checksums use a fixed-seed hasher (`cache::stable_hasher`), so a Rust toolchain upgrade doesn't orphan the cache. A golden-value test guards this.
- JPEG posters carry a checked metadata sidecar (`DFPS0002`) with source revision, size, edge, actual timestamp, and image checksum. Overview format v8 (`DFOV0008`) records actual timestamps. A poster first seen in memory (for example from an overview) is written to disk once, not on every hit. Legacy formats are purged by disk maintenance.
- GIF overviews don't include the FFmpeg backend revision in their cache key; an FFmpeg update doesn't invalidate them.
- Suitable larger posters downsample to smaller requests. Icon size does not duplicate entries with the same requested edge. Reading a poster from an overview reads only its header and first frame.
- Sample aspect ratio feeds the FFmpeg geometry filter; rotation relies on FFmpeg's automatic rotation. HDR and wide-gamut conversion uses declared primaries, transfer, matrix, and range. Without the `zscale` and `tonemap` filters, HDR **and BT.2020 SDR** sources report an unsupported-media failure rather than persisting incorrectly interpreted colors.
- One active media process; at most two decoder threads, one filter thread, and one output encoder thread. Each task has an eight-second active-work budget, a thirty-second total-age cap, and bounded output. Sequential batches have a two-second subprocess deadline. Completed samples remain usable after a later timeout, without caching a partial overview as complete.
- Overview edges are at most 480 pixels, with at most twelve frames and 12 MiB decoded pixels. Individual encoded reads/process output are bounded to 16 MiB. Poster memory is bounded to sixteen entries and 4 MiB. Pending cache writes are bounded to sixty-four entries and 16 MiB, plus the one active write. Disk eviction starts above 512 MiB and targets 384 MiB.
- Every media process runs in a Windows Job Object with kill-on-close. Cancelling terminates the job (the whole tree, including launchers) without spawning `taskkill`, and children die with the app even if it aborts. If a job can't be created, `taskkill /T` is the fallback. Pipe readers get one second after the child exits; a grandchild holding the pipe is left behind and logged instead of blocking the worker. The last 64 KiB of stderr is kept, so `showinfo` timestamps survive warning spam.
- Only a missing executable counts as "FFmpeg unavailable"; other spawn errors (an antivirus block, for example) are reported as unsupported and don't trigger a download.

## Getting FFmpeg

`find_tool` looks for each tool next to the executable, then in the app's data folder (`ffmpeg_install::managed_dir`, e.g. `%LOCALAPPDATA%\leinnan\dirfleet\data\ffmpeg`), then on `PATH`. When FFmpeg is missing, it is downloaded once per session into the data folder, with a toast while it runs. The Windows build (gyan.dev) publishes a SHA-256 next to the archive; a mismatch discards the download. Other providers publish no SHA-256, so those downloads are installed unverified (and logged).

## Measurements and defaults

An opt-in benchmark compares PNG with framed RGB/PPM, decoder thread counts, separate seeks, sequential batches, and two active processes. On this development machine, using a generated ten-second 1280×720, 30 fps H.264 video with a 300-frame GOP:

| Experiment | Median elapsed time |
| --- | ---: |
| Three seeks, 160 px PNG, one decoder thread | 649 ms |
| Three seeks, 160 px PNG, two decoder threads | 557 ms |
| Three seeks, 480 px PNG, one decoder thread | 680 ms |
| Three seeks, 480 px PNG, two decoder threads | 570 ms |
| Three seeks, 480 px framed RGB, two decoder threads | 593 ms |
| Twelve separate seeks, two decoder threads | 2,222 ms |
| One sequential overview batch, two decoder threads | 254 ms |
| Twelve seeks, two concurrent processes | 1,109 ms |

Each median uses three trials. These are synthetic local measurements, not guarantees for other codecs, devices, or files. PNG remains the production transport because framed RGB did not improve this workload. Two decoder threads improved seek time by roughly 15–17%; four were faster on this machine but consume more CPU. One active process remains the default to control memory and foreground contention. Sequential batching requires duration ≤12 seconds and estimated decoded pixel work ≤`1280 × 720 × 30 × 12`; unknown frame rate or excessive work falls back to sparse seeks.

## Verification and profiling

```text
cargo test --offline app::assets -- --test-threads=1
cargo test --offline windows_tools -- --ignored
cargo test --offline app::assets::media::integration_tests::real_ -- --ignored --test-threads=1 --nocapture
cargo test --offline benchmark_transport_and_decoder_threads -- --ignored --test-threads=1 --nocapture
cargo clippy --offline --all-targets
cargo check --offline --features profiling
```

Set `LWA_THUMBNAIL_BENCHMARK_SOURCE` to benchmark an existing video. `benchmark_external_video_overview` reports coarse/complete/persisted timing, disk reload, process count, decoded memory, and cache bytes. Set `LWA_THUMBNAIL_ARTIFACT_DIR` to an existing directory for a generated overview contact sheet. Profiling counters expose scheduler queue wait, subprocess time, process-permit wait, output bytes, sample count, cache hits, and dropped/coalesced/failed writes.
