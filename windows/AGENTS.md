# Native Windows shell (`dirfleet_windows`)

WinUI 3 from Rust through Microsoft's `windows-reactor`, on the shared core
(`crates/dirfleet_core`) and its Rust effect handlers (`crates/dirfleet_effects`).
The core is linked as a normal dependency: events and the view model are typed
Rust values, with no FFI and no serialization. The shell draws, collects input
and answers the six effects that need a window (`Render`, `OpenWindow`,
`PickFolder`, `PickApp`, `QuickLook`, `QuitApp`).

The plan and its milestones: `docs/windows-native.md`.

## Commands

```
just windows-check          # fmt + clippy -D warnings
just windows-test
just windows-run --folder C:\Users
just windows-perf           # the core's answers in folders of 10,000 and 100,000 entries
just windows-package        # the MSI, into target/windows-package/installer (needs WiX 5)
```

The crate is a workspace member but not a default one, so `just validate`
leaves it out. `DIRFLEET_TRACE=1` writes timing notes to stderr.
`--state-file <file>` keeps test runs away from the real state.

The first build downloads the Windows App SDK runtime (`windows-reactor-setup`,
needs network, `curl.exe` and `tar.exe`) and stages its DLLs beside the exe.

## Modules

| File | Role |
|---|---|
| `main.rs` | Entry: `App::run_application::<DirFleetApp>` |
| `launch.rs` | Command line; where the state file lives |
| `core_link.rs` | `EffectRouter` + `Handlers`; inbox of shell effects; stale flag |
| `ui_thread.rs` | Message-only window: wake-ups from handler threads, and deferred UI-thread work |
| `app.rs` | `Application`: one native window per core window; shell effects (pickers, quit) |
| `store.rs` | Splits the view model into `Rc` parts that keep identity while equal |
| `window.rs` | `WindowRoot`: title bar (with the tabs of a window's only pane), More menu, panes laid out from the core's `FlatLayout`, splitters |
| `theme.rs` | The surfaces, edges and gaps that set the window's areas apart; the switch without the system's words |
| `hwnd.rs` | Native window subclass: activation (`WindowFocused`), the active window for dialogs |
| `commands.rs` | `CommandId` → core event, or a command the window carries out itself |
| `shortcuts.rs` | Key chords parsed from the core's shortcut labels |
| `pane.rs` | `Pane`: the strip of its tabs (when the window has several panes), header, empty states, the plugins' bar, status line |
| `pathbar.rs` | Crumbs, folding, the anchor's and each crumb's folder lists |
| `icons.rs` | Shell icons by kind of file, as PNG the UI shares |
| `files/view.rs` | `FileView`: list or icon grid over `ItemsRepeater::virtual_source`; viewport reports, scroll-to, keys |
| `files/layout.rs` | Pure geometry: lines, entries in sight, cursor moves |
| `files/columns.rs` | The list's columns and header: sort, resize grips, optional columns |
| `files/row.rs`, `files/tiles.rs` | A list row; a line of grid tiles. Components of their own |
| `files/press.rs` | What a press on an entry means; telling it from a press on empty space |
| `files/rename.rs` | `RenameBox`: the text box that stands in for a name |
| `sidebar.rs` | Places, drives, sections that fold |
| `dialogs.rs` | Which dialog a window shows, and each one: confirmations, conflict, eject, drop, batch rename, Go to Folder |
| `sheets.rs` | The palette, the shortcuts list and Settings, shown as dialogs |
| `notices.rs` | Operations in progress and toasts |
| `menus.rs`, `popup.rs` | Context menus as data, and their Win32 popup |
| `search.rs` | The search box, its filters and its options menu |
| `inspector.rs` | What is known about the selection |
| `preview.rs` | Quick Look: a window for images and text |
| `picker.rs` | Choosing a video's thumbnail from its frames |
| `plugins.rs` | Everything plugins show: bar, forms, results, jobs, inspector sections, their part of Settings |
| `perf.rs` | A measurement of the core's answers in large folders (`just windows-perf`) |
| `packaging/` | The MSI: `package.ps1` builds, stages, validates and starts it; `dirfleet.wxs` is its WiX source |
| `trace.rs` | Timing notes |
| `tools/start.ps1`, `tools/drive.ps1`, `tools/capture.ps1` | Start a build of its own, drive it and capture it, for manual checks |

## Rules

- **Threads.** Reactor messages can only be sent from the UI thread. Handler
  threads call `ui_thread::wake()`; never touch reactor state from them.
- **The app needs a window to live.** `DirFleetApp::create` waits for the
  core's first `OpenWindow` before returning, or the application would end.
- **Identity is the diff.** Components compare inputs with `PartialEq`, and
  `Rc<T: Eq>` compares pointers first. `Store::update` keeps the old `Rc` for
  every part that did not change; do not rebuild parts needlessly.
- **Rows by position.** The list is as long as the folder (`total_rows`), keyed
  by row position. The core sends only rows near the viewport; a position it
  has not sent draws an empty row of the same height.
- **Keep the `VirtualSource`.** A new source rebuilds every realized row, so
  `FileList` builds it in `create`/`input_changed`, never in `view`.
- **Callbacks for window observers are made once** (`WindowRoot::on_size`).
- **Unsafe** only per module, with `#![expect(unsafe_code, reason = …)]`.
- **Win32 stays on `windows` 0.59**, like the rest of the workspace; reactor
  brings its own windows-rs revision and their types do not mix.

- **Anything with its own message loop starts from `ui_thread::defer`**: file
  dialogs, popup menus, drags. Inside a reactor callback it would stall reactor.
- **An attachment's root keeps its element type.** Reactor panics (and the
  process ends) when a flyout's or tooltip's root changes from one control to
  another between renders; wrap what varies in a `Border`.
- **A new element refuses the focus until it is laid out.** `FileView` asks
  again every 40 ms until `request_focus_result` says yes.
- **The `TabView` holds titles only.** Content moved between its items goes
  blank (closing the shown tab left an empty view), so the strip has no
  content and the folder is drawn once, below it. With one pane the strip is
  in the title bar; `Pane` then draws none.
- **A switch says nothing beside itself.** `theme::switch` hides the control's
  own "On"/"Off", which come in the system's language; give it an
  `automation_name`, since its label is a separate text.
- **Text-editing chords belong to whatever has the keyboard.** The window runs
  the other shortcuts in its preview handler; `Chord::edits_text` ones (Ctrl+C,
  Del, Space…) are run by `FileView` only while it has the focus.

## Checking by hand

`tools/drive.ps1` presses buttons and menu items through UI Automation and
posts key messages to the app's window; `tools/capture.ps1` saves a picture of
it. Neither moves the pointer, so clicks on rows, drags and hover are not
covered by them and need a person.

Drive only the instance `tools/start.ps1` started, never one a person may be
using: a posted key lands in whatever that window has focused. `start.ps1`
runs a build from `target\drive\build` (a running `target\debug\DirFleet.exe`
is locked, and is somebody's), gives it a state file of its own and remembers
its process id; `drive.ps1` touches no other process. Point it at a scratch
folder. Keys can only be posted while the window has had the keyboard; do not
bring it to the front to get it.

## The reactor fork

The shell needs a few members upstream `windows-reactor` (rev `e977232`) lacks.
They live on branch `dirfleet` of <https://github.com/Leinnan/windows-rs>, and
`Cargo.toml` pins that branch's rev:

| Added | Used for |
|---|---|
| `ElementRef<ScrollViewer>::observe_view` → `ScrollPosition` | the rows in sight (`Event::ViewportChanged`) |
| `ElementRef<ScrollViewer>::request_scroll_to` | `scroll_to` (Back and Forward return to where you were) |
| `ElementRef<TextBox>::request_select` | rename with the stem preselected |
| `PointerEventInfo::{is_x_button1_pressed, is_x_button2_pressed, wheel_delta}`, `Border::on_pointer_wheel_changed` | mouse Back/Forward, Ctrl+wheel |

The scroll and selection members are hand-written beside reactor's other
element requests (`reference.rs`, `native/winui/adapter.rs`); the rest goes
through its generator (`cargo run -p tool-reactor` after editing
`crates/tools/reactor/src/{schema.toml,bindings.txt}`).

To work on the fork, clone it beside this repo and point the build at the clone
with a git-ignored `.cargo/config.toml`; once the change is pushed, bump the rev
in `Cargo.toml` and delete the file:

```toml
[patch."https://github.com/Leinnan/windows-rs"]
windows-reactor = { path = "E:/PROJECTS/windows-rs/crates/libs/reactor" }
```

Why the scroll members are required: without scroll events the rows in sight
can only be guessed from the rows the list builds, and after a jump the list
also rebuilds leftover rows near the old position. Measured on 100,000 rows,
the guess spanned rows 0..50,011 and the core sent every row (440 ms per view
model, against 0.6 ms with exact reports).

## Still missing in `windows-reactor`

- Menus attach to buttons only; there is no menu at the pointer.
- Only `Border` takes pointer, key and drop events.
- A scroll asked for before the list is laid out at its new length is cut
  short, and a change of extent alone raises no event; `FileList` asks again
  every 30 ms until the position holds.
