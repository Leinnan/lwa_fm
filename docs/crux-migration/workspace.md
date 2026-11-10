# Workspace design (T3)

T3 moves windows, tabs, panes and navigation into the core for both apps.
It is also where the egui app first takes its *state* from the core. Up to
T2 it has only shared *code* with it: the listing model, the reader, the
sort, the hidden-file rule and the columns. This page settles both before
any code is written.

## Where things stand

| | egui app | Mac app |
|---|---|---|
| Windows | `Workspace` (`src/app/dock/workspace.rs`): `windows[0]` is the main window, each with an `egui_dock::DockState<TabId>` | Core windows (`dirfleet_core::app`), one SwiftUI window each |
| Tabs | `TabStore` of `TabData` (`src/app/dock.rs:236`): listing, order, selection, search, history, view, load state and drawing state together | Core tabs: path, back stack, listing, order, view, selection |
| Panes | egui_dock splits with any nesting, drag to split, tear-off | none |
| Saved | `app.ron`: each window's `DockState<PathBuf>` and position | `state.json`: windows, each with tab folders and the visible tab |

About 550 places in the egui crate read `TabData` directly, so moving it
in one step would be the riskiest change of the migration.

## The core's workspace

```text
Workspace
  windows: Vec<Window>            in opening order
  tabs: BTreeMap<TabId, Tab>      every tab, wherever it is
  closed: Vec<ClosedTab>          the last 20 closed tabs, for Reopen
Window
  id: WindowId
  layout: Node                    egui_dock's tree, as plain data
  focused: TabId                  the tab keyboard commands go to
  group: Option<u32>              the Mac's native tab group, for restore
Node
  Pane { tabs: Vec<TabId>, active: usize }
  Split { vertical: bool, permille: u16, first: Box<Node>, second: Box<Node> }
Tab                               as now, plus history (back and forward)
                                  entries that keep the folder's search,
                                  scroll row and selection, as egui's do
```

New events. Every event names its window or tab.
- **Tabs:** `NewTab { window, path }`, `CloseTab { tab }`, `ActivateTab { tab }`, `DuplicateTab { tab }`, `CloseOtherTabs { tab }`, `ReopenClosedTab { window }`, `CycleTab { window, step }`, `GoToTab { window, index }` (the last index means the last tab).
- **Panes:** `SplitPane { tab, direction }`, `ResizeSplit { window, path, permille }` and `MoveTab { tab, to }`, where `to` is a pane and index in some window, or a new window. Dragging a tab out of a window is a `MoveTab` to a new window.
- **Navigation:** `Forward { tab }`, `Up { tab }` and `Restore { tab, entry }`. `Back` exists already.
- **Windows:** `CloseWindow` applies egui's main-window rule. Closing the main window hands its tabs to the next window. This is the only egui-only rule, and the Mac app never triggers it.

Saving extends `PersistedWindow` with the layout tree and `group`, using
serde defaults, so `state.json` files written today still load.

## How each app shows it

**egui:** the core's layout tree *is* the dock layout.
- egui keeps an `egui_dock::DockState<TabId>` only as a drawing mirror, rebuilt from the core whenever the layout changes.
- egui_dock's own changes (a drag, a split, a close button) are read back from it after the frame and sent as `MoveTab`, `SplitPane` or `CloseTab`.

The core then owns the layout without reimplementing egui_dock.

**Mac:** each core *window* is one NSWindow, as now.
- AppKit's native tabs group those windows. ⌘T asks the core for a new window and adds it to the current window's tab group (`NSWindow.addTabbedWindow`).
- Tear-off, merging and moving between groups come free from AppKit.
- The shell reports the group with a `WindowGrouped` event, so the core can save it and restore the grouping.
- A split shows the window's first pane and second pane side by side (`HSplitView`). The Mac app never builds deeper trees. egui can, but the two apps never share saved state.
- The tab commands (next, previous, go to tab N, reopen closed tab) map to native window-tab commands and to the core's reopen stack.

This is the mapping the plan named ("a native tab group is one core
window"), corrected: a core window is one Mac *tab*. The group is
recorded, but it's AppKit's to manage.

## Moving egui over, in steps

Each step lands with the egui tests at the baseline and the snapshots
unchanged, as every track has so far.

1. **Run the core inside egui.** The egui `App` creates an `EffectRouter` (as `examples/core_spike.rs` does) and keeps the core's view model for the frame. Nothing reads it yet.
2. **Tab identity and layout.** egui's `Workspace` hands windows, layout and tab ids to the core. `TabData` keeps everything else, keyed by the core's `TabId`.
3. **Navigation.** `path`, history and Back/Forward/Up move to the core tab. `TabData` reads the core's current folder.
4. **Listing.** `listing`, `order` and `selection` move to the core tab, and egui's list and grid read the core's rows. egui reads the row window from the view model, as the Mac app does, so its virtual scrolling reports a viewport.
5. **Drawing state stays in egui:** scroll offsets, hover, text layout caches, textures and the dock mirror.

Search state and its walk move in T8, and file-operation state in T5. Until
then they stay in `TabData` beside the core tab.

## What could go wrong

- **egui_dock's tree differs from the core's.** egui_dock nests splits per surface, and windows are surfaces. Each egui window maps to one core window, with the main surface's tree as its layout.
- **Frame timing in egui.** Effects resolve on handler threads, so egui must redraw when the core renders: the spike's `Repaint` pattern. Tests drive kittest frames; settle frames may need adjusting, as the memory notes on driving the app already warn.
- **Performance.** egui currently reads `Arc<Listing>` and `order` directly. Step 4 replaces that with the view model's row window, which the spike gate already covers (p95 well under 2 ms).
