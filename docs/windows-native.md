# Native Windows client for DirFleet (WinUI 3, pure Rust)

## Context

The `rewrite` branch moved DirFleet to a Crux core (`crates/dirfleet_core`) with Rust effect
handlers (`crates/dirfleet_effects`) and a native macOS shell (`apple/`, SwiftUI + AppKit).
Windows users still get the egui app, which does not run on the core yet. This plan adds a
native Windows shell with feature parity with the macOS app, modelled on
`E:\PROJECTS\wihaister_full\client_windows`: pure Rust, WinUI 3 through Microsoft's
`windows-reactor`, core linked as a normal Cargo dependency, unpackaged and self-contained.

What makes this tractable:
- Only 6 effects reach a shell (`Render`, `PickFolder`, `PickApp`, `OpenWindow`, `QuickLook`,
  `QuitApp`); `Handlers::handle` (`crates/dirfleet_effects/src/lib.rs:236`) does the rest, and
  Windows implementations already exist (listing, drives, clipboard, Recycle Bin, thumbnails).
- A Rust shell uses typed `Event`/`ViewModel` directly, exactly like `src/app/core_link.rs`.
  No FFI, no bincode, no codegen.
- The view model is already viewport-windowed (`TabViewModel.rows` + `Event::ViewportChanged`).

Decisions taken with you:
- **Reactor gaps** (no scroll-to/scroll observation, no TextBox selection, no XButton/wheel):
  carry a **pinned fork** of `microsoft/windows-rs`.
- **Quick Look**: own preview window hosting **shell preview handlers**, images drawn directly.
- **Explorer shell menu**: in a **"More options" submenu**, loaded lazily.

## Layout

```
windows/                      # new crate `dirfleet_windows`, bin `DirFleet` (parallels apple/)
  Cargo.toml  build.rs        # windows_reactor_setup::as_self_contained() + icon (winresource)
  AGENTS.md                   # module map, threading rules, reactor gotchas
  src/
    main.rs                   # args (--state-file, --folder), tracing, App::run_application
    core_link.rs              # EffectRouter + Handlers, stale flag, shell-effect queue
    dispatcher.rs (+generated)# DispatcherQueue bindings, copied from wihaister
    app.rs                    # DirFleetApp: owns core, declares one window per WindowView
    store.rs                  # per-part Rc diffing of the ViewModel (mirror of ViewStore/TabStore)
    window/                   # WindowRoot component, title bar, HWND subclass, dialogs, toasts
    pane/                     # split layout, TabView, header (nav, progress), folder states
    files/                    # list, grid, row, columns, viewport, selection, rename, keys,
                              #   rubber_band, drag (out), drop (in)
    menus/                    # model (MenuEntry builders), popup (HMENU), commands (CommandId→Event)
    sidebar.rs pathbar.rs goto.rs search.rs inspector.rs status_bar.rs palette.rs
    batch_rename.rs thumbnail_picker.rs activity.rs shortcuts.rs
    settings/  plugins/       # 5 settings pages; plugin bar, form, results
    quicklook/                # preview window
    icons.rs thumbnails.rs    # shell icon cache, thumbnail decode queue
    theme.rs tokens.rs ui_compat.rs
crates/dirfleet_winshell/     # new: Win32 helpers shared with the egui app (windows 0.59)
```

- `windows/` joins `[workspace] members` but **not** `default-members`, so `just validate`
  on Linux/macOS is untouched.
- `crates/dirfleet_winshell` receives the Win32 code now inside the egui crate, re-parameterised
  to take an owner HWND as `isize` (types do not cross `windows` 0.59 ↔ the reactor's git
  `windows`): `src/windows_tools/context_menu.rs` (IContextMenu host), `src/windows_tools.rs`
  (`pick_folder`, `pick_program`, `open_properties`, `open_with`), and the data-object half of
  `src/native_drag.rs`. The egui crate then depends on it instead of its own copies.
- Unsafe stays per-module with `#![expect(unsafe_code, reason = …)]`; workspace lints apply.

## Reactor fork

Fork `microsoft/windows-rs` at the rev wihaister pins (`e977232…`), branch `dirfleet`, pin by rev
in `windows/Cargo.toml`. Entry point is `crates/tools/reactor/src/schema.toml` plus adapters in
`crates/libs/reactor/src/native/winui/`. Keep the patch small and upstreamable:

| Must have | Why |
|---|---|
| `ScrollViewer`: `on_view_changed` (offset, viewport, extent) and an `ElementRef` request for `ChangeView` | exact `ViewportChanged`, `scroll_to`, restore on Back |
| `TextBox`: select-range request | rename with the stem preselected |
| Pointer info: XButton1/2; a wheel event on `Border` | mouse back/forward; Ctrl+wheel icon size without a global hook |

Nice to have, only if cheap: `TabViewItem` icon, `Image` decode size, public menu-at-point.

## Architecture

**Core link** (`core_link.rs`, from `src/app/core_link.rs`): `EffectRouter<DirFleet, WinRoutes>`
with `Handlers::new(&router, Some(state_file))`; state at
`%APPDATA%\io.github.leinnan.dirfleet.native\state.json` (separate from egui, as on macOS).
Handler threads set a `stale` flag and enqueue at most one dispatcher callback
(`TryEnqueue`), which posts an app message. Shell effects are queued and handled on the UI
thread; `PickFolder`/`PickApp` answer through `router.resolve_request`. Startup sends
`Event::Started { open, home }`.

**App and windows** (`app.rs`): `App::run_application::<DirFleetApp>`; `view()` declares
`.window::<WindowRoot>(id, WindowInput { view: Rc<WindowView>, globals: Rc<Globals> })` per
`ViewModel.windows`. `OpenWindow` → `show_window(id)`; a key that disappears closes its window;
`WindowRoot::drop` sends `WindowClosed`. `on_window_placement` → `WindowFramed`,
`initial_placement` ← `WindowView.frame`, HWND subclass for `WM_ACTIVATE` → `WindowFocused`.

**Diffing** (`store.rs`): after each render take `router.view()` and reuse the previous `Rc`
for every part that compares equal (window, pane chrome, counts, inspector, rows), so reactor's
`PartialEq` input check skips unchanged components. Add `PartialEq` derives to core view types
where missing.

**File list** (`files/`), the performance-critical piece:
- `ItemsRepeater::virtual_source` with `len = total_rows` (grid: rows of N tiles), **row index
  as key**, `VirtualSource` memoised and rebuilt only when rows/selection/cursor/columns change.
- One `Row` component per realized index with a `PartialEq` input; hover state lives in the row.
  Indexes outside `[first_row, first_row + rows.len())` draw a fixed-height placeholder.
- Viewport from `on_view_changed`: `first_row = offset / row_h`, coalesced and deduped →
  `ViewportChanged`. `scroll_to`/`scroll_count` → `ChangeView`.
- Selection is drawn from `TabViewModel.selection` (binary search over `RowRange`); input sends
  `Click`, `MoveCursor`, `SetSelection`, `TypeSelect`, `ContextClick`, `HoverRow`.
- Columns: hand-built header (sort click, edge drag → `SetColumnWidth`, right-click → `ToggleColumn`).
- Icons: shell icons on a worker thread, cached per extension and per path (exe, lnk, folders),
  delivered in memory. Thumbnails: decode off-thread, LIFO priority, byte-bounded LRU, cancel on
  recycle (mirror of `apple/DirFleet/ThumbnailLoader.swift`).

**Menus** (`menus/`): port the `MenuEntry` builders from `apple/DirFleet/FileMenus.swift` (row,
trash row, empty area, transfer, view options, folder actions) and the `CommandId → Event` map
from `apple/DirFleet/Core.swift:751-811`. Show with `TrackPopupMenuEx` from the dispatcher, opened
on the render that follows `ContextClick` so items match the new selection. "More options" is a
submenu filled on `WM_INITMENUPOPUP` by `dirfleet_winshell::context_menu` (ids 1..0x7FFF; app ids
from 0x8000). Dark mode as in wihaister's `popup_menu.rs`.

**Drag and drop**: in via `Border` drop policy (`StorageItems`), modifiers from
`GetAsyncKeyState` → `DropItems { copy, force_move }` / `DropOnTrash`. Out via the shell data
object (`dirfleet_winshell`) plus wihaister's drop source, `LoopPulse` and drag-threshold gesture
(`client_windows/src/drag_out/{start,drop_source,gesture}.rs`). Internal drags ride the same path.

**Quick Look** (`quicklook/`): a plain Win32 window; images/GIF decoded directly, otherwise
`IPreviewHandler` from the file's registered handler, else large icon + details. Space toggles,
arrows step through `QuickLook.paths`, Enter sends `OpenPreviewed`.

**Windows conventions instead of Mac ones**
- Tabs: the core's per-pane tabs in a `TabView` (`NewTab`, `CloseTab`, `ActivateTab`, `MoveTab`);
  panes render the whole `FlatLayout` with draggable splitters (`ResizeSplit`).
- No menu bar: toolbar "More" menu, context menus, palette and shortcuts cover every command.
- Shortcuts are bound by parsing `CommandView.shortcut` (non-Mac table in
  `crates/dirfleet_core/src/app/palette.rs:196-231`), so labels and bindings cannot drift.
- Put Back is shown (`trash.can_restore`); the Full Disk Access UI is hidden
  (`access.disk_access_applies` is false).

**Small shared changes**
- `crates/dirfleet_effects/src/system.rs`: `GetInfo` on Windows opens the Properties dialog
  (today it returns "not available here yet").
- `crates/dirfleet_core/src/app/palette.rs`: per-OS wording ("Show in Explorer", "Properties",
  Recycle Bin).
- `docs/crux-migration/parity.toml` + `crates/dirfleet_ffi/tests/parity.rs`: add a
  `windows_test` key so the ledger tracks this shell too.

## Milestones (each ends runnable)

0. **Spike, go/no-go.** Fork with the ScrollViewer members; crate skeleton; core link; one
   window listing a 100k-entry folder with `ViewportChanged` and `scroll_to`. Measure first rows
   and scroll cost. Also settle the unverified points: `run_window` from `create()`, `TabView`
   inside `TitleBar.content`, animated GIF in `Image`.
1. **Shell foundation.** Multi-window, all 6 shell effects (Quick Look stubbed), frame/focus/close
   events, state restore, Mica + appearance setting, title bar.
2. **Browsing.** Splits, tabs, header nav, path bar (crumbs, folding, anchor and child menus),
   list with columns, icon grid (4 sizes), selection, keyboard, type-select, open, icons,
   thumbnails, animated previews, hidden toggle, empty/denied/error states, status bar.
3. **Sidebar and navigation.** Sections (collapse, reorder), favorites, drives with capacity and
   eject, Recycle Bin, recent, saved searches, Go To sheet, mouse back/forward.
4. **File operations.** Every context menu, clipboard, inline and batch rename, new folder/file,
   duplicate, trash/delete/put back/empty, Move To/Copy To, conflicts, undo/redo, activity,
   progress, toasts, confirmations, drag and drop, "More options", Properties, Open With.
5. **Search, inspector, Quick Look, palette, shortcuts list, settings, saved-search editor.**
6. **Plugins and media.** Plugin bar, form (13 field kinds), results sheet, settings page,
   plugin shortcuts, video thumbnail picker.
7. **Ship.** Accessibility names, perf pass, parity ledger, CI job, MSI (WiX, runtime DLLs beside
   the exe, as in `wihaister_full/packaging/windows`), `docs/windows-native.md`.

Stretch after parity: rubber-band selection, Fluent context menus, tab tear-out.

## Verification

- `justfile` recipes: `windows-check`, `windows-run`, `windows-test`, `windows-perf`,
  `windows-package`; a Windows CI job runs check, clippy `-D warnings` and tests.
- Unit tests for pure logic: viewport and grid math, `RowRange` lookup, menu builders, shortcut
  parsing, command mapping, split layout, crumb folding.
- Integration test in the style of `crates/dirfleet_effects/tests/router.rs`: real
  `EffectRouter` + `Handlers` on a temp folder with a fake shell answering the 6 effects.
- Perf harness matching `apple/DirFleetTests/PerfTests.swift` scenarios (Desktop-like, 10k,
  100k) against the gates in `docs/crux-migration/perf.md`, with tracing spans from day one.
- Manual pass per milestone against `docs/crux-migration/parity.toml`, driving the running app;
  `just parity` stays green.
- Packaging smoke test: the MSI installs and runs on a machine without the Windows App Runtime.

## Risks

- **Reactor is a preview** and has removed public API between revs: pin the fork, keep all
  reactor calls behind `ui_compat.rs`, and keep the egui app shipping until parity.
- **WinUI-side cost at 100k rows is unmeasured** anywhere: milestone 0 exists to measure it
  before the rest is built. Fallback for the list alone is a Direct2D-drawn surface
  (`windows-canvas`), as wihaister does for its terminal.
- **Modal Win32 loops under WinUI** (menus, OLE drag) must run from the dispatcher, never inside
  a reactor callback; never block the UI thread on a WinRT async.
- **First build needs network** (`windows-reactor-setup` downloads the runtime, ~59 MB staged);
  arm64 is untested.

## Status

**Milestones 0 to 3 and 6 are built, 4, 5 and 7 mostly.** `just parity` counts the shell
at 184 of the ledger's 227 wanted features.

The reactor fork is published (<https://github.com/Leinnan/windows-rs>, branch `dirfleet`)
and pinned by rev in `windows/Cargo.toml`.

| Milestone | State |
|---|---|
| 0 Spike | Done. 100,000-file folder, release build: first rows 0.49 s after launch, view model 0.26 ms per jump, 135 MB. |
| 1 Shell foundation | Done: multiple windows, close and quit, frames saved and restored, activation, appearance, folder and app pickers, shortcuts from the core's table, the More menu. |
| 2 Browsing | Done: split panes with splitters, tabs (in the title bar while the window has one pane, above each pane when it has several), header, path bar with folding and folder lists, list with sortable, resizable, optional columns, icon grid in four sizes whose tiles share the line's width, thumbnails, shell icons, keyboard, empty and error states, status line. |
| 3 Sidebar and navigation | Done: sections that fold, places, drives with capacity and eject (with its confirmation), the Recycle Bin, Go to Folder, mouse Back/Forward. Reordering favorites and sections is not there yet. |
| 4 File operations | Mostly: context menus for the selection, Recycle Bin items and the folder (native popups, Explorer's menu behind "More options"), inline rename, batch rename, trash/delete/empty with confirmations, conflicts, Move To/Copy To, operation progress with Stop, toasts, files dropped in with the move-or-copy question. Missing: dragging out, dropping on a folder row or a sidebar place, rename in the icon grid, the activity list, menus on sidebar rows and column headers, dark menus. |
| 5 Search, inspector, Quick Look, settings | Mostly: search box with filters, options and Stop, the inspector, the command palette, the shortcuts list, Settings, Quick Look for images and text. Missing: highlighted matches, saving and editing searches, Explorer's preview handlers in Quick Look. |
| 6 Plugins and media | Done: the plugins' bar under the folder, forms (all field kinds; a file's path is typed, a folder's can be picked), results, jobs with Stop, inspector sections, actions in menus with their confirmation, shortcuts, the Plugins part of Settings, and the video thumbnail picker. |
| 7 Ship | Mostly: names for screen readers on the controls that had none, a perf measurement (`just windows-perf`), the ledger's `windows` column, a CI job (lint and tests), and an MSI (`just windows-package`). Missing: a perf run that drives the window itself, a pass with a screen reader, the MSI in the release workflow, signing, arm64. |

Shared changes made along the way:
- `crates/dirfleet_winshell`: the Win32 shell helpers (pickers, Properties, Open With,
  Explorer's context menu, drag data object), moved out of the egui app, which now uses them.
- `dirfleet_effects`: Get Info opens the Properties dialog on Windows.
- `dirfleet_core`: command names in Windows wording on Windows ("Recycle Bin", "Explorer",
  "Properties").
- `docs/crux-migration/parity.toml` and its check: a `windows` status per feature, with
  `windows_test` naming the shell's unit tests. The shell has no UI tests, so "done" there
  means built on the core, not proven by a test.

Checked by driving a debug build (`windows/tools/`), on scratch files where anything was
changed: a second window, frames restored after relaunch, splits, tabs (adding, closing the
shown one), icons, sorting, a column toggled, a crumb's folder list, sidebar places, Go to
Folder, arrow keys, Enter, Backspace, End, Escape, Delete to the Recycle Bin with its toast,
F2 rename committed to disk, the selection's menu and Duplicate from it, Select All from the
palette, the batch rename dialog, Settings with its Plugins part, live search, the
inspector, Quick Look on a text file, an animated GIF playing in a tile, and the Git plugin
in a scratch repository: its bar, the Commit form filled and submitted, the commit made and
its toast. The thumbnail picker was checked by hand.

Not checked, because pointer input cannot be driven without a person: clicking and
double-clicking rows, Ctrl/Shift clicks, right-click (the menu was opened with the Menu
key), dragging splitters and column edges, tab reordering, hover previews, the extra mouse
buttons, dropping files in. Chords with Ctrl, Shift or Alt were not sent either. The
pickers, the conflict dialog, Move To/Copy To, Explorer's menu, a plugin's results, its
jobs, its inspector sections and its shortcuts were not opened.

Measured by `just windows-perf` (the core and its handlers without a window, debug build):
100,000 entries listed in 0.4 s with 150 rows sent, a jump anywhere in the folder answered
in 2 ms, of which taking and splitting the view model is 0.4 ms.

The MSI (`just windows-package`) is 33 MB: an 11 MB exe that needs no C runtime, and 58 MB
of Windows App Runtime beside it. It passes `wix msi validate`, and the app unpacked from
it (`msiexec /a`) opens its window. It has not been installed for real, nor tried on a
machine without the runtime. In the first packaging run the unpacked app ended with code
0 before opening a window, once, while other builds were loading the machine; it did not
happen again in five more starts and is not explained. The script now prints what the app
wrote if it does.

Known gaps:
- Icons are by kind of file; programs, shortcuts and folders with their own icon show the
  generic one.
- Context menus draw light in a dark app.
- Explorer's menu and "More options" only cover selected rows the core has sent.
- A plugin's file field has no picker; only folders have one.
- 26 tests of `dirfleet_core` and one of `dirfleet_effects` fail on Windows for path
  separators; they failed before this work. `just validate` in CI's Windows job runs them.
