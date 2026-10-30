**Animated thumbnail implementation — 30 September 2026**

The requested overview behavior is preserved: videos show a sequence of moments from across the source. The pipeline now extracts full-color frames directly, uses sparse seeks for longer videos, and bounds generation, result queues, playback memory, and disk storage. This implements the first phase of the [analysis and plan](animated-video-thumbnails.md).

**What the user sees**

- An overview contains up to 12 moments from 5% through 90% of the video, in chronological order, with a 250 ms hold per moment. Long-video generation publishes four widely distributed moments before completing the other eight.
- Hovering for 250 ms starts playback and generation. Passing over a tile or opening its context menu does not start an overview. Window focus loss pauses playback and cancels preview work.
- Selection animation is off by default. The new “Animate selected videos and GIFs” setting enables one selected preview across all visible panes. Existing saved settings deserialize with this default.
- Posters and animation use bounded physical-pixel tiers of 160, 240, 360, and 480 pixels, according to the displayed area and screen density. List/sidebar requests can retain their smaller sizes. Portrait, rotation, and sample aspect ratio are respected.
- A deferred texture update keeps the last displayed frame visible. Each animation reuses one texture instead of accumulating one texture for every frame.
- GIF previews keep the total timing represented by their first 120 source frames, including very short delays and long holds, while reducing the retained sequence to at most 12 frames.

**Generation and scheduling**

The asset facade and scheduler remain in `src/app/assets.rs`. `assets/process.rs` supervises subprocesses, `assets/media.rs` handles probing, sampling, frame bundles, and background compression, and `assets/cache.rs` handles disk storage and eviction. Real-media checks live in `assets/integration_tests.rs`.

FFprobe selects a usable video stream, skips attached cover art, validates duration and source dimensions, and detects PQ/HLG transfer metadata. If the sidecar distribution omits FFprobe, a bounded FFmpeg input-header probe obtains duration and the video stream index. Missing duration produces one beginning frame rather than assuming an arbitrary duration; this fallback is not persisted as a complete whole-video overview.

Videos longer than eight seconds use independent input seeks. Each sample yields to the shared video scheduler so a newly hovered item can outrank an unfinished selected preview. Short sources use one bounded sequential extraction process, avoiding 12 process starts. The first overview sample and poster share the same timestamp contract, allowing reuse from memory or the versioned overview bundle without another probe or decode process. Half-second sources no longer seek past the end.

Posters, probes, and video overview samples share one video worker and a global process permit. Decoder, filter, and encoder thread counts are explicitly limited to one. Subprocess stdout is bounded, stderr retains at most 16 KiB while continuing to drain, and cancellation/deadline expiry kills and reaps the direct child. Probes and independent samples have two-second deadlines; a video task has an eight-second deadline from its first execution, including time between scheduling steps. The short sequential process uses that task deadline. Poster generation has a five-second overall budget, with two-second process stages.

Request attempt IDs are separate from source/cache identity. Every frame collects the assets still desired by all visible panes and cancels obsolete requests. Focused-directory changes adjust priority without invalidating another pane's requests. Workers revalidate the source's size and nanosecond modification revision before publication; late results cannot clear a newer attempt. Exact invalidation also handles filenames containing `#`.

Errors distinguish cancellation, timeout, unavailable tools, invalid media, and resource limits. Backend preparation/download runs in the background, retries at most once per 30 seconds, and clears unavailable-tool failures after recovery. A cancelled partial preview can resume generation on the next stable hover; failed completion can retain the four-moment preview while retry backoff applies.

**Memory and cache limits**

| Area | Policy |
|---|---|
| Queued/running requests | At most 512 per scheduler; continuations retain their reserved slot |
| Result delivery | Four messages, with a shared 16 MiB decoded-byte budget |
| One decoded overview | At most 12 frames and 12 MiB of RGBA pixels |
| Resident decoded animations | 32 MiB and 64 entries |
| Resident animation texture payload | 16 MiB; inactive textures released after two seconds |
| Static texture payload | 128 MiB and 512 entries |
| Shared first-sample/poster buffers | Four MiB and 16 entries |
| Background cache writer | Two queued jobs plus one being processed; shared frame buffers |
| One overview bundle/read | 16 MiB encoded limit, validated dimensions/counts/timing/checksums |
| Disk storage | Prune above 512 MiB toward 384 MiB, every 60 seconds or 16 MiB written |

These are payload/queue limits, not a hard ceiling on application or child-process RSS. Image codecs, FFmpeg, native icon APIs, temporary buffers, and graphics backends have additional overhead. FFmpeg's 128 MiB `max_alloc` bounds an individual allocation, not its total memory use. Texture update timing measures the application's copy/enqueue work; it does not measure completed GPU execution. Upload work is limited to two milliseconds and four textures per UI frame, with at most three static uploads.

Video extraction passes full-color PNG frames to Rust. The version-six overview bundle stores JPEG frames at quality 90 for opaque video and PNG frames for GIF sources. It records source revision/size, resolution specification, frame dimensions, playback delays, source positions, lengths, and checksums. Video cache identity also incorporates the resolved FFmpeg executable's path, size, and modification time. Corrupt bundles regenerate; unchanged disk hits start no FFmpeg or FFprobe process. Old thumbnail and animation formats are retired by recurring maintenance.

Overview and video-poster compression happens on a separate bounded writer, outside the generation lane. It rechecks the source before and after encoding and publishes atomically. If its queue is full, persistence is skipped without blocking display. Cache reads update recency for eviction during the current session; modification time is the fallback across restarts. Temporary files older than an hour are removed. LRU accounting now includes entries evicted by item capacity as well as replacements and byte limits.

**Measured results**

Three sequential trials compared the original and new Rust generation/decoding paths on identical synthetic H.264 sources: 640×360, 30 fps, two-second GOP, local macOS ARM64, FFmpeg 8.1.1, development application profile with optimized dependencies. Preview edge was 240 pixels. The 60- and 600-second fixtures repeat a 20-second segment using stream copy. Raw trials and methodology are in [thumbnail-implementation-benchmark.json](thumbnail-implementation-benchmark.json).

| Source | Original complete overview | New first four moments | New complete overview | New overview persisted | Warm disk decode |
|---|---:|---:|---:|---:|---:|
| 60 seconds | 471 ms | 153 ms | 418 ms | 450 ms | 5.9 ms |
| 600 seconds | 4,063 ms | 153 ms | 428 ms | 459 ms | 5.8 ms |

The ten-minute fixture's complete overview is **9.50× faster**; generation plus new cache persistence is 8.85× faster than the original generation/decode time, which excludes its cache write. The one-minute complete overview is 1.13× faster. These are median wall times, excluding UI dwell, queue wait, and texture uploads. They are not CPU/RSS measurements or a guarantee for other codecs, keyframe intervals, resolutions, storage devices, or release builds.

The new fixture bundle is about 128 KiB versus 56 KiB for the original palette GIF. Full-color storage improves gradients/detail at a compressed-size cost; recurring eviction controls disk growth. Decoded pixels are approximately 1.48 MiB in both implementations. The sparse cold path starts one probe and 12 sequential sample processes; it replaces continuous decoding through most of the video with short seeks.

**Verification**

- `cargo check --offline` and `cargo check --offline --features profiling` pass.
- `cargo test --offline -- --skip app::dock::tests::test_dock_view_` passes: 69 tests, four opt-in tests ignored. Deterministic checks cover priority/continuation handling, cancellation and child reaping, timeout/output caps, global process serialization, result byte pressure, stale attempt/source protection, dwell and selection limits, partial-preview resumption, cache corruption/eviction, GIF timing, LRU counters, and reusable/deferred texture behavior.
- `cargo test --offline real_ -- --ignored --test-threads=1 --nocapture` passes all three real-video tests. They cover a full-color 12-frame timeline, progressive four-frame publication, no-process disk/poster reuse, corrupt-cache regeneration, FFmpeg-only probing, half-second portrait video, background poster persistence, anamorphic geometry, rotation metadata, and detected HDR sources with the available conversion/fallback.
- The complete suite with graphics access reports 71 passing tests, eight snapshot mismatches, and four ignored tests. The same eight snapshots fail on the original HEAD with the preexisting widget edits preserved on this machine; two empty-view snapshots pass in both. Native macOS icons/reference rendering differ. Reference snapshot images were not updated.
- `cargo clippy --offline --all-targets` remains blocked by existing denied lints in unrelated application/database code and existing test helpers. It reports no denied lint in the new asset modules or settings changes. `git diff --check` passes.

The fourth opt-in test is a repeatable external-source benchmark. Set `LWA_THUMBNAIL_BENCHMARK_SOURCE` to a local video and run `cargo test --offline benchmark_external_video_overview -- --ignored --test-threads=1 --nocapture`. It reports coarse/complete/persisted/disk latency, process count, and retained bytes. Optional `LWA_THUMBNAIL_ARTIFACT_DIR` saves a labeled overview sheet from the real-video test. Profiling builds add scopes for probe/process/sample/cache/queue work and counters for subprocesses, samples, disk hits, stale/cancelled results, result bytes, uploads, and resident pixel payloads.

**Remaining validation limits**

HDR tone mapping is enabled when the installed FFmpeg exposes both `zscale` and `tonemap`. It reduces geometry before linear-light conversion, applies Hable tone mapping, and produces SDR RGB. The local FFmpeg lacks `zscale`, so the bounded fallback and HDR detection were tested; color accuracy of the conditional tone-map path still requires a supporting binary and representative HDR10/HLG sources. Unsupported builds log the limitation and still generate thumbnails.

Deadlines and cancellation are checked between bounded native decode/cache stages. They cannot forcibly interrupt a filesystem call stalled inside the operating system, or a native icon API. The existing sidecar downloader runs independently and is not cancelled with an individual preview. GIFs longer than 120 source frames intentionally show a bounded prefix; zero-delay frames use a 100 ms fallback.

Further tuning should use real 1080p/4K, HEVC/AV1, long-GOP, variable-frame-rate, corrupt and remote/external-drive sources, plus long browsing sessions. The eight-second sequential/sparse crossover, 250 ms hold/dwell, and resolution tiers are explicit initial policies. Scene scoring, blank-frame rejection, hardware decoding, and texture atlases remain optional follow-up experiments; the current implementation uses deterministic timeline coverage and a reusable texture.
