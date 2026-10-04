# Video thumbnails and timeline previews

The implementation lives in `src/app/assets.rs` and `src/app/assets/{media,process,cache}.rs`.

## Pipeline

1. The UI builds request identities from directory-entry metadata without resolving paths or reading media files each frame. Scheduler priorities are rebuilt only when intent changes.
2. General asset workers check validated poster caches. Video misses transfer to the bounded video scheduler; cache hits do not wait behind FFmpeg decoding.
3. FFprobe chooses the default non-artwork video stream, with resolution and stream index as deterministic tie breakers. Stream duration and duration tags take precedence over container duration, preventing audio tails from shifting samples past the video.
4. Posters seek to 5% of the video. Successful empty decodes retry at zero. Overview samples retain decoded timestamps from FFmpeg `showinfo`, skip empty results, and deduplicate timestamps. Sparse videos can produce fewer than twelve useful frames.
5. A suitable cached poster immediately seeds an overview. Short videos with known bounded decode work use one sequential batch; other videos use sparse seeks and yield to the scheduler after each sample. Once a coarse overview is available, visible posters outrank further refinement.
6. A bounded background writer encodes and publishes caches. Pending writes coalesce by destination. Under pressure, previews are dropped before posters. Publication rechecks the source under the same lock used for invalidation.

## Correctness and limits

- FFmpeg and FFprobe paths are resolved together. Executable metadata contributes to the backend revision; installation and periodic refresh clear stale capabilities, probes, posters, and UI failures when that identity changes. Updates hidden behind an unchanged launcher are not detected from launcher metadata alone.
- File and directory invalidations retire source generations and purge memory and owned disk entries. Bounded generation tables invalidate all older tasks on overflow. Unrelated sources remain reusable during ordinary invalidations.
- JPEG posters carry a checked metadata sidecar with source revision, size, edge, actual timestamp, and image checksum. Overview format v7 records actual timestamps. Legacy formats regenerate on demand; disk maintenance eventually evicts them.
- Suitable larger posters downsample to smaller requests. Icon size does not duplicate entries with the same requested edge. Reading a poster from an overview reads only its header and first frame.
- Rotation and sample aspect ratio feed the FFmpeg geometry filter. HDR and wide-gamut conversion uses declared primaries, transfer, matrix, and range. HDR without suitable metadata or required conversion filters reports an unsupported-media failure rather than persisting incorrectly interpreted colors.
- One active media process; at most two decoder threads, one filter thread, and one output encoder thread. Each task has an eight-second active-work budget, a thirty-second total-age cap, and bounded output. Sequential batches have a two-second subprocess deadline. Completed samples remain usable after a later timeout, without caching a partial overview as complete.
- Overview edges are at most 480 pixels, with at most twelve frames and 12 MiB decoded pixels. Individual encoded reads/process output are bounded to 16 MiB. Poster memory is bounded to sixteen entries and 4 MiB. Pending cache writes are bounded to sixty-four entries and 16 MiB, plus the one active write. Disk eviction starts above 512 MiB and targets 384 MiB.
- Windows cancellation terminates the owned process tree, including executable launchers. Tests require permission to terminate their own descendants; a restricted sandbox may deny this operation.

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
cargo test --offline app::assets::media::integration_tests::real_ -- --ignored --test-threads=1 --nocapture
cargo test --offline benchmark_transport_and_decoder_threads -- --ignored --test-threads=1 --nocapture
cargo clippy --offline --all-targets
cargo check --offline --features profiling
```

Set `LWA_THUMBNAIL_BENCHMARK_SOURCE` to benchmark an existing video. `benchmark_external_video_overview` reports coarse/complete/persisted timing, disk reload, process count, decoded memory, and cache bytes. Set `LWA_THUMBNAIL_ARTIFACT_DIR` to an existing directory for a generated overview contact sheet. Profiling counters expose scheduler queue wait, subprocess time, process-permit wait, output bytes, sample count, cache hits, and dropped/coalesced/failed writes.
