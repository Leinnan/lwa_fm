# Mac app ↔ egui app: feature parity

The SwiftUI macOS shell (`apple/`) is to reach full feature parity with the
egui app, built from native macOS controls rather than a copy of the egui
layout. Features move into the shared Crux core one at a time, for both apps
together: logic into `crates/dirfleet_core`, I/O into `crates/dirfleet_effects`.

The item-by-item record is [`parity.toml`](parity.toml), checked by
`just parity` (`crates/dirfleet_ffi/tests/parity.rs`). This page is the
comparison it was built from, written on 2026-11-08 at `5a9ac7c`, before the
P0 groundwork (windows, an Open Folder picker and saved windows in the core).

## Where each app stands today

| | egui app | Mac app |
|---|---|---|
| Size | about 52k lines of Rust in `src/`; 58 UI tests, 31 plugin tests, 14 file-op tests | 145 lines of Swift (`ContentView`, `Core`, `DirFleetApp`), 2 XCTests |
| Uses the Crux core? | **No.** It only borrows *types* (`CurrentPath`, `TabId`, `WindowId`, `history`, `search`, `fileops`, `view_settings`). Nothing in `src/` imports `dirfleet_effects` or `crux_core`. | Yes, through `CoreFfi` (`crates/dirfleet_ffi/src/lib.rs`) and the spike app (`crates/dirfleet_core/src/app/mod.rs`) |
| What the core can do | — | Navigate, open a folder row, go Back, Refresh, report the viewport, re-list on a watch event. Only `Render` reaches the shell. |

**What this means:** parity is almost entirely core work. Of the roughly 200 user-visible features in the egui app, the Mac app has 6:
1. type a path;
2. list a folder (name, size, modified);
3. double-click a folder to open it;
4. Back (⌘[);
5. loading and error text;
6. a live refresh when the folder changes.

Every other feature first has to move out of the egui crate into `dirfleet_core` (logic) and `dirfleet_effects` (I/O). That is the same work the old Step 3 planned. Here it is done one feature at a time, for both apps together.

## Feature by feature

Legend:
- "Logic today" is where the code to reuse lives.
- **C** = pure, goes to `dirfleet_core`.
- **E** = does I/O, goes to `dirfleet_effects`.
- **M** = mixed with egui and needs splitting.
- **S** = drawing only.

| # | Area (egui features) | Mac today | Logic today | Native mapping for the Mac shell |
|---|---|---|---|---|
| 1 | **Listing.** List view with 8 columns (Name, Folder, Modified/Deleted, Created, Opened, Opens, Kind, Size); sorting, resizing and showing/hiding columns; grid view in 4 icon sizes; 8 sort orders + Reverse, folders first; hidden files (`.DS_Store` and `._*` never shown); packages shown as one item; per-folder view settings falling back to global ones; streamed partial results with progress text; empty, permission-denied and Trash-is-empty states | Name/size/modified only; plain `std::fs::read_dir`; lowercase sort | `dir_handling.rs` (ICU `COLLATER`, sort), `fs_enum*` E, `database.rs` E (sled cache), `data/files.rs` C+E, `dock/columns.rs` C, `directory_view_settings.rs` C | `Table` + `TableColumnCustomization` (sort, resize, hide; macOS 14), `LazyVGrid` for the grid, `ContentUnavailableView` for empty states, "Open Privacy Settings" button |
| 2 | **Selection & keyboard.** Click, ⌘-click, ⇧-click, rubber-band select; arrows, Home/End, PgUp/PgDn; ⌘A, invert (⌘⇧I), Esc; type-to-select; Enter opens (several folders → new tabs); a cursor separate from the selection; new, renamed and pasted items selected afterwards | none | `egui_mev_core::Selection` C, `dock/interaction.rs` S/M | `Table(selection:)` and the grid's own selection send `SelectionChanged`. AppKit handles arrow keys and type-select; the core keeps selection and the auto-select rule. |
| 3 | **Quick Look.** Space toggles; arrows step through items; Enter opens | none | `dock/quick_look.rs` S (an egui overlay) | System `QLPreviewPanel` via a shell-lane `QuickLook { paths, index }` effect; the panel's arrow-key moves are reported back to the core |
| 4 | **Workspace.** Several windows; tabs (⌘T/⌘W, ⌘1–9, next/previous, reopen closed tab, last 20 kept); tab menu (Duplicate, Move to new window, Close others); split panes; tear-off; lazy restore; command-line folder; Back/Forward/Up restoring search, scroll and selection; mouse back/forward buttons; recent folders | one window, Back only | `dock/workspace.rs` S/M, `history.rs` C (already in core), `ids.rs` C | Each native window tab shows one core tab; a native tab group is one core window; a two-pane split is `HSplitView`; tear-off and merging come free from window tabbing; ⌘[ ⌘] ⌘↑ |
| 5 | **Path bar.** Breadcrumbs from an anchor (Home, volume, root), collapsing into "…"; a chevron menu of subfolders; ⌘-click opens a new tab; crumbs accept drops; path editor (⌘L) with completion, `~`, Tab, matching favorites/recent | a plain `TextField` | `breadcrumbs.rs` M, `path_editor.rs` S, `directory_path_info.rs` C+E (`child_directories` does I/O) | Toolbar breadcrumb control with menus and `.dropDestination`; a "Go to folder" field (⌘L and Finder's ⌘⇧G) with a completion popover fed by a core `ChildDirs` effect |
| 6 | **Search.** Chips (pills) in the input; query syntax (`ext:`, `kind:`, `size:`, `modified:`, `opened:`, `in:`, regex, glob, OR, `-word`); "+ Filter" menu; match case; This folder / Favorites; subfolder depth 2–7; match all/any; result count, Stop; a Folder column; highlighted matches; undo steps after a 700 ms pause; history entries | none | `search/*` C (already in core), `search/filter.rs` M (COLLATER, VIDEO_EXTS, open_history), `search_state.rs` C, `widgets/chip_input.rs` S | `.searchable(text:tokens:)` for chips, `searchScopes` for This folder / Favorites, toolbar filter `Menu`, `AttributedString` highlights from match ranges in the view model, ⌘F |
| 7 | **Saved searches.** Save (⌘S), update, revert, Save as new; editor (name, query, scope folders, options); run in this tab or a new one; duplicate, delete, reorder; all undoable | none | `search/saved.rs` C, `saved_searches.rs` S | A sheet for the editor; a sidebar section with `.onMove` and context menus |
| 8 | **Sidebar.** Favorites, Saved searches, Recent (5), Locations, Trash, Drives (kind icon, capacity bar, eject); sections collapse and reorder; rows accept drops; ⌘-click opens a new tab; folder context actions | none | `side_panel.rs` S, `locations.rs` M, `drives_ui.rs` S | `NavigationSplitView` sidebar `List` with `Section(isExpanded:)` (macOS 14), `.onMove`, `.dropDestination`, `ProgressView` capacity bars, eject button |
| 9 | **File operations.** Copy/move with APFS clone; conflict dialog (Replace / Skip / Keep both); Move to / Copy to submenu and Choose folder; rename (F2) with name checks; batch rename (find & replace, `{name}{n}{ext}` pattern, preview); New folder / New text document followed by rename; Duplicate (⌘D); trash with optional confirm; delete permanently; progress with cancel; Activity menu (last 30); undo/redo for file ops, search and saved searches | none | `fileops/*` E + `dirfleet_core::fileops` C, `fileops/naming.rs` C, `undo.rs` C, `file_ops.rs` S/M (dialogs, transfer logic) | A sheet per `Modal`; rename inline in the table row (with the sheet as fallback for batch rename); toolbar progress plus an Activity popover; Edit ▸ Undo/Redo titles taken from the core's undo labels |
| 10 | **Trash view.** List `~/.Trash` (needs Full Disk Access); purge; Empty via Finder. Restore is off on macOS, as in egui. | none | `fileops/trash_bin.rs` E | A sidebar row and a Trash tab with an "Empty…" toolbar button |
| 11 | **Clipboard & cut.** Copy/Cut/Paste working with Finder through NSPasteboard; Copy as path (⌘⇧C); cut items dimmed until the clipboard changes; status line | none | `clipboard*` E (`clipboard/macos.rs` already uses NSPasteboard) | Edit menu commands go through core events; the Rust handler writes the pasteboard; the view model marks cut rows |
| 12 | **Drag & drop.** Inside the app (⌘ copies, ⇧ moves, same drive moves, otherwise copies; optional Move-or-copy confirm; a badge); drop onto `.app` opens with it; drag out to other apps; drop in from other apps with an overlay | none | `file_ops.rs` M (target rules), `native_drag.rs` S, `macos_tools.rs:90` E | `.draggable` with file URLs (drag out comes free), `.dropDestination(for: URL.self)` on rows, crumbs and sidebar; the core decides move or copy from the modifiers the shell reports |
| 13 | **Open & friends.** Open (adds to open history); Show Package Contents; Open with… (app picker); Show in Finder; Open in terminal; Get Info | double-click into folders only | `macos_tools.rs` E, `open_history.rs` E, `entry_actions.rs` S | All handled in Rust except the Open-with picker, which is a shell-lane `PickApp` effect (`NSOpenPanel`) |
| 14 | **Drives.** Polled every 2 s; kinds; capacity; eject (asks first while operations are running); connect/disconnect toasts; status bar shows free space; tabs on a removed drive move home | none | `drives/*` E (the `Drive` type is C) | Sidebar section, toasts, `confirmationDialog` before eject |
| 15 | **Inspector.** Preview card; info (type, size with a Calculate button for folders, dates, times opened); Open / Copy path; several items: counts, total size, by kind, buttons; plugin sections | none | `inspector.rs` S, folder size `inspector.rs:266` (`thread::spawn`, to become an effect) | `.inspector` (macOS 14) with `Form`/`LabeledContent` |
| 16 | **Previews & thumbnails.** Image, GIF and video formats; hover preview after 250 ms; animated video frames; disk cache; FFmpeg found or downloaded (checksum checked); Select thumbnail dialog (timeline, More frames, Use / Reset); reset when the file changes; reads don't touch access time | none | `assets/*` E (`kinds.rs`, `failures.rs` C), `assets.rs` M (egui textures), `thumbnail_timeline.rs` C, `thumbnail_choice.rs` E, `thumbnail_picker.rs` S | The Rust media service writes cache files or hands over RGBA buffers (Parked lane); Swift builds `CGImage`s. Grid tiles and the inspector show them. The thumbnail picker is a sheet with a `LazyVGrid` timeline. File icons come from `NSWorkspace` in Swift (drawing only). |
| 17 | **Plugins (Lua).** 5 bundled plus user plugins with hot reload; manifest activation rules; actions in context menus, palette and shortcuts; permissions with approval; forms (13 field types, `visible_if`/`enabled_if`, remember, validation); result tables with a row menu and Run/Stop/background; bar segments; inspector sections; jobs (at most 2, progress, cancel); Settings ▸ Plugins page | none | `plugins/{manifest,permissions}` C, `api,runtime,jobs,sandbox,usage*,discovery` E, `host.rs` M (state machine + 3 egui functions), `panel.rs`, `ui.rs` S. Form and table types come from `egui_mev_data` (depends on egui) and are deserialize-only. | Form rendering driven by data (`Form`, `TextField`, `Toggle`, `Slider`, `Picker`, path fields with `NSOpenPanel`); results as `Table` in a window/sheet; a bar as a bottom accessory view; jobs in the Activity popover |
| 18 | **Context menus.** Item, Trash row, empty area, sidebar/crumb/drive rows, tab, list header, saved search, search pill (full list in the inventory) | none | `entry_actions.rs`, `menus.rs` S (the items are logic) | `.contextMenu` built from a core `MenuVm` so both apps show the same items |
| 19 | **Commands & keys.** Command palette (⌘⇧P/⌘K: commands, drives, plugin actions, recent folders, saved searches); 30+ keymap commands; Keyboard shortcuts overlay | ⌘[ only | `keymap.rs` M, `command_palette.rs` S, `commands.rs` C-ish | The menu bar (`CommandMenu`/`CommandGroup`) built from the core's `CommandVm`; a palette in a floating panel; a shortcuts list in Settings. Keys follow Mac conventions: hidden files on ⌘⇧. (⌘H hides the app), Back ⌘[, Forward ⌘], Up ⌘↑, Delete ⌘⌫, Quick Look Space. |
| 20 | **Settings.** General (terminal, Full Disk Access, about), Appearance (theme, sidebar items, drive capacity/notifications), Files & folders (defaults, confirmations, open history), Previews (icon size, animation, reset thumbnails), Keyboard, Plugins | none | `settings.rs` S (edits mutate the app directly) | A `Settings` scene with a `TabView` of `Form`s; every edit is a core event |
| 21 | **Toasts** (Info, Warn, Error, Ok), sent from any thread | error text only | `PENDING_TOASTS` global → `Model.toasts` | A small overlay stack; errors also as a banner |
| 22 | **Full Disk Access banner.** Ask → Waiting (checks every 1 s) → Restart | none | `disk_access.rs` S, `macos_tools.rs:178-203` E | A banner view; checking driven by a core `Time` tick |
| 23 | **Persistence.** Settings, windows/tabs, recent, favorites, saved searches, per-folder view settings, columns, sled (listing cache, open history, thumbnails), FFmpeg, plugin storage; import from earlier builds | none | `app.ron` + egui memory (`legacy_import.rs`), sled modules E | A core-owned `PersistedState` behind a `Storage` effect (Rust). The Mac app uses its own data folder (bundle id `…dirfleet.native`). |

Not applicable to the Mac app (kept in egui while egui exists):
- Windows-only: Explorer context menu, custom title bar, Recycle Bin restore, `$`-hidden files.
- Linux `/media` anchors.
- The narrow-window overlay sidebar and inspector: native split views collapse on their own.
- The egui Quick Look overlay: replaced by the system panel.
- The profiler.

egui behaviour that should **not** be copied (found in the inventory):
- Shortcut text hard-coded as "Ctrl" on macOS.
- The confirm text says "Ctrl+Z brings it back" for trash, but undoing a trash fails on macOS (`fileops/worker.rs:701`).
- ⌘H is bound to hidden files.
- Palette "Open …" entries that never show (`build_for_path`).

Fix these in the core's labels and keymap. The fixes then reach egui too.

