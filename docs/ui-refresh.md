# DirFleet UI refresh

The interface uses shared dark and light surfaces, compact Lucide controls, blue selection, grouped locations, a unified toolbar, and card-based settings. System appearance is the default. The existing egui 0.34 stack, docked tabs, split panes, file operations, search pipeline, and thumbnail cache remain in use.

## Controls

- Command/Ctrl+L: edit the current path. The dropdown lists the folders under what is typed: Up/Down choose, Tab completes and lists the next level, Enter or a click opens, and Command/Ctrl-click opens in a new tab. A leading `~` means Home. The path opens selected, so typing a bare name replaces it and searches favorite and recent folders. Pressing outside the field or its dropdown closes the editor.
- Command/Ctrl+F: open or close the focused pane’s search.
- Command/Ctrl+,: settings; the existing Command/Ctrl+P shortcut also works.
- Alt/Option+Command/Ctrl+S: toggle the sidebar.
- Alt/Option+Command/Ctrl+I: toggle the inspector.
- Command/Ctrl+R: command palette; arrow keys select and Enter runs a command.
- F5: force refresh.

The toolbar contains navigation, collapsible breadcrumbs, search, list/grid, sorting, More, and panel controls. More contains hidden-file visibility, terminal, refresh, settings, and commands. Advanced search options live in a scrolling popover.

## Panels and persistence

The sidebar defaults to 200 points (160–280). The inspector defaults closed and to 280 points (240–360). Visibility, widths, and appearance use backward-compatible settings defaults. Below 1000 points the inspector collapses; below 800 the sidebar also collapses and view/sort move into More. Automatic collapse does not overwrite visibility preferences. Compact panel controls open overlays dismissible by Escape or clicking outside.

The inspector follows the focused pane’s file paths. Selection is reconciled by path after filtering, sorting, navigation, and watcher changes. It shows previews, file/folder metadata, and Open/Copy Path actions. Folder sizes are not calculated recursively; absent dates are shown as unavailable. Preview requests share AssetManager’s loading/failure states and concurrency limits, and the inspector owns the selected animation request while visible.

## Validation

Freshly rendered screenshots were visually reviewed before saving approved `.png` baselines. Existing `.new.png` and `.diff.png` review artifacts were not adopted as baselines. Full-window snapshots use bundled fonts and cover both themes at 1100×720 and 640×420, list/grid, split panes, long names, search options, settings categories, compact overlays, theme changes, and inspector selection states. Synthetic preview snapshots cover ready/loading/failure without requiring a decoder.

The regular test suite includes keyboard selection and search, command-palette keyboard choice, path-editor focus, overlay dismissal, selection reconciliation, legacy settings migration, settings round trips, large-directory virtualization, and preview scheduling/budgets. The real-media tests additionally cover video cache reuse, geometry, and HDR fallback.

Run `cargo fmt --check`, `cargo check --offline`, and `cargo test --offline -- --test-threads=1`. Snapshot tests require a graphics adapter. Run the three decoder checks with `cargo test --offline real_ -- --ignored --test-threads=1`. The separate external-video benchmark requires its source environment variable and remains opt-in. Clippy was run and compared with the pre-refresh workspace; existing lint failures remain outside this refresh.
