use super::*;
use super::{
    dock::{MyTabs, Selected, TabData},
    ui::Appearance,
};
use egui_kittest::{Harness, kittest::Queryable as _};

fn fixture(appearance: Appearance, grid: bool, inspector: bool) -> App {
    let root = Path::new("/virtual/Projects/DirFleet");
    let mut tab = TabData::from_path(root);
    tab.list = [
        "Documents",
        "Design notes.txt",
        "A very long file name that should truncate gracefully inside a narrow split pane.md",
        "Preview unavailable.bin",
    ]
    .iter()
    .map(|name| DirEntry::test_new(&format!("{}/{name}", root.display())))
    .collect();
    tab.list[0].meta.entry_type = crate::data::files::EntryType::Directory;
    tab.visible_entries = (0..tab.list.len()).collect();
    tab.loading = false;
    tab.current_path = CurrentPath::One(root.to_path_buf());
    tab.top_display_path.build(root, false);
    tab.display_type = if grid {
        DisplayType::Icons
    } else {
        DisplayType::List
    };
    let mut tabs = MyTabs::new(root);
    *tabs.get_current_tab().expect("tab") = tab;
    tabs.focused = true;
    let mut settings = ApplicationSettings::default();
    settings.appearance = appearance;
    settings.inspector_visible = inspector;
    App {
        tabs,
        settings,
        user_locations: Locations {
            locations: vec![
                Location::from_path(root, "DirFleet"),
                Location::from_path("/virtual/Documents", "Documents"),
                Location::from_path("/virtual/Downloads", "Downloads"),
            ],
        },
        ..Default::default()
    }
}
fn draw(root: &mut egui::Ui, app: &mut App) {
    let ctx = &root.ctx().clone();
    let initialized = ctx
        .data(|data| data.get_temp::<bool>(egui::Id::new("fixture_initialized")))
        .unwrap_or(false);
    if !initialized {
        let mut fonts = egui::FontDefinitions::default();
        ui::register_icons(&mut fonts);
        ctx.set_fonts(fonts);
        ui::configure(ctx);
        if let Some(tab) = app.tabs.get_current_tab() {
            ctx.data_set_path(
                &tab.current_path,
                Selected {
                    selected_fields: vec![1],
                    just_changed: false,
                },
            );
            ctx.data_set_path(
                &tab.current_path,
                DirectoryViewSettings {
                    display_type: tab.display_type,
                    ..Default::default()
                },
            );
        }
        ctx.data_mut(|data| data.insert_temp(egui::Id::new("fixture_initialized"), true));
        return;
    }
    app.assets.begin_frame();
    app.assets.poll_results(ctx);
    app.render_browser(root);
    if app.display_modal == Some(ModalWindow::Settings) {
        app.settings.display(ctx);
    }
    app.assets.wait_for_idle(ctx);
    app.assets.end_frame();
}

#[test]
fn full_window_appearances_and_compact_layouts() {
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut snapshots = egui_kittest::SnapshotResults::new();
    for (name, appearance, size, grid) in [
        (
            "dirfleet_full_dark",
            Appearance::Dark,
            egui::vec2(1100.0, 720.0),
            false,
        ),
        (
            "dirfleet_full_light",
            Appearance::Light,
            egui::vec2(1100.0, 720.0),
            false,
        ),
        (
            "dirfleet_grid_dark",
            Appearance::Dark,
            egui::vec2(1100.0, 720.0),
            true,
        ),
        (
            "dirfleet_grid_light",
            Appearance::Light,
            egui::vec2(1100.0, 720.0),
            true,
        ),
        (
            "dirfleet_compact_dark",
            Appearance::Dark,
            egui::vec2(640.0, 420.0),
            false,
        ),
        (
            "dirfleet_compact_light",
            Appearance::Light,
            egui::vec2(640.0, 420.0),
            false,
        ),
    ] {
        let mut harness = Harness::builder()
            .with_size(size)
            .build_ui_state(draw, fixture(appearance, grid, true));
        harness.run_steps(12);
        snapshots.add(harness.try_snapshot(name));
    }
    snapshots.unwrap();
}
#[test]
fn search_settings_and_overlay_snapshots() {
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut snapshots = egui_kittest::SnapshotResults::new();
    let mut app = fixture(Appearance::Dark, false, false);
    app.tabs.get_current_tab().expect("tab").search = Some(Search {
        value: "notes".into(),
        depth: 3,
        case_sensitive: true,
        ..Default::default()
    });
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw, app);
    harness.run_steps(12);
    harness.get_by_label("Options •").click();
    harness.run_steps(4);
    snapshots.add(harness.try_snapshot("dirfleet_search_options"));
    let mut app = fixture(Appearance::Light, false, false);
    app.display_modal = Some(ModalWindow::Settings);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw, app);
    harness.run_steps(12);
    snapshots.add(harness.try_snapshot("dirfleet_settings_light"));
    let mut app = fixture(Appearance::Dark, false, false);
    app.inspector_overlay = true;
    let mut harness = Harness::builder()
        .with_size(egui::vec2(640.0, 420.0))
        .build_ui_state(draw, app);
    harness.run_steps(12);
    snapshots.add(harness.try_snapshot("dirfleet_inspector_overlay"));
    snapshots.unwrap();
}
#[test]
fn selection_follows_paths_across_sort_filter_and_deletion() {
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let ctx = egui::Context::default();
    let mut app = fixture(Appearance::Dark, false, true);
    let tab = app.tabs.get_current_tab().expect("tab");
    let path = tab.current_path.clone();
    let id = tab.id;
    ctx.data_set_path(
        &path,
        Selected {
            selected_fields: vec![1],
            just_changed: false,
        },
    );
    // Draw once to record the canonical file selection.
    let _ = ctx.run(Default::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| app.tabs.ui(ui, &mut app.assets));
    });
    let selected_path = app.inspector_view(&ctx).expect("view").paths[0].clone();
    app.tabs
        .get_tab_by_id(id)
        .expect("tab")
        .visible_entries
        .reverse();
    app.tabs.reconcile_selection(&ctx);
    assert_eq!(
        app.inspector_view(&ctx).expect("view").paths,
        vec![selected_path]
    );
    app.tabs.get_tab_by_id(id).expect("tab").visible_entries = vec![0, 3];
    app.tabs.reconcile_selection(&ctx);
    assert!(app.inspector_view(&ctx).expect("view").paths.is_empty());
}
#[test]
fn auto_collapse_keeps_preferences_and_toggle_opens_overlay() {
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let ctx = egui::Context::default();
    let mut app = fixture(Appearance::Dark, false, true);
    let _ = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(640.0, 420.0),
            )),
            ..Default::default()
        },
        |ctx| {
            app.handle_action(ctx, ActionToPerform::ToggleSidebar);
            app.handle_action(ctx, ActionToPerform::ToggleInspector);
        },
    );
    assert!(app.settings.sidebar_visible && app.settings.inspector_visible);
    assert!(app.sidebar_overlay && app.inspector_overlay);
}
