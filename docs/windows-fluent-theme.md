# Egui 0.36 and Windows Fluent

DirFleet pins egui, eframe, egui_extras, egui-winit and egui_kittest to 0.36.2, with egui_dock 0.21.1, egui_taffy 0.14.0 and WGPU 30. Rust 1.95 or newer is required. Cargo.lock is now included for reproducible application builds. Renderer, persistence, image loaders and existing platform features are retained. Present mode moved to WGPU's SurfaceConfig; the existing skip/recreate policy is unchanged.

The notification fork and pinned profiler UI are local dependencies; licenses and revisions are recorded in ../vendor/PROVENANCE.md. The profiler core and HTTP server remain at their original pinned revision. Both default and profiling dependency graphs contain one egui version.

Windows installs both Fluent light and dark styles automatically. Source attribution is in ../src/app/theme/PROVENANCE.md. Other platforms keep their existing style and font setup. Appearance serializes as the existing System, Light or Dark value. System theme changes apply live, and persisted overrides remain authoritative. Accent shades are read once from the Windows registry, with the source blue palette as fallback; accent changes require restart.

Fonts prefer Segoe UI Variable/Segoe UI, Segoe UI Semibold and Cascadia Mono/Consolas from WINDIR/Fonts. Missing fonts fall back to egui fonts. Lucide remains its own family. The type scale is 12/14/20 with 13 monospace; standard controls are 32px, navigation rows 36px, control radii 4px, and cards/dialogs 8px. Toolbar reservation follows the current button metrics. Panels, cards, selection, navigation and dock tabs use the same tokens.

Native chrome state is skipped by serde. DWM rounded corners, caption fill/text and dark-mode attributes follow the resolved theme. Missing handles are retried; unsupported attributes retain the OS fallback. Client rendering remains opaque.

## Verification

The migration was compiled and reviewed before activating Fluent. Windows legacy baselines are in ../tests/snapshots/windows/dirfleet_*.png and dock_view_*.png; Fluent baselines use fluent_*.png in that directory. Existing non-Windows baselines remain separate. Tests disable system fonts, inject a fixed accent, omit host drives and use deterministic document icons. Platform path formatting still follows the host. Snapshot review identified small text/panel layout changes in egui 0.36 and host-specific path/icon differences; Windows baselines record those separately from Fluent.

Coverage includes full windows, grids, compact layouts, settings, appearance switching, split panes and rename dialogs. Behavioral tests exercise selection, keyboard navigation, search and text caches across theme changes. Unit tests cover accent parsing/default shades, font fallbacks, installed Windows font rendering, palette/style agreement, live system input, persisted appearance overrides, legacy settings and unavailable native handles. Existing thumbnail/media tests run in both feature configurations. CI installs stable Rust and validates profiling in addition to the default configuration.

Windows default and profiling suites each passed 112 tests, with eight existing integration/fixture tests ignored. Formatting, strict Clippy for both feature configurations, and compilation checks passed. The child-process cancellation test must run outside the restricted Windows sandbox to terminate its own descendants.

## Remaining interactive checks

An interactive Windows pass is still needed for actual OS light/dark notifications, persisted override behavior after restarting the native window, visual Segoe rendering on different DPI settings, caption updates and DWM behavior on older Windows builds. Simulated system-theme input, appearance serialization and unavailable-handle fallback are covered automatically. Linux compilation and non-Windows snapshot refresh were deferred at the user's request.
