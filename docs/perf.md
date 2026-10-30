# Performance baselines

Run in release mode (fixtures are created once in the temp dir and reused):

```
cargo test --release perf_ -- --ignored --nocapture --test-threads=1
```

`LWA_PERF_N` overrides the flat-directory size for `perf_listing_pipeline`.
Windows 11, NTFS SSD. "Before" = after Phase 0 build settings only.

| Measurement | n | Before | After |
|---|---|---|---|
| `read_directory` depth 5 (deep search) | 39.8k | 672 ms | **7.7 ms** |
| Persistent cache hit (sled get + decode) | 20k | 0.61 ms ¹ | **0.27 ms** |
| Uncached read_dir + build | 20k | 12.9 ms | 15.5 ms ² |
| Sort Name / Size / Modified | 20k | 0.55 / 0.50 / 0.71 ms | **0.13 / 0.03 / 0.32 ms** |
| Filter, ASCII query | 20k | 6.0 ms | **0.19 ms** |
| Filter, Unicode query | 20k | 16.0 ms | **0.16 ms** |
| Rows materialised / borrowed (all) | 20k | 0.31 ms | **0.05 ms** |
| List frame, nothing selected | 100k | 0.34 ms | 0.34 ms |
| List frame, all selected | 100k | 4.7 ms | **0.51 ms** |
| Icons frame, all selected | 100k | 4.4 ms | **0.32 ms** |
| JPEG thumbnail, 12 MP → 160 px | 1 | 44 ms | **23 ms** |

¹ Previously the copy out of the in-memory cache alone; a hit is now an `Arc` clone.
² Within noise of the old path; this read also builds the collation keys that
are persisted with the cache entry.

## Decisions taken from measurements

- **Persistent cache kept on sled.** A hit (0.27 ms) is ~60× faster than a cold
  read; sled's own get adds ~13 µs, so replacing it (redb/rkyv) would not be
  measurable.
- **Windows Shell thumbnails only for video posters.** For images a cache-miss
  probe costs ~7 ms and a cold generation ~90 ms, versus 23 ms for the
  DCT-scaled decoder; for video it replaces an ffmpeg process. The quick
  workers only read the Shell's cache (~7 ms); generation runs on the video
  worker with a 3 s timeout, and its result is cached by us.
- **List rows use 64 px thumbnails.** Rows draw icons at most 24 points tall;
  they previously decoded up to 360 px (about 0.5 MB of texture per image at
  ExtraLarge), now 64 px (16 KB). Decode time is unchanged (DCT-scaled decode
  still ~23 ms for a 12 MP JPEG after adding EXIF orientation).
- **Cached thumbnails keep transparency.** The cache format follows the
  pixels: fully opaque thumbnails are JPEG, anything with alpha is PNG.
- **List view kept on egui_taffy; asset keys kept as strings.** The 100k-entry
  frame is ~0.3 ms, so a hand-painted rewrite (which changes pixels and
  snapshots) or integer keys would save microseconds.
- **Asset scheduler heap kept.** The queue is capped at 512 jobs, so its O(n)
  rebuilds are microseconds.

## Native enumeration and watcher pass

Directory reads on Windows go through `src/fs_enum.rs`: one
`NtQueryDirectoryFile` call fills a reused 64 KiB buffer with
`FILE_ID_EXTD_DIR_INFORMATION` records, and names are decoded straight into
the listing arena. `std::fs::read_dir` remains the fallback (and the only path
off Windows). Same machine and fixtures as above.

| Measurement | n | Before | After |
|---|---|---|---|
| Enumerate only (count entries) | 20k | 9.0 ms (std) | **3.7 ms** |
| Uncached read_dir + build | 20k | 15.5 ms | **8.3 ms** |
| `read_directory` depth 5, app pool | 39.8k | 26.9 ms (2 threads) | **10.1 ms** (8 threads) |
| `read_directory` depth 5, global pool | 39.8k | 7.7 ms | 7.3 ms |

The deep-walk rows on the app pool are both measured on the new reader; the
only difference is the pool size. Earlier "deep search" numbers ran on the
global rayon pool, which the app does not use for walks.

## Decisions taken in this pass

- **Bulk NT enumeration, not the MFT.** File Pilot's speed comes from plain
  Win32/NT enumeration, multithreaded walks and caching; reading the `$MFT` and
  tailing the USN journal (what Everything does) needs administrator rights and
  only works on NTFS. It stays a possible opt-in index for drive-wide search.
- **Attributes are kept** (`EntryFlags` on every row, plus the file id).
  "Hidden" now follows Explorer: the hidden or system attribute, as well as the
  existing `.`/`$` name rule. With hidden files off, deep searches skip
  `AppData`, `ProgramData`, `$RECYCLE.BIN` and similar. The cache format
  version was bumped, so old records are re-read once.
- **Walk pool sized to the machine** (`available_parallelism`, clamped to
  2..8) instead of a fixed 2 threads. It stays separate from the global rayon
  pool, which the UI thread uses for sorting and filtering.
- **No second walk on the UI thread.** Directory symlinks to watch come from
  the background walk's own loop-protection set instead of a `walkdir` sweep
  run on every refresh of a deep single-root search.
- **Watcher events are surgical on Windows.** notify's Windows backend reports
  creates, deletes and modifications as `Any` and sets no rename tracker;
  these used to be dropped (creates, deletes) or turned into full folder
  re-reads (every write). They now map to row edits, untracked renames pair by
  adjacency, and each path's metadata is read once on the watcher thread
  instead of up to three times on the UI thread.
