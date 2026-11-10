# Mac app performance

Measured with `just apple-perf` (`apple/DirFleetTests/PerfTests.swift`). The test runs on a folder shaped like a busy Desktop, built by `scripts/desktop_like_fixture.sh`:
- 847 entries;
- 723 screenshots (PNG, up to 3456×2234);
- 69 MOV and 22 MP4 screen recordings (H.264 and HEVC);
- JPEG and HEIC photos, app bundles and a few documents.

The app's own window content (`WindowContent`) is drawn in a real, off-screen window. The build is Release, without the main thread checker (scheme `DirFleet-perf`), on an Apple-silicon MacBook.

How to read the columns:
- **refreshes:** view models fetched by Swift.
- **unchanged:** fetched view models that equalled the last one, so SwiftUI was left alone.
- **longest stall:** the longest time the main run loop didn't turn. A main-queue heartbeat measures it every 4 ms; over 50 ms is a visible hitch.
- **settled:** the time until no view model came for 1.5 s, e.g. once thumbnails stop arriving.

## Baseline (2026-11-09, before the performance work)

| Scenario | Refreshes | Longest stall | Stalls > 50 ms | Other |
|---|---|---|---|---|
| Open, Icons, cold thumbnail cache | 168 | 1195 ms | 26 | listed 1.24 s, settled 10.5 s, 73/75 thumbnails |
| Open, Icons, warm cache | 260 | 1741 ms | 3 | listed 1.59 s, settled 2.5 s |
| Click in Icons (30 clicks) | 209 | 362 ms | 36 | p50 288 ms from click to the selection shown |
| Idle, 15 s | 7 | 5 ms | 0 | the drive poll every 2 s |
| Scroll the grid to the end | 1005 | 571 ms | 40 | 1544 events, 15.1 s for 10164 pt |
| Scroll the list to the end | 1741 | 524 ms | 81 | 1621 events, 22.2 s for 20223 pt |
| Click, list, 10 windows open | 34 | 286 ms | 33 | p50 179 ms, 461 KB per view model |

Fetching and decoding the view model is cheap: about 1 ms per refresh with one window, about 4 ms with ten. The time goes to SwiftUI re-evaluating and laying out the whole window on every change.

A 20-second `sample` of the clicks run (bare `FolderView`) put 67% of the main thread in `ViewGraphRootValueUpdater._updateViewGraph`. 35% was `ViewGraph.sizeThatFits`, called from `NSHostingView.updateConstraints → updateWindowContentSizeExtremaIfNecessary → minSize`: the window's minimum size is measured again after every change. The path bar's crumb folding (`PathBar.crumbs(_:folded:)`) and SF Symbol lookups (`CUICatalog namedVectorGlyph…`) showed up too.

## After step 2: one refresh per frame, none for an unchanged view

What changed:
- The FFI drops a render while the last one hasn't been fetched yet.
- Swift fetches at most once every 8 ms, and skips SwiftUI when the bytes are unchanged.
- Rows are sent in windows that move 10 rows at a time.
- Viewport reports are sent once per run-loop turn.

| Scenario | Refreshes (before) | Longest stall | Other |
|---|---|---|---|
| Open, Icons, cold | 27 (168) | 1216 ms | settled 10.2 s, 73/83 thumbnails |
| Open, Icons, warm | 6 (260) | 1390 ms | settled 2.3 s |
| Click in Icons | 37 (209) | 445 ms | p50 302 ms |
| Idle, 15 s | 9, all unchanged (7) | 5 ms | |
| Scroll the grid | 40 (1005) | 501 ms | 107 events (1544) |
| Scroll the list | 83 (1741) | 344 ms | 81 events (1621) |
| Click, list, 10 windows | 34 (34) | 258 ms | p50 189 ms |

The bridge now does little work, but the stalls are unchanged. Each click and each scroll step still costs SwiftUI hundreds of milliseconds re-evaluating and laying out the window, which is what step 3 is for.

## After step 3 (first part): no more measuring the path bar three ways

A profile of clicks (`sample`, 15 s) put 77% of the main thread in the path bar's `ViewThatFits`. It measured three crumb layouts, each full of buttons and menus, on every window layout, and nothing was cached. Making `PathBar` `Equatable` didn't help, because the measuring is layout work, not body work.

What changed:
- The bar now picks its crumbs from its width, with titles measured in the font.
- `core.view` became a store with one observed property per part.
- Grid tiles are `Equatable` views; their menu reads the tab in its own small view.
- Rows are sent a screen before and after the visible ones, moving half a screen at a time.

| Scenario | Before (step 2) | Now |
|---|---|---|
| Click in Icons, p50 | 302 ms, 30 stalls | **35 ms**, 0 stalls over 50 ms |
| Click, list, 10 windows, p50 | 189 ms | **50 ms** |
| Open, Icons, warm: settled / longest stall | 2.3 s / 1390 ms | **0.49 s** / 364 ms |
| Open, Icons, cold: longest stall | 1216 ms, 25 stalls | 491 ms, 5 stalls |
| Scroll the grid / the list | 16 s / 19 s | 5.6 s / 6.9 s |

Scrolling still stalls at every step. Each step brings in thumbnails, and every refresh then costs about 100 ms: the lazy grid re-measures its rows (`HVGrid.lengthAndSpacing`, `placeSubviews`, about a quarter of the main thread), new tiles are built, and JPEG thumbnails are decoded on the main thread when drawn. With no core updates at all, the same scroll takes 0.8 s with no stalls.

## After step 4: no image work on the main thread; a grid that places its own tiles

What changed:
- Thumbnails are decoded off the main thread with ImageIO, fully, so drawing them costs nothing; an `NSCache` holds the pixels.
- Icons are cached by file type. Folders, packages and apps get their own icon, looked up off the main thread.
- The icon grid places fixed-size tiles itself from the scroll offset (read from the `NSScrollView`), instead of `LazyVGrid`, which measured every row above the visible ones. It also follows the cursor now.
- The header, toolbar menus, status bar and sidebar get the tab without its rows and selection, and compare equal while those are all that changed.

| Scenario | Before (step 3) | Now |
|---|---|---|
| Click in Icons, p50 | 35 ms | 33 ms |
| Scroll the grid, warm (benchmark) | 5.6 s, longest stall 218 ms | 2.5 s, longest 85 ms |
| Scroll the grid at 60 pt a frame, warm | — | 0 stalls over 50 ms, longest 48 ms |
| Scroll the list, warm | 7.3 s, longest 124 ms | 4.5 s, longest 69 ms |
| Scroll the list at 60 pt a frame, warm | — | 3 stalls over 50 ms, longest 51 ms |

In the full run, scrolling right after a cold open still stalls: 59 times over 50 ms at 60 pt a frame. Video thumbnails from the cold open are still arriving through the single FFmpeg process, each one a refresh, while FFmpeg takes CPU. Step 5 is for that. A refresh during scrolling costs about 50 ms of SwiftUI work: graph updates, the hosting view re-measuring the window's size limits, and the list's table.

## After step 5: thumbnails from the system's frameworks

What changed:
- **Video posters on macOS** come from AVFoundation, 11–30 ms each with no process. Before, they took an FFprobe and an FFmpeg run in turn, through one process slot shared by the whole app. FFmpeg remains for files AVFoundation can't read, chosen-frame timelines and animated previews.
- **Images:** ImageIO writes the thumbnail for JPEG, TIFF, HEIC, HEIF and AVIF. It is faster for JPEG (12 ms against 19 ms for a 12 MP photo) and the only decoder for the rest, so HEIC files get thumbnails now. PNG stays with the portable decoder, which was faster: 24–39 ms against 37–59 ms for 2–4K screenshots.
- **Retries:** thumbnails that failed because FFmpeg was missing are asked for again once it is ready.
- **Animated previews** are encoded at GIF speed 10.
- **Refreshes while scrolling** are at least 150 ms apart, unless rows in sight are missing, so thumbnails arriving one by one don't each cost a SwiftUI update mid-scroll.

| Scenario | Before (step 4) | Now |
|---|---|---|
| Open, Icons, cold: every requested thumbnail done | 10.2 s | **2.1 s** |
| Scroll the grid, cold cache (flick / 60 pt a frame) | 37 / 52 stalls over 50 ms | 10 / 13–15 |
| Scroll the list, cold cache (flick / 60 pt a frame) | 81 / 46 stalls | 20 / 13–16 |
| Steps with rows not yet sent, while scrolling | — | grid 0; list 0 at 60 pt a frame, 4 of 84 at a flick |

## Step 6: measured, and left as it is

Going to another folder and back into the 847 entries takes 68 ms (p50), 167 ms at worst. Most of that is SwiftUI building the grid for the new rows; listing the folder is a small part. In every scenario, the core's update work on the main thread totals a few milliseconds: about 0.1 ms per event. Fetching the view model costs about 1 ms per refresh with one window and about 3 ms with ten.

So the core-side items planned for this step aren't changed yet, since none of them would move a measured number:
- a save counter instead of comparing the saved state on each event;
- post-passes only when their inputs change;
- `getattrlistbulk` listing;
- applying watch changes in place;
- a reopen cache;
- one timer thread.

They are worth revisiting for folders far larger than this one (100k entries), with a scenario that measures them.

## Step 7: less SwiftUI work per tile and per update

A profile of grid scrolling showed four costs that had nothing to do with what was on screen:
- The window's hosting view measured the whole window for its size limits after every change: 11.6% of the main thread (`NSHostingView.SizeConstraints.update`).
- The scroll view measured the grid's `ZStack`, which measured every tile.
- Every tile, files included, was a drop target. A file tile took twice as long to build as without it, though only folders and apps take drops.
- Every scroll step destroyed the row of tiles that left and built the row that came in.

What changed:
- **Window size by hand.** The hosting view's `sizingOptions` are empty, and the window gets a fixed minimum size (`WindowContent.minSize`).
- **A grid layout that knows its size.** `Pinned` returns its size without asking the tiles, and places each tile where the layout says.
- **Drop targets on folders and apps only.** A drop on a file goes to the folder around it.
- **Tiles reused.** The grid always draws a whole pool of rows and gives each tile the identity `index % pool`. So the row scrolling in takes the slots of the row scrolling out, and SwiftUI updates those tiles instead of building new ones. Tile placement went from 18.6% to 5.6% of the main thread in the profile.
- **One hover region for the grid.** An AppKit tracking area on the probe behind the tiles finds the tile under the pointer from the layout, so the tiles don't each track hover.
  - SwiftUI's own hover reported a pointer over the off-screen test window, where there was none. In the old code that sent about 70 `hoverRow` events per scroll and started previews.
  - `onContinuousHover` did the same once at opening.
  - The tracking area fires only on real pointer movement.
- **Column toggles.** While setting up, the list's table wrote its column customization back 14 times, before the core's view caught up. Each write toggled the four optional columns, 56 `toggleColumn` events at every opening. An odd count would have flipped columns the user never touched. Now only a visibility the user changed is sent.
- **The harness** scrolls within the scroll view's insets. It used to scroll the list sideways under the sidebar.

Two cold runs of each. The old code had its spurious `hoverRow` events taken out, so both sides do the same work:

| Scenario | Before | Now |
|---|---|---|
| Scroll the grid, flick: stalls over 50 ms / time | 9–11 / 1.52–1.62 s | **4** / **1.08–1.09 s** |
| Scroll the grid at 60 pt a frame: stalls | 7–10 | **2** |
| Go to another folder and back: p50 / worst | 93–152 / 253 ms | 80–87 / 240–245 ms |
| Click in Icons, p50 | 32 ms | 29–32 ms |
| Core updates while opening the folder | 59 | **3–4** |
| Scroll the list | 17–18 stalls | 15–17 stalls |

The list hardly changed. Its cost is SwiftUI's `Table`: rows sized automatically, one hosting view per cell. A fixed row height made no difference. Going further there means an `NSTableView` of our own.

Tests:
- The pool's slot reuse and the hover hit-testing are covered in `FolderViewTests`.
- So are hover through the tracking area (and none without a pointer), the window sized by hand, and the list drawn without column events.
- `DIRFLEET_PERF_ONLY=navigate` loops navigation for a profiler.

## Where it stood after step 7 (Desktop-shaped folder, Release)

| Scenario | Baseline | After step 7 |
|---|---|---|
| Click in Icons: selection shown, p50 | 288 ms | 29–32 ms |
| Click with 10 windows open, p50 | 179 ms | 50 ms |
| Refreshes while opening cold | 168 | 5–9 |
| Scroll the grid to the end (flick) | 15.1 s, 40 stalls | 1.1 s, 4 stalls |
| Scroll the list to the end (flick) | 22.2 s, 81 stalls | 2.7 s, 15–17 stalls |
| Idle 15 s | 7 refreshes | 8 fetches, all unchanged: no SwiftUI work |

Left then: the list (SwiftUI's `Table`), building tiles when the whole set changes, and the first window.

## Round two (2026-11-10): the rows leave SwiftUI

Everything in this round was measured while other work ran on the machine (load average 13–60 on 18 cores), so the absolute numbers are upper bounds. Each comparison alternates the build before the round ("before") with the new one; the differences are far larger than the noise. The tests now print the load (`PERF machine load_1m=…`).

### Step 8: measure what wasn't measured

- A click is timed until it is drawn (`drawn_p50_ms`), not until the view model has it.
- Launch with saved windows: the core's start, the window, the first rows drawn.
- Idle: CPU time, threads and memory next to the fetches.
- Flat folders of 10,000 and 100,000 files, as a list and as icons: open, select all, click, reverse, jump halfway, flick, Back with everything selected.
- `crates/dirfleet_ffi/tests/latency.rs` times views and events with 100,000 rows selected, in Rust.

What that showed, before any change:
- Each click cost 31–42 ms and each scroll step stalled, though only a tile or a row changed.
- In a folder of 100,000 files the list took 0.6 s per click, 1 s to select all, and stalled over 50 ms on 102 of 150 scroll steps.
- With 100,000 rows selected the core took 21 ms per view (70 ms with the inspector open).
- Idle, the drive poll made the app fetch its view every two seconds.

### Step 9: a store per window and per tab

Every view, every grid tile's menu and the menu bar's commands read one observed property, the view's windows. A click, a scroll step or a thumbnail in any window ran all of them again.

What changed:
- Each window and each shown tab has a store with separately observed parts: the tab without rows and selection (header, toolbar, search, menus), the counts (status bar), the inspector's part, and the rows.
- The windows as fetched stay available to handlers and tests, unobserved (`view.snapshot`).
- A click on a tile fetches the view in the same turn of the run loop.

| Scenario | Before | Store split |
|---|---|---|
| Click in Icons, drawn, p50 | 31–42 ms | 7–9 ms |
| Scroll the list (flick): stalls over 50 ms | 28–29 | 2–3 |
| Scroll the list at 60 pt a frame: stalls | 75–78 | 0 |
| Click, list, 10 windows, p50 | 57–84 ms | 20–30 ms |

### Step 10: the list and the grid drawn by AppKit

What changed:
- **The list** is an `NSTableView` (`FileList.swift`): rows of one height, reused cells placed by frame. It selects and sorts natively and tells the core, whose answer it then shows.
- **The grid** is a pool of tile views as big as what fits (`FileGrid.swift`), placed from the scroll offset with the same `TilePlacement`. A tile redraws only when what it shows changes. The core owns the selection; a click is drawn in the same turn.
- **Rows bypass SwiftUI.** `FolderController` gets the tab from its store (`TabStore.rows`) and updates only the rows in sight that differ.
- **Menus are data** (`FileMenus.swift`), built when they open. The same lists feed the menu bar, the toolbar, the sidebar and the path bar.
- **Thumbnails** load through `ThumbnailLoader`: each file once, the newest request first, cancelled when its cell is reused.
- The 150 ms wait between refreshes while scrolling is gone; a refresh no longer costs a SwiftUI update of the rows.
- New with it: arrow keys in the grid, dragging a selection of several items, the list's Opens column, widths from the core.

`FileViewTests` drive both views with mouse and key events in a window off screen.

### Step 11: large selections and large folders in the core

What changed:
- The selection counts its changes. Its ranges, the items plugins get and the inspector's summary are worked out once per change, not per view. Plugins are asked about a selection only when one has an action or a section for it.
- The focused tab is built once and copied into the window's `active`.
- Back into a folder, the items an operation made, and the cut items are found with sets.
- A watcher that starts, or a change that comes, while a folder is being read no longer starts another read at once: one more read follows the one under way.

In Rust, one window on 100,000 files with every row selected:

| | Before | Now |
|---|---|---|
| `view()` | 21 ms | 75 µs |
| `view()`, inspector open | 70 ms | 73 µs |
| A hover event, inspector open | 10.5 ms | 14 µs |

### Step 12: idle, and reading the view model

What changed:
- **Drives:** what is mounted is read cheaply every 2 s; the drives are listed in full when that changes, and every 30 s for free space.
- **One timer thread** answers every delay (hover, typed letter, toast); each used to start a thread.
- **A folder's watcher sleeps** until a change or its stop. Changes that don't settle are reported every second.
- **FFmpeg** is fetched on macOS when something first needs it, not at launch.
- **Thumbnails:** ImageIO's file is handed over without reading it back; half the cores make thumbnails, not four threads.
- **The view model is read in place.** The generated reader made an array for every number it read. `apple/Serde/BinaryDeserializer.swift` replaces it after the types are generated. A viewport change with ten windows on the 100k folder (event, view, decode) went from 5.2 ms to 1.6 ms.

## Where it stands

Desktop-shaped folder (847 entries), one run each on the same afternoon:

| Scenario | Before round two | Now |
|---|---|---|
| Click in Icons, drawn, p50 / p95 | 40.8 / 48.2 ms | **1.8 / 2.4 ms** |
| Click, list, 10 windows open, p50 | 85.6 ms | **5.8 ms** |
| Go to another folder and back, p50 / worst | 110 / 148 ms | 51 / 66 ms |
| Scroll the grid to the end (flick) | 1.18 s, 10 stalls over 50 ms | 0.76 s, none (longest 22 ms) |
| Scroll the list to the end (flick) | 3.06 s, 22 stalls | 1.51 s, none (longest 13 ms) |
| Scroll the list at 60 pt a frame | 50 stalls | none (longest 18 ms) |
| Idle 15 s | 8 fetches, 637 ms of CPU | **no fetch**, 109 ms of CPU |
| Open, cold: quiet after | 2.1 s (the drive poll's fetch) | 0.8 s |

Folder of 100,000 files:

| Scenario | Before, list / icons | Now, list / icons |
|---|---|---|
| Select all, drawn | 1002 / 69 ms | 50 / 53 ms |
| ⌘-click with everything selected | 716 / 68 ms | 44 / 32 ms |
| Click | 627 / 41 ms | 10 / 26 ms |
| Jump halfway down | 749 / 923 ms | 33 / 36 ms |
| 150 scroll steps: stalls over 50 ms | 102 / 109 | **0 / 0** |
| Back into it with everything selected | 6.7 / 8.3 s | 0.58 / 0.68 s |

In a folder of 10,000 files a click in the list went from 278 ms to 4 ms, and 97 of 150 scroll steps stalled before, none now.

What is left:
- **Launch.** With `DIRFLEET_LAUNCH_TIME=1` the built app prints its steps. Measured under heavy load: the core is made about 100 ms after the process starts and takes 1 ms; the window appears 165 ms later; the first rows are shown 70 ms after that and drawn some hundreds of milliseconds later. Nearly all of it is SwiftUI building the window: the toolbar bridge reshapes the window and lays everything out again. Our own code is a few milliseconds of it. The launch wasn't made faster in this round, and its numbers need a quiet machine.
- **Navigation, 51 ms.** A profile shows SwiftUI laying out the toolbar's items and the header again when the folder changes; the rows take about 6 ms of it. The path bar and toolbar would have to leave SwiftUI too.
- **The first read of 100,000 files** takes 0.5 s (one `lstat` per entry). `getattrlistbulk`, which the folder-size walk already uses, would about halve it.
- **Selecting all at 100k, 50 ms:** not profiled yet; the core's part is under a millisecond.
- **Not done from the plan, since nothing measured asked for it:** skipping the core's post-passes for scroll and hover events (an event costs 13 µs), sort keys only for the name sort, filtering before sorting, `mimalloc` in the bridge, a lower priority for thumbnail threads.
- **Rubber-band selection** in the grid and dragging rows the core hasn't sent need core events (`select.rubber_band` is open in the parity ledger).
