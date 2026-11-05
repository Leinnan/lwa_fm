use super::*;
use super::{
    dock::{TabData, Workspace},
    ui::Appearance,
};
use crate::data::files::DirEntry;
use egui_kittest::{Harness, kittest::Queryable as _};

fn fixture(appearance: Appearance, grid: bool, inspector: bool) -> App {
    let root = Path::new("/virtual/Projects/DirFleet");
    let mut tab = TabData::from_path(root);
    let mut entries: Vec<DirEntry> = [
        "Documents",
        "Design notes.txt",
        "A very long file name that should truncate gracefully inside a narrow split pane.md",
        "Preview unavailable.bin",
    ]
    .iter()
    .map(|name| DirEntry::test_new(&format!("{}/{name}", root.display())))
    .collect();
    entries[0].meta.entry_type = crate::data::files::EntryType::Directory;
    tab.set_entries(&entries);
    tab.load = crate::app::dock::LoadState::Idle;
    tab.current_path = CurrentPath::One(root.to_path_buf());
    tab.top_display_path.build(root);
    tab.display_type = if grid {
        DisplayType::Icons
    } else {
        DisplayType::List
    };
    let mut tabs = Workspace::from_tabs(vec![tab]);
    tabs.focused = true;
    let mut settings = ApplicationSettings::default();
    // A program every Windows machine has, so the settings page reads the
    // same everywhere.
    settings.terminal_path = r"C:\Windows\System32\cmd.exe".into();
    settings.appearance = appearance;
    settings.inspector_visible = inspector;
    App {
        tabs,
        settings,
        drives: super::drives_ui::DrivesState::fixed(vec![
            crate::drives::test_drive(r"C:\", "Local Disk", crate::drives::DriveKind::Fixed),
            crate::drives::test_drive(r"E:\", "Backup", crate::drives::DriveKind::Removable),
        ]),
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
        .data(|data| data.get_temp::<bool>(egui::Id::unique("fixture_initialized")))
        .unwrap_or(false);
    if !initialized {
        ui::configure_test(
            ctx,
            ctx.data(|data| data.get_temp::<bool>(egui::Id::unique("fluent_fixture")))
                .unwrap_or(false),
        );
        if let Some(tab) = app.tabs.get_current_tab() {
            tab.selection.set_rows(
                ctx.data(|data| data.get_temp::<Vec<usize>>(egui::Id::unique("initial_selection")))
                    .unwrap_or_else(|| vec![1]),
            );
            ctx.data_set_path(
                &tab.current_path,
                DirectoryViewSettings {
                    display_type: tab.display_type,
                    ..Default::default()
                },
            );
        }
        ctx.data_mut(|data| data.insert_temp(egui::Id::unique("fixture_initialized"), true));
        return;
    }
    app.assets.begin_frame();
    app.assets.poll_results(ctx);
    app.render_browser(root);
    app.render_modal(ctx);
    app.assets.wait_for_idle(ctx);
    app.assets.end_frame();
}

fn draw_fluent(root: &mut egui::Ui, app: &mut App) {
    root.ctx()
        .data_mut(|data| data.insert_temp(egui::Id::unique("fluent_fixture"), true));
    draw(root, app);
}

#[cfg(windows)]
#[test]
fn fluent_windows_grids_compact_settings_and_appearance_switching() {
    let _guard = egui_mev_test::snapshot_lock();
    let mut snapshots = egui_kittest::SnapshotResults::new();
    for (suffix, appearance) in [("dark", Appearance::Dark), ("light", Appearance::Light)] {
        for (layout, size, grid) in [
            ("full", egui::vec2(1100.0, 720.0), false),
            ("grid", egui::vec2(1100.0, 720.0), true),
            ("compact", egui::vec2(640.0, 420.0), false),
        ] {
            let mut harness = Harness::builder()
                .with_options(egui_mev_test::snapshot_options())
                .with_size(size)
                .build_ui_state(draw_fluent, fixture(appearance, grid, true));
            harness.run_steps(12);
            snapshots.add(harness.try_snapshot(format!("fluent_{layout}_{suffix}")));
        }
        let mut app = fixture(appearance, false, false);
        app.display_modal = Some(ModalWindow::Settings);
        let mut harness = Harness::builder()
            .with_options(egui_mev_test::snapshot_options())
            .with_size(egui::vec2(1100.0, 720.0))
            .build_ui_state(draw_fluent, app);
        harness.run_steps(12);
        snapshots.add(harness.try_snapshot(format!("fluent_settings_{suffix}")));
        harness.get_by_label("Appearance").click();
        harness.run_steps(4);
        snapshots.add(harness.try_snapshot(format!("fluent_settings_appearance_{suffix}")));
    }
    let mut harness = Harness::builder()
        .with_options(egui_mev_test::snapshot_options())
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw_fluent, fixture(Appearance::Dark, false, true));
    harness.run_steps(12);
    harness.state_mut().settings.appearance = Appearance::Light;
    harness.run_steps(12);
    snapshots.add(harness.try_snapshot("fluent_appearance_changed_to_light"));
    harness.state_mut().settings.appearance = Appearance::Dark;
    harness.run_steps(12);
    snapshots.add(harness.try_snapshot("fluent_appearance_changed_to_dark"));
    snapshots.unwrap();
}

#[test]
fn fluent_keyboard_search_dialogs_and_split_panes_survive_theme_changes() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let mut harness = Harness::builder()
        .with_options(egui_mev_test::snapshot_options())
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw_fluent, fixture(Appearance::Dark, false, true));
    harness.run_steps(12);
    harness.hover_at(egui::pos2(380.0, 180.0));
    harness.run_steps(2);
    harness.key_press(egui::Key::ArrowDown);
    harness.run_steps(2);
    let ctx = harness.ctx.clone();
    let selected = harness
        .state_mut()
        .inspector_view(&ctx)
        .expect("selection")
        .paths;
    assert!(
        selected[0]
            .to_string_lossy()
            .contains("A very long file name")
    );
    for appearance in [Appearance::Light, Appearance::Dark] {
        harness.state_mut().settings.appearance = appearance;
        harness.run_steps(12);
        assert_eq!(
            harness
                .state_mut()
                .inspector_view(&ctx)
                .expect("selection")
                .paths,
            selected
        );
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
        harness.run_steps(2);
        assert!(
            harness
                .state_mut()
                .tabs
                .get_current_tab()
                .expect("tab")
                .search
                .is_some()
        );
        harness.key_press(egui::Key::Escape);
        harness.run_steps(2);
    }
    // Add a second dock pane and render with the changed text cache and theme.
    let mut second = TabData::from_path(Path::new("/virtual/Downloads"));
    second.current_path = CurrentPath::One("/virtual/Downloads".into());
    second.set_entries(&[DirEntry::test_new("/virtual/Downloads/Download.txt")]);
    second.load = crate::app::dock::LoadState::Idle;
    let nodes = harness.state_mut().tabs.split_right(second);
    harness
        .state_mut()
        .tabs
        .dock_mut()
        .set_focused_node_and_surface(egui_dock::NodePath::new(
            egui_dock::SurfaceIndex::main(),
            nodes[0],
        ));
    harness.run_steps(12);
    assert!(harness.state_mut().tabs.get_current_tab().is_some());
    let table_right = ctx.content_rect().right() - harness.state().settings.inspector_width;
    let headers: Vec<_> = harness
        .get_all_by_label("Size")
        .filter(|node| node.rect().top() < 150.0)
        .collect();
    assert_eq!(headers.len(), 2);
    assert!(
        headers
            .iter()
            .all(|node| node.rect().right() <= table_right)
    );
    #[cfg(windows)]
    harness.snapshot("fluent_split_panes_dark");
    harness
        .state_mut()
        .begin_rename(PathBuf::from("/virtual/Download.txt"));
    harness.run_steps(12);
    let _ = harness.get_by_label("New name");
    #[cfg(windows)]
    harness.snapshot("fluent_split_panes_rename");
    while COMMANDS_QUEUE.pop().is_some() {}
}

#[test]
fn full_window_appearances_and_compact_layouts() {
    let _guard = egui_mev_test::snapshot_lock();
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
            .with_options(egui_mev_test::snapshot_options())
            .with_size(size)
            .build_ui_state(draw, fixture(appearance, grid, true));
        harness.run_steps(12);
        snapshots.add(harness.try_snapshot(name));
    }
    snapshots.unwrap();
}
#[test]
fn search_settings_and_overlay_snapshots() {
    let _guard = egui_mev_test::snapshot_lock();
    let mut snapshots = egui_kittest::SnapshotResults::new();
    let mut app = fixture(Appearance::Dark, false, false);
    app.tabs.get_current_tab().expect("tab").search = Some(Search {
        value: "notes".into(),
        depth: 3,
        case_sensitive: true,
        chips: crate::app::search::query::parse("ext:pdf -draft").chips,
        ..Default::default()
    });
    let mut harness = Harness::builder()
        .with_options(egui_mev_test::snapshot_options())
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw, app);
    harness.run_steps(12);
    snapshots.add(harness.try_snapshot("dirfleet_search_bar"));
    harness.get_by_label("+ Filter").click();
    harness.run_steps(4);
    snapshots.add(harness.try_snapshot("dirfleet_search_options"));
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    harness.get_by_label("Extension: .pdf").click();
    harness.run_steps(6);
    snapshots.add(harness.try_snapshot("dirfleet_search_pill_editor"));
    let mut app = fixture(Appearance::Light, false, false);
    app.display_modal = Some(ModalWindow::Settings);
    let mut harness = Harness::builder()
        .with_options(egui_mev_test::snapshot_options())
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw, app);
    harness.run_steps(12);
    snapshots.add(harness.try_snapshot("dirfleet_settings_light"));
    // The dialog keeps its size and place on every page.
    let frame = harness.get_by_label("Close (Esc)").rect();
    for (page, name) in [
        ("Appearance", "appearance"),
        ("Files & folders", "files"),
        ("Previews", "previews"),
        ("Keyboard", "keyboard"),
    ] {
        harness.get_by_label(page).click();
        harness.run_steps(4);
        snapshots.add(harness.try_snapshot(format!("dirfleet_settings_{name}")));
        assert_eq!(harness.get_by_label("Close (Esc)").rect(), frame, "{page}");
    }
    // Narrow windows show the pages as tabs.
    let mut app = fixture(Appearance::Dark, false, false);
    app.display_modal = Some(ModalWindow::Settings);
    let mut harness = Harness::builder()
        .with_options(egui_mev_test::snapshot_options())
        .with_size(egui::vec2(640.0, 420.0))
        .build_ui_state(draw, app);
    harness.run_steps(12);
    snapshots.add(harness.try_snapshot("dirfleet_settings_compact"));
    let mut app = fixture(Appearance::Dark, false, false);
    app.tabs.window_mut().inspector_overlay = true;
    let mut harness = Harness::builder()
        .with_options(egui_mev_test::snapshot_options())
        .with_size(egui::vec2(640.0, 420.0))
        .build_ui_state(draw, app);
    harness.run_steps(12);
    snapshots.add(harness.try_snapshot("dirfleet_inspector_overlay"));
    snapshots.unwrap();
}
#[test]
fn selection_follows_paths_across_sort_filter_and_deletion() {
    let _guard = egui_mev_test::snapshot_lock();
    let ctx = egui::Context::default();
    let mut app = fixture(Appearance::Dark, false, true);
    let tab = app.tabs.get_current_tab().expect("tab");
    let _path = tab.current_path.clone();
    let id = tab.id;
    app.tabs
        .get_tab_by_id(id)
        .expect("tab")
        .selection
        .set_rows([1]);
    // Draw once to record the canonical file selection.
    let mut output = ctx.run_ui(egui::RawInput::default(), |root| {
        egui::CentralPanel::default().show(root, |ui| app.tabs.ui(ui, &mut app.assets));
    });
    output.textures_delta.clear();
    let selected_path = app.inspector_view(&ctx).expect("view").paths[0].clone();
    app.tabs
        .get_tab_by_id(id)
        .expect("tab")
        .visible_entries
        .reverse();
    app.tabs.reconcile_selection();
    assert_eq!(
        app.inspector_view(&ctx).expect("view").paths,
        vec![selected_path]
    );
    app.tabs.get_tab_by_id(id).expect("tab").visible_entries = vec![0, 3];
    app.tabs.reconcile_selection();
    assert_eq!(app.inspector_view(&ctx).expect("view").paths.len(), 0);
}
#[test]
fn auto_collapse_keeps_preferences_and_toggle_opens_overlay() {
    let _guard = egui_mev_test::snapshot_lock();
    let ctx = egui::Context::default();
    let mut app = fixture(Appearance::Dark, false, true);
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(640.0, 420.0),
            )),
            ..Default::default()
        },
        |root| {
            let ctx = root.ctx();
            app.handle_action(ctx, ActionToPerform::ToggleSidebar);
            app.handle_action(ctx, ActionToPerform::ToggleInspector);
        },
    );
    output.textures_delta.clear();
    assert!(app.settings.sidebar_visible && app.settings.inspector_visible);
    assert!(app.tabs.window().sidebar_overlay && app.tabs.window().inspector_overlay);
}

#[test]
fn inspector_selection_states_and_split_panes() {
    let _guard = egui_mev_test::snapshot_lock();
    let mut snapshots = egui_kittest::SnapshotResults::new();
    for (name, indices) in [
        ("dirfleet_inspector_empty", Vec::<usize>::new()),
        ("dirfleet_inspector_multiple", vec![1usize, 2]),
        ("dirfleet_inspector_folder", vec![0usize]),
    ] {
        let mut harness = Harness::builder()
            .with_options(egui_mev_test::snapshot_options())
            .with_size(egui::vec2(1100.0, 720.0))
            .build_ui_state(
                move |root, app| {
                    root.ctx().data_mut(|data| {
                        data.insert_temp(egui::Id::unique("initial_selection"), indices.clone())
                    });
                    draw(root, app);
                },
                fixture(Appearance::Dark, false, true),
            );
        harness.run_steps(12);
        snapshots.add(harness.try_snapshot(name));
    }
    let mut app = fixture(Appearance::Dark, false, false);
    let mut second = TabData::from_path(Path::new("/virtual/Downloads"));
    second.current_path = CurrentPath::One("/virtual/Downloads".into());
    second.set_entries(&[DirEntry::test_new(
        "/virtual/Downloads/A long downloaded file name.txt",
    )]);
    second.load = crate::app::dock::LoadState::Idle;
    let nodes = app.tabs.split_right(second);
    app.tabs
        .dock_mut()
        .set_focused_node_and_surface(egui_dock::NodePath::new(
            egui_dock::SurfaceIndex::main(),
            nodes[0],
        ));
    let mut harness = Harness::builder()
        .with_options(egui_mev_test::snapshot_options())
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw, app);
    harness.run_steps(12);
    snapshots.add(harness.try_snapshot("dirfleet_split_panes"));
    snapshots.unwrap();
}

#[test]
fn inspector_preview_loading_ready_and_failure() {
    let _guard = egui_mev_test::snapshot_lock();
    let mut snapshots = egui_kittest::SnapshotResults::new();
    for (name, state) in [
        ("dirfleet_preview_loading", 0),
        ("dirfleet_preview_failure", 1),
        ("dirfleet_preview_ready", 2),
    ] {
        let mut assets = assets::AssetManager::default();
        let mut harness = Harness::builder()
            .with_options(egui_mev_test::snapshot_options())
            .with_size(egui::vec2(320.0, 300.0))
            .build_ui(|root| {
                ui::configure_test(root.ctx(), false);
                root.ctx().set_theme(egui::Theme::Dark);
                egui::CentralPanel::default().show(root, |ui| {
                    let preview = match state {
                        0 => assets::Preview::Pending,
                        1 => assets::Preview::Unavailable {
                            reason: "Decoder unavailable".into(),
                            retry_after: None,
                        },
                        _ => assets::Preview::Ready(ui.ctx().load_texture(
                            "preview_fixture",
                            egui::ColorImage::filled(
                                [160, 90],
                                egui::Color32::from_rgb(30, 100, 180),
                            ),
                            egui::TextureOptions::LINEAR,
                        )),
                    };
                    ui::card(ui).show(ui, |ui| {
                        inspector::render_preview(
                            ui,
                            &mut assets,
                            &DirEntry::test_new("/virtual/movie.mp4"),
                            preview,
                            egui::vec2(240.0, 240.0),
                        );
                    });
                });
            });
        harness.run_steps(12);
        snapshots.add(harness.try_snapshot(name));
    }
    snapshots.unwrap();
}

#[test]
fn appearance_change_keeps_file_labels_visible() {
    let _guard = egui_mev_test::snapshot_lock();
    let mut harness = Harness::builder()
        .with_options(egui_mev_test::snapshot_options())
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw, fixture(Appearance::Dark, false, true));
    harness.run_steps(12);
    harness.state_mut().settings.appearance = Appearance::Light;
    harness.run_steps(12);
    harness.snapshot("dirfleet_appearance_changed_to_light");
}

#[test]
fn keyboard_selection_and_search_follow_the_active_pane() {
    let _guard = egui_mev_test::snapshot_lock();
    let mut harness = Harness::builder()
        .with_options(egui_mev_test::snapshot_options())
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw, fixture(Appearance::Dark, false, true));
    harness.run_steps(12);
    harness.hover_at(egui::pos2(380.0, 160.0));
    harness.run_steps(2);
    harness.key_press(egui::Key::ArrowDown);
    harness.run_steps(2);
    let ctx = harness.ctx.clone();
    let view = harness.state_mut().inspector_view(&ctx).expect("inspector");
    assert!(
        view.paths[0]
            .to_string_lossy()
            .contains("A very long file name")
    );
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
    harness.run_steps(2);
    assert!(
        harness
            .state_mut()
            .tabs
            .get_current_tab()
            .expect("tab")
            .search
            .is_some()
    );
    // Ctrl+F focuses an open search instead of closing it; Esc closes it.
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
    harness.run_steps(2);
    assert!(
        harness
            .state_mut()
            .tabs
            .get_current_tab()
            .expect("tab")
            .search
            .is_some()
    );
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    assert!(
        harness
            .state_mut()
            .tabs
            .get_current_tab()
            .expect("tab")
            .search
            .is_none()
    );
}

#[test]
fn command_palette_supports_keyboard_choice() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let mut app = fixture(Appearance::Dark, false, false);
    app.display_modal = Some(ModalWindow::Commands);
    app.command_palette.commands = vec![
        ActionToPerform::ToggleSidebar.into(),
        ActionToPerform::ToggleInspector.into(),
    ];
    let mut harness = Harness::builder()
        .with_options(egui_mev_test::snapshot_options())
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw, app);
    harness.run_steps(12);
    while COMMANDS_QUEUE.pop().is_some() {}
    let snapshot = harness.try_snapshot("dirfleet_commands");
    harness.key_press(egui::Key::ArrowDown);
    harness.run_steps(2);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(2);
    assert!(matches!(
        COMMANDS_QUEUE.pop(),
        Some(ActionToPerform::CloseActiveModalWindow)
    ));
    let action = COMMANDS_QUEUE.pop();
    assert!(
        matches!(action, Some(ActionToPerform::ToggleInspector)),
        "received {action:?}"
    );
    snapshot.expect("command palette snapshot");
}

/// A second window draws, gets the commands scheduled from it, and closes
/// with its tabs. (Tests draw one window per frame: embedded viewports
/// would share the main window's panel ids.)
#[test]
fn second_window_draws_takes_its_commands_and_closes() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let shown = egui::Id::unique("shown_window");
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(
            |root, app: &mut App| {
                let window = root
                    .ctx()
                    .data(|data| data.get_temp::<dock::WindowId>(shown))
                    .unwrap_or(dock::WindowId::MAIN);
                app.tabs.set_current_window(window);
                commands::in_window(window, || draw(root, app));
            },
            fixture(Appearance::Dark, false, false),
        );
    harness.run_steps(2);
    let ctx = harness.ctx.clone();
    let folder = std::env::temp_dir();
    harness
        .state_mut()
        .handle_action(&ctx, ActionToPerform::NewWindow(folder.clone()));
    let second = harness.state().tabs.window_ids()[1];
    harness.state_mut().drain_command_queue(&ctx);
    ctx.data_mut(|data| data.insert_temp(shown, second));
    harness.run_steps(4);
    ctx.data_mut(|data| data.insert_temp(shown, dock::WindowId::MAIN));
    harness.run_steps(1);
    let app = harness.state_mut();
    assert_eq!(app.tabs.window_ids(), vec![dock::WindowId::MAIN, second]);
    app.tabs.set_current_window(second);
    assert_eq!(app.tabs.get_current_path(), Some(folder));

    // "New tab" scheduled from the second window opens there.
    commands::in_window(second, || {
        ActionToPerform::RunCommand(keymap::Command::NewTab).schedule();
    });
    app.tabs.set_current_window(dock::WindowId::MAIN);
    app.drain_command_queue(&ctx);
    app.tabs.set_current_window(second);
    assert_eq!(app.tabs.window().tab_ids().count(), 2);
    app.tabs.set_current_window(dock::WindowId::MAIN);
    assert_eq!(app.tabs.window().tab_ids().count(), 1);

    harness.state_mut().tabs.close_window(second);
    harness.run_steps(2);
    let app = harness.state_mut();
    assert_eq!(app.tabs.window_ids(), vec![dock::WindowId::MAIN]);
    assert_eq!(app.tabs.take_retired().len(), 2);
}

#[test]
fn path_editor_takes_focus_and_compact_overlay_dismisses() {
    let _guard = egui_mev_test::snapshot_lock();
    let mut harness = Harness::builder()
        .with_options(egui_mev_test::snapshot_options())
        .with_size(egui::vec2(640.0, 420.0))
        .build_ui_state(draw, fixture(Appearance::Dark, false, true));
    harness.run_steps(12);
    let ctx = harness.ctx.clone();
    let id = harness.state_mut().tabs.get_current_tab().expect("tab").id;
    harness
        .state_mut()
        .handle_action(&ctx, ActionToPerform::ToggleTopEdit);
    harness.run_steps(2);
    assert_eq!(
        ctx.memory(egui::Memory::focused),
        Some(egui::Id::unique(("path_editor", id)))
    );
    while COMMANDS_QUEUE.pop().is_some() {}
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    let action = COMMANDS_QUEUE.pop().expect("close path editor");
    assert!(matches!(action, ActionToPerform::ToggleTopEdit));
    harness.state_mut().handle_action(&ctx, action);
    let tab = harness.state_mut().tabs.get_tab_by_id(id).expect("tab");
    assert!(tab.path_edit.is_none());
    harness
        .state_mut()
        .handle_action(&ctx, ActionToPerform::ToggleSidebar);
    harness.run_steps(12);
    assert!(harness.state().tabs.window().sidebar_overlay);
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    assert!(!harness.state().tabs.window().sidebar_overlay);
    assert!(harness.state().settings.sidebar_visible);
}

/// A window with the path editor open and focused on `info`.
fn path_editor_harness(
    appearance: Appearance,
    info: crate::app::directory_path_info::DirectoryPathInfo,
) -> Harness<'static, App> {
    let mut app = fixture(appearance, false, false);
    let tab = app.tabs.get_current_tab().expect("tab");
    tab.path_edit = Some(info);
    let id = tab.id;
    let mut harness = Harness::builder()
        .with_options(egui_mev_test::snapshot_options())
        .with_size(egui::vec2(900.0, 560.0))
        .build_ui_state(draw, app);
    harness.run_steps(4);
    harness
        .ctx
        .memory_mut(|memory| memory.request_focus(egui::Id::unique(("path_editor", id))));
    harness.run_steps(4);
    harness
}

/// What the editor asked for: where the tab should go, which new tab to open
/// and whether it closes.
fn take_path_actions() -> (Vec<PathBuf>, Vec<PathBuf>, bool) {
    let (mut moved, mut opened, mut closed) = (Vec::new(), Vec::new(), false);
    while let Some(action) = COMMANDS_QUEUE.pop() {
        match action {
            ActionToPerform::TabAction(_, commands::TabAction::ChangePaths(path)) => {
                moved.push(path.single_path().expect("one path"));
            }
            ActionToPerform::NewTab(path) => opened.push(path),
            ActionToPerform::ToggleTopEdit => closed = true,
            _ => {}
        }
    }
    (moved, opened, closed)
}

/// A real folder with `Alpha/Inner` and `Beta` in it.
fn path_editor_folder(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("lwa_fm_path_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Alpha").join("Inner")).expect("create folders");
    std::fs::create_dir_all(root.join("Beta")).expect("create folders");
    root
}

#[test]
fn path_editor_enter_opens_the_typed_path_not_the_hovered_row() {
    use crate::app::directory_path_info::DirectoryPathInfo;
    let _guard = egui_mev_test::snapshot_lock();
    let root = path_editor_folder("hover");
    let mut harness = path_editor_harness(Appearance::Dark, DirectoryPathInfo::build(&root));
    while COMMANDS_QUEUE.pop().is_some() {}
    harness.get_by_label("Beta").hover();
    harness.run_steps(2);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(1);
    let (moved, _, closed) = take_path_actions();
    assert_eq!(moved, std::slice::from_ref(&root));
    assert!(closed);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn path_editor_arrow_keys_choose_the_row_enter_opens() {
    use crate::app::directory_path_info::DirectoryPathInfo;
    let _guard = egui_mev_test::snapshot_lock();
    let root = path_editor_folder("arrows");
    let mut harness = path_editor_harness(Appearance::Dark, DirectoryPathInfo::build(&root));
    while COMMANDS_QUEUE.pop().is_some() {}
    harness.key_press(egui::Key::ArrowDown);
    harness.key_press(egui::Key::ArrowDown);
    harness.run_steps(1);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(1);
    let (moved, _, closed) = take_path_actions();
    assert_eq!(moved, [root.join("Beta")]);
    assert!(closed);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn path_editor_tab_completes_keeps_focus_and_lists_the_next_level() {
    use crate::app::directory_path_info::DirectoryPathInfo;
    let _guard = egui_mev_test::snapshot_lock();
    let root = path_editor_folder("tab");
    let mut harness = path_editor_harness(Appearance::Dark, DirectoryPathInfo::build(&root));
    let ctx = harness.ctx.clone();
    let focused = ctx.memory(egui::Memory::focused);
    assert!(focused.is_some());
    harness.key_press(egui::Key::Tab);
    harness.run_steps(3);
    assert_eq!(ctx.memory(egui::Memory::focused), focused);
    let tab = harness.state_mut().tabs.get_current_tab().expect("tab");
    let info = tab.path_edit.as_ref().expect("still editing");
    let expected = format!(
        "{}{}",
        root.join("Alpha").display(),
        std::path::MAIN_SEPARATOR
    );
    assert_eq!(info.text_input, expected);
    assert!(harness.query_by_label("Inner").is_some());
    assert!(harness.query_by_label("Beta").is_none());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn path_editor_click_navigates_and_command_click_opens_a_tab() {
    use crate::app::directory_path_info::DirectoryPathInfo;
    let _guard = egui_mev_test::snapshot_lock();
    let root = path_editor_folder("click");
    let mut harness = path_editor_harness(Appearance::Dark, DirectoryPathInfo::build(&root));
    while COMMANDS_QUEUE.pop().is_some() {}
    harness.get_by_label("Beta").click();
    harness.run_steps(2);
    let (moved, opened, closed) = take_path_actions();
    assert_eq!(moved, [root.join("Beta")]);
    assert_eq!(opened, Vec::<PathBuf>::new());
    assert!(closed);

    let mut harness = path_editor_harness(Appearance::Dark, DirectoryPathInfo::build(&root));
    while COMMANDS_QUEUE.pop().is_some() {}
    harness
        .get_by_label("Beta")
        .click_modifiers(egui::Modifiers::COMMAND);
    harness.run_steps(2);
    let (moved, opened, closed) = take_path_actions();
    assert_eq!(moved, Vec::<PathBuf>::new());
    assert_eq!(opened, [root.join("Beta")]);
    assert!(closed);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn path_editor_closes_when_pressed_outside_and_stays_when_pressed_inside() {
    use crate::app::directory_path_info::DirectoryPathInfo;
    let _guard = egui_mev_test::snapshot_lock();
    let root = Path::new("/virtual/Projects/DirFleet");
    let mut harness = path_editor_harness(
        Appearance::Dark,
        DirectoryPathInfo::with_children(root, &["Alpha", "Beta"]),
    );
    let press = |harness: &mut Harness<'static, App>, pos: egui::Pos2| {
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(pos));
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run_steps(2);
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run_steps(2);
    };
    let id = harness.state_mut().tabs.get_current_tab().expect("tab").id;
    let field = harness
        .ctx
        .read_response(egui::Id::unique(("path_editor", id)))
        .expect("path field")
        .rect
        .center();
    press(&mut harness, field);
    let tab = harness.state_mut().tabs.get_current_tab().expect("tab");
    assert!(tab.path_edit.is_some());
    press(&mut harness, egui::pos2(800.0, 500.0));
    let tab = harness.state_mut().tabs.get_current_tab().expect("tab");
    assert!(tab.path_edit.is_none());
}

#[test]
fn path_editor_scrolls_the_chosen_row_into_view() {
    use crate::app::directory_path_info::DirectoryPathInfo;
    let _guard = egui_mev_test::snapshot_lock();
    let names: Vec<String> = (0..40).map(|i| format!("Folder {i:02}")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let root = Path::new("/virtual/Projects/DirFleet");
    let mut harness = path_editor_harness(
        Appearance::Dark,
        DirectoryPathInfo::with_children(root, &names),
    );
    // Only the rows in view are drawn.
    assert!(harness.query_by_label("Folder 00").is_some());
    assert!(harness.query_by_label("Folder 30").is_none());
    for _ in 0..=30 {
        harness.key_press(egui::Key::ArrowDown);
    }
    harness.run_steps(2);
    assert!(harness.query_by_label("Folder 30").is_some());
    assert!(harness.query_by_label("Folder 00").is_none());
}

#[test]
fn path_editor_dropdown_snapshots() {
    use crate::app::directory_path_info::DirectoryPathInfo;
    let _guard = egui_mev_test::snapshot_lock();
    let mut snapshots = egui_kittest::SnapshotResults::new();
    let root = Path::new("/virtual/Projects/DirFleet");
    let names = [
        "24_Ambience_Sounds",
        "A folder with a name long enough that it has to be cut off before it reaches the edge of the dropdown list",
        "AltLogos",
        "Asset_Files",
        "Base Software",
        "bevy-main",
        "bevy-release-0.18.1",
        "binaryen-version_129",
        "blender-mcp-main",
        "bushes",
        "CaRpg_0.19",
    ];
    let typed = |text: &str| {
        let mut info = DirectoryPathInfo::with_children(root, &names);
        info.text_input = format!("{}/{text}", root.display());
        info.mark_edited();
        info
    };
    let mut harness = path_editor_harness(
        Appearance::Dark,
        DirectoryPathInfo::with_children(root, &names),
    );
    harness.key_press(egui::Key::ArrowDown);
    harness.run_steps(2);
    snapshots.add(harness.try_snapshot("dirfleet_path_editor_dark"));
    let mut harness = path_editor_harness(
        Appearance::Light,
        DirectoryPathInfo::with_children(root, &names),
    );
    harness.key_press(egui::Key::ArrowDown);
    harness.run_steps(2);
    snapshots.add(harness.try_snapshot("dirfleet_path_editor_light"));
    let mut harness = path_editor_harness(Appearance::Dark, typed("bev"));
    snapshots.add(harness.try_snapshot("dirfleet_path_editor_filtered"));
    let mut harness = path_editor_harness(Appearance::Dark, typed("zzz"));
    snapshots.add(harness.try_snapshot("dirfleet_path_editor_empty"));
    let mut shortcuts = typed("").with_shortcuts(
        vec![(
            "Invoices".to_owned(),
            PathBuf::from("/virtual/Work/Invoices"),
        )],
        &[PathBuf::from("/virtual/Archive/2026/Invoices old")],
    );
    "inv".clone_into(&mut shortcuts.text_input);
    let mut harness = path_editor_harness(Appearance::Dark, shortcuts);
    snapshots.add(harness.try_snapshot("dirfleet_path_editor_shortcuts"));
}

#[test]
fn keyboard_selects_and_renames_without_hovering_the_list() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw, fixture(Appearance::Dark, false, true));
    harness.run_steps(12);
    let visible = harness
        .state_mut()
        .tabs
        .get_current_tab()
        .expect("tab")
        .visible_entries
        .len();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness.run_steps(2);
    let selected = |harness: &mut Harness<'_, App>| {
        harness
            .state_mut()
            .tabs
            .get_current_tab()
            .expect("tab")
            .selection
            .len()
    };
    assert_eq!(selected(&mut harness), visible);
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    assert_eq!(selected(&mut harness), 0);
    harness.key_press(egui::Key::End);
    harness.run_steps(2);
    assert_eq!(selected(&mut harness), 1);
    harness.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Home);
    harness.run_steps(2);
    assert_eq!(selected(&mut harness), visible);
    // F2 with several items selected renames them together.
    harness.key_press(egui::Key::F2);
    harness.run_steps(2);
    assert_eq!(
        harness.state().display_modal,
        Some(ModalWindow::BatchRename)
    );
    let _ = harness.get_by_label("Find and replace");
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    // The test frame doesn't process queued actions; run them like the app.
    let ctx = harness.ctx.clone();
    harness.state_mut().drain_command_queue(&ctx);
    assert_eq!(harness.state().display_modal, None);
    // With one item, F2 renames just that one.
    harness.key_press(egui::Key::Home);
    harness.run_steps(2);
    assert_eq!(selected(&mut harness), 1);
    harness.key_press(egui::Key::F2);
    harness.run_steps(2);
    assert_eq!(harness.state().display_modal, Some(ModalWindow::Rename));
    let _ = harness.get_by_label("New name");
    while COMMANDS_QUEUE.pop().is_some() {}
}

#[test]
fn settings_switches_restore_defaults_and_close() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let mut app = fixture(Appearance::Dark, false, false);
    app.display_modal = Some(ModalWindow::Settings);
    let mut harness = Harness::builder()
        .with_options(egui_mev_test::snapshot_options())
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw, app);
    harness.run_steps(4);
    harness.get_by_label("Appearance").click();
    harness.run_steps(3);
    harness.get_by_label("File inspector").click();
    harness.run_steps(3);
    assert!(harness.state().settings.inspector_visible);

    // Restoring takes a second, confirming click. The button sits below the
    // page's rows, so scroll it into view first.
    harness.get_by_label("File inspector").hover();
    harness.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, -400.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers: egui::Modifiers::NONE,
    });
    harness.run_steps(4);
    harness.get_by_label("Restore defaults").click();
    harness.run_steps(2);
    assert!(harness.state().settings.inspector_visible, "not yet");
    harness.get_by_label("Click again to restore").click();
    harness.run_steps(2);
    assert!(!harness.state().settings.inspector_visible);

    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    let ctx = harness.ctx.clone();
    harness.state_mut().drain_command_queue(&ctx);
    assert_eq!(harness.state().display_modal, None);
    while COMMANDS_QUEUE.pop().is_some() {}
}

/// The fixture with 200 files, enough to scroll in either view.
fn long_fixture(grid: bool) -> App {
    let mut app = fixture(Appearance::Dark, grid, false);
    let root = Path::new("/virtual/Projects/DirFleet");
    let entries: Vec<DirEntry> = (0..200)
        .map(|i| DirEntry::test_new(&format!("{}/file {i:03}.txt", root.display())))
        .collect();
    app.tabs
        .get_current_tab()
        .expect("tab")
        .set_entries(&entries);
    app
}

/// The cursor row, and whether all of it was on screen when last drawn.
fn cursor_shown(harness: &mut Harness<'_, App>) -> (usize, bool) {
    let tab = harness.state_mut().tabs.get_current_tab().expect("tab");
    let cursor = tab.selection.cursor().expect("cursor");
    (cursor, tab.view.shows(cursor))
}

#[test]
fn keyboard_moves_keep_the_cursor_in_view() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    for grid in [false, true] {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1100.0, 720.0))
            .build_ui_state(draw, long_fixture(grid));
        harness.run_steps(6);
        let press = |harness: &mut Harness<'_, App>, key| {
            harness.key_press(key);
            harness.run_steps(3);
        };
        press(&mut harness, egui::Key::End);
        assert_eq!(cursor_shown(&mut harness), (199, true), "End, grid {grid}");
        press(&mut harness, egui::Key::Home);
        assert_eq!(cursor_shown(&mut harness), (0, true), "Home, grid {grid}");
        press(&mut harness, egui::Key::PageDown);
        press(&mut harness, egui::Key::PageDown);
        let (row, shown) = cursor_shown(&mut harness);
        assert!(row > 0 && shown, "Page Down to {row}, grid {grid}");

        // Stepping past the bottom edge scrolls by about one row, not by
        // half a screen.
        press(&mut harness, egui::Key::Home);
        let page = harness
            .state_mut()
            .tabs
            .get_current_tab()
            .expect("tab")
            .view
            .page_rows;
        let columns = harness
            .state_mut()
            .tabs
            .get_current_tab()
            .expect("tab")
            .view
            .columns;
        for _ in 0..=page {
            press(&mut harness, egui::Key::ArrowDown);
        }
        let before = harness
            .state_mut()
            .tabs
            .get_current_tab()
            .expect("tab")
            .view
            .scroll_y;
        press(&mut harness, egui::Key::ArrowDown);
        let view = &harness
            .state_mut()
            .tabs
            .get_current_tab()
            .expect("tab")
            .view;
        let moved = view.scroll_y - before;
        assert!(
            moved <= view.row_pitch * 1.5,
            "scrolled {moved} for one row of {}, grid {grid}",
            view.row_pitch
        );
        let (row, shown) = cursor_shown(&mut harness);
        assert_eq!(row, (page + 2) * columns, "grid {grid}");
        assert!(shown, "ArrowDown past the edge, grid {grid}");
    }

    // Type-to-select jumps to and reveals a name far down the list.
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw, long_fixture(false));
    harness.run_steps(6);
    harness.event(egui::Event::Text("file 150".into()));
    harness.run_steps(3);
    assert_eq!(cursor_shown(&mut harness), (150, true));
    while COMMANDS_QUEUE.pop().is_some() {}
}

#[test]
fn navigation_keeps_your_place() {
    use super::dock::ScrollRequest;
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let mut app = long_fixture(false);
    let tab = app.tabs.get_current_tab().expect("tab");
    let root = PathBuf::from("/virtual/Projects/DirFleet");
    tab.set_path(CurrentPath::One(root.clone()));
    // A folder shown for the first time starts at the top.
    assert_eq!(
        tab.take_scroll_request(false),
        Some(ScrollRequest::Offset(0.0))
    );
    tab.selection.select_only(42);
    tab.view.scroll_y = 900.0;

    // Going up selects the folder you came from.
    tab.set_path(CurrentPath::One(PathBuf::from("/virtual/Projects")));
    assert_eq!(
        tab.pending_select.as_ref().map(|p| p.paths.clone()),
        Some(vec![root.clone()])
    );
    assert_eq!(
        tab.take_scroll_request(false),
        Some(ScrollRequest::Offset(0.0))
    );

    // Back returns to the scroll position and selection you left.
    let _ = tab.go_back().expect("back");
    tab.set_path(CurrentPath::One(root.clone()));
    tab.load = crate::app::dock::LoadState::Idle;
    assert_eq!(
        tab.take_scroll_request(false),
        Some(ScrollRequest::Offset(900.0))
    );
    assert_eq!(
        tab.pending_select.as_ref().map(|p| p.paths.clone()),
        Some(vec![root.join("file 042.txt")])
    );
    while COMMANDS_QUEUE.pop().is_some() {}
}

#[test]
fn keyboard_cursor_ring_snapshot() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let mut harness = Harness::builder()
        .with_options(egui_mev_test::snapshot_options())
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw, fixture(Appearance::Dark, false, false));
    harness.run_steps(6);
    // Ctrl+↓ moves the cursor without selecting: the ring shows where.
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::ArrowDown);
    harness.run_steps(4);
    harness.snapshot("dirfleet_list_cursor");
    while COMMANDS_QUEUE.pop().is_some() {}
}

/// The pills in the open search, as their labels.
fn pill_labels(harness: &mut Harness<'_, App>) -> Vec<String> {
    harness
        .state_mut()
        .tabs
        .get_current_tab()
        .and_then(|tab| tab.search.as_ref())
        .map(|search| {
            search
                .chips
                .iter()
                .map(crate::app::search::query::chip_label)
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn search_pills_remove_edit_and_clear_with_mouse_and_keyboard() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let mut app = fixture(Appearance::Dark, false, false);
    let search = Search {
        depth: 1,
        chips: crate::app::search::query::parse("ext:pdf kind:image -draft").chips,
        ..Default::default()
    };
    let tab = app.tabs.get_current_tab().expect("tab");
    tab.search = Some(search);
    tab.search_committed.clone_from(&tab.search);
    let mut harness = Harness::builder()
        .with_options(egui_mev_test::snapshot_options())
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw, app);
    harness.run_steps(4);
    let all = ["Extension: .pdf", "Type: Image", "Not: draft"];
    assert_eq!(pill_labels(&mut harness), all);

    // The × removes its pill and doesn't open the editor underneath it.
    harness.get_by_label("Remove Extension: .pdf").click();
    harness.run_steps(3);
    assert_eq!(pill_labels(&mut harness), ["Type: Image", "Not: draft"]);
    assert!(harness.query_by_label("Done").is_none(), "no editor opened");

    // Middle-click and the right-click menu remove too.
    harness
        .get_by_label("Type: Image")
        .click_button(egui::PointerButton::Middle);
    harness.run_steps(3);
    assert_eq!(pill_labels(&mut harness), ["Not: draft"]);
    harness.get_by_label("Not: draft").click_secondary();
    harness.run_steps(3);
    harness.get_by_label("Remove").click();
    harness.run_steps(3);
    assert_eq!(pill_labels(&mut harness), Vec::<String>::new());

    // Each removal is one undo step.
    let ctx = harness.ctx.clone();
    for _ in 0..3 {
        harness.state_mut().undo(&ctx, false);
    }
    harness.run_steps(3);
    assert_eq!(pill_labels(&mut harness), all);

    // Keyboard: Backspace selects the last pill, Delete removes it, ←
    // moves, Enter edits and Esc in the editor puts the pill back.
    harness.key_press(egui::Key::Backspace);
    harness.run_steps(2);
    harness.key_press(egui::Key::Delete);
    harness.run_steps(2);
    assert_eq!(
        pill_labels(&mut harness),
        ["Extension: .pdf", "Type: Image"]
    );
    // The neighbour (Type) is selected now; ← then → comes back to it.
    harness.key_press(egui::Key::ArrowLeft);
    harness.run_steps(2);
    harness.key_press(egui::Key::ArrowRight);
    harness.run_steps(2);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(3);
    harness.get_by_label("Image").click();
    harness.run_steps(2);
    assert_eq!(pill_labels(&mut harness), ["Extension: .pdf", "Type: "]);
    harness.key_press(egui::Key::Escape);
    harness.run_steps(3);
    assert!(harness.query_by_label("Done").is_none(), "editor closed");
    assert_eq!(
        pill_labels(&mut harness),
        ["Extension: .pdf", "Type: Image"]
    );

    // "+ Filter → Extension…" edits the existing extension pill; a new
    // "Exclude a word…" pill left blank disappears without an undo step.
    harness.get_by_label("+ Filter").click();
    harness.run_steps(3);
    harness.get_by_label("Exclude a word…").click();
    harness.run_steps(4);
    assert_eq!(
        pill_labels(&mut harness).len(),
        3,
        "blank pill while editing"
    );
    harness.key_press(egui::Key::Escape);
    harness.run_steps(3);
    assert_eq!(
        pill_labels(&mut harness),
        ["Extension: .pdf", "Type: Image"]
    );

    // The clear button empties the field in one undoable step.
    harness.get_by_label("Clear search").click();
    harness.run_steps(3);
    assert_eq!(pill_labels(&mut harness), Vec::<String>::new());
    harness.state_mut().undo(&ctx, false);
    harness.run_steps(3);
    assert_eq!(
        pill_labels(&mut harness),
        ["Extension: .pdf", "Type: Image"]
    );
    while COMMANDS_QUEUE.pop().is_some() {}
}

#[test]
fn search_edits_undo_redo_and_back_restores_them() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let ctx = egui::Context::default();
    let mut app = fixture(Appearance::Dark, false, false);
    let tab_id = app.tabs.get_current_tab().expect("tab").id;
    let chips = |app: &mut App| {
        app.tabs
            .get_tab_by_id(tab_id)
            .and_then(|tab| tab.search.as_ref())
            .map(|search| search.chips.clone())
    };
    app.tabs.get_tab_by_id(tab_id).expect("tab").search = Some(Search::default());
    app.commit_search(tab_id, "open search");
    let ext = crate::app::search::query::parse("ext:pdf").chips;
    app.tabs
        .get_tab_by_id(tab_id)
        .and_then(|tab| tab.search.as_mut())
        .expect("search")
        .chips
        .clone_from(&ext);
    app.commit_search(tab_id, "add filter ext:pdf");
    assert_eq!(chips(&mut app), Some(ext.clone()));

    app.undo(&ctx, false);
    assert_eq!(chips(&mut app), Some(Vec::new()));
    app.undo(&ctx, true);
    assert_eq!(chips(&mut app), Some(ext));

    // Back steps through earlier searches like through folders.
    let back = app
        .tabs
        .get_tab_by_id(tab_id)
        .expect("tab")
        .go_back()
        .expect("history");
    let ActionToPerform::TabAction(_, TabAction::Restore(entry)) = back else {
        panic!("expected a history restore");
    };
    assert_eq!(entry.search.map(|search| search.chips), Some(Vec::new()));
    while COMMANDS_QUEUE.pop().is_some() {}
}

#[test]
fn saved_searches_save_update_delete_and_undo() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let ctx = egui::Context::default();
    let mut app = fixture(Appearance::Dark, false, false);
    let tab_id = app.tabs.get_current_tab().expect("tab").id;
    app.tabs.get_tab_by_id(tab_id).expect("tab").search = Some(Search {
        chips: crate::app::search::query::parse("report ext:pdf").chips,
        ..Default::default()
    });
    app.open_save_dialog(&ctx);
    let editor = app.saved_editor.take().expect("save dialog");
    app.finish_saved_search_editor(&ctx, editor, false);
    let saved = crate::app::saved_searches::load_saved(&ctx);
    assert_eq!(saved.searches.len(), 1);
    let id = saved.searches[0].id;
    assert_eq!(saved.searches[0].name, "report ext:pdf");
    let search = |app: &mut App| {
        app.tabs
            .get_tab_by_id(tab_id)
            .and_then(|tab| tab.search.clone())
            .expect("search")
    };
    assert_eq!(search(&mut app).saved_from, Some(id));

    // Change the open search, then update the saved one from it.
    app.tabs
        .get_tab_by_id(tab_id)
        .and_then(|tab| tab.search.as_mut())
        .expect("search")
        .chips
        .push(crate::app::search::Chip::Kind(vec![
            crate::app::search::FileKind::Document,
        ]));
    app.update_saved_from_tab(&ctx);
    let saved = crate::app::saved_searches::load_saved(&ctx);
    assert_eq!(saved.searches[0].query.chips.len(), 3);

    app.delete_saved_search(&ctx, id);
    assert_eq!(
        crate::app::saved_searches::load_saved(&ctx).searches,
        Vec::new()
    );
    app.undo(&ctx, false);
    let restored = crate::app::saved_searches::load_saved(&ctx);
    assert_eq!(restored.searches.len(), 1);
    assert_eq!(restored.searches[0].id, id);
    while COMMANDS_QUEUE.pop().is_some() {}
}

#[test]
fn sidebar_sections_collapse_and_reorder() {
    use crate::app::settings::SidebarSection;
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw, fixture(Appearance::Dark, false, false));
    harness.run_steps(4);
    harness.ctx.data_set_persisted(Locations {
        locations: vec![Location::from_path("/virtual/Starred", "Starred")],
    });
    harness.run_steps(4);
    let top = |harness: &Harness<'_, App>, label: &str| harness.get_by_label(label).rect().top();
    assert!(top(&harness, "Favorites") < top(&harness, "Locations"));

    harness.get_by_label("Locations").click();
    harness.run_steps(4);
    assert!(
        harness
            .state()
            .settings
            .is_section_collapsed(SidebarSection::Locations)
    );
    assert!(harness.query_by_label("Downloads").is_none());
    harness.get_by_label("Locations").click();
    harness.run_steps(4);
    assert!(harness.query_by_label("Downloads").is_some());

    harness
        .state_mut()
        .settings
        .move_sidebar_section(SidebarSection::Locations, 0);
    harness.run_steps(4);
    assert!(top(&harness, "Locations") < top(&harness, "Favorites"));
    while COMMANDS_QUEUE.pop().is_some() {}
}

#[test]
fn saved_search_sidebar_and_editor_snapshots() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let mut snapshots = egui_kittest::SnapshotResults::new();
    let mut harness = Harness::builder()
        .with_options(egui_mev_test::snapshot_options())
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw, fixture(Appearance::Dark, false, false));
    harness.run_steps(4);
    let ctx = harness.ctx.clone();
    ctx.data_set_persisted(crate::app::SavedSearches {
        searches: vec![
            crate::app::SavedSearch {
                id: 1,
                name: "Invoices".into(),
                query: Search {
                    chips: crate::app::search::query::parse("invoice ext:pdf modified:year").chips,
                    ..Default::default()
                },
                scope: crate::app::search::SavedScope::CurrentFolder,
            },
            crate::app::SavedSearch {
                id: 2,
                name: "Big videos".into(),
                query: Search {
                    chips: crate::app::search::query::parse("kind:video size:>1gb").chips,
                    depth: 4,
                    ..Default::default()
                },
                scope: crate::app::search::SavedScope::Favorites,
            },
        ],
    });
    harness.run_steps(8);
    snapshots.add(harness.try_snapshot("dirfleet_saved_searches_sidebar"));
    harness.state_mut().open_edit_dialog(&ctx, 1);
    harness.run_steps(8);
    let _ = harness.get_by_label("Save and run");
    snapshots.add(harness.try_snapshot("dirfleet_saved_search_editor"));
    while COMMANDS_QUEUE.pop().is_some() {}
    snapshots.unwrap();
}

/// Dragging across empty space selects the items in the rectangle, in
/// both views, and Ctrl-dragging adds to the selection.
#[test]
fn dragging_on_empty_space_selects_items() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    for grid in [false, true] {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1100.0, 720.0))
            .build_ui_state(draw, fixture(Appearance::Dark, grid, false));
        harness.run_steps(6);
        let selected = |harness: &mut Harness<'_, App>| {
            let tab = harness.state_mut().tabs.get_current_tab().expect("tab");
            tab.selection.rows().collect::<Vec<_>>()
        };
        // From empty space at the bottom right, up over every item.
        harness.drag_at(egui::pos2(1060.0, 600.0));
        harness.run_steps(2);
        harness.hover_at(egui::pos2(700.0, 400.0));
        harness.run_steps(2);
        harness.hover_at(egui::pos2(230.0, 70.0));
        harness.run_steps(2);
        assert_eq!(
            selected(&mut harness),
            [0, 1, 2, 3],
            "while dragging, grid {grid}"
        );
        harness.drop_at(egui::pos2(230.0, 70.0));
        harness.run_steps(2);
        assert_eq!(
            selected(&mut harness),
            [0, 1, 2, 3],
            "after the drop, grid {grid}"
        );

        // A click on empty space clears it again.
        harness.drag_at(egui::pos2(1060.0, 600.0));
        harness.drop_at(egui::pos2(1060.0, 600.0));
        harness.run_steps(2);
        assert!(
            selected(&mut harness).is_empty(),
            "click clears, grid {grid}"
        );
    }
    while COMMANDS_QUEUE.pop().is_some() {}
}

/// The header's menu shows extra columns, and dragging a column's edge
/// resizes it; both are remembered.
#[test]
fn list_columns_can_be_added_and_resized() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw, fixture(Appearance::Dark, false, false));
    harness.run_steps(6);
    let columns = |harness: &Harness<'_, App>| {
        harness
            .ctx
            .data_get_persisted::<crate::app::dock::ListColumns>()
            .unwrap_or_default()
    };
    assert!(harness.query_by_label("Kind").is_none());
    harness
        .get_by_label("Modified")
        .click_button(egui::PointerButton::Secondary);
    harness.run_steps(2);
    harness.get_by_label("Kind").click();
    harness.run_steps(4);
    assert!(columns(&harness).is_shown(crate::app::dock::Column::Kind));
    assert!(
        harness.query_by_label("Kind").is_some(),
        "Kind header shown"
    );

    // Drag the left edge of the Modified column 60 points to the left.
    let modified = harness.get_by_label("Modified").rect();
    let edge = egui::pos2(
        modified.left() - crate::app::dock::CELL_PADDING,
        modified.center().y,
    );
    harness.hover_at(edge);
    harness.run_steps(1);
    harness.drag_at(edge);
    harness.run_steps(1);
    for step in 1..=6 {
        harness.hover_at(edge - egui::vec2(10.0 * step as f32, 0.0));
        harness.run_steps(1);
    }
    harness.drop_at(edge - egui::vec2(60.0, 0.0));
    harness.run_steps(2);
    let width = columns(&harness).width(crate::app::dock::Column::Modified);
    assert!(
        width > crate::app::dock::Column::Modified.default_width() + 40.0,
        "Modified is {width} wide"
    );
    harness
        .ctx
        .data_set_persisted(crate::app::dock::ListColumns::default());
    while COMMANDS_QUEUE.pop().is_some() {}
}

// ── Top bar: layout, targets, names and contrast ─────────────────────────

/// Folders the top bar is checked with: deep, Unicode, a volume and Home.
fn bar_paths() -> Vec<PathBuf> {
    let mut paths = vec![
        PathBuf::from(
            "/virtual/Clients/Acme Corp/2026/Q3/Projects/Website Redesign/Assets/Images/Hero/Exports/Retina/Final",
        ),
        PathBuf::from("/virtual/Zdjęcia 📷/Ünïcødé — naïve café 🎉"),
        PathBuf::from("/Volumes/Backup SSD/Projects/2026"),
    ];
    if let Some(dirs) = directories::UserDirs::new() {
        paths.push(dirs.home_dir().to_path_buf());
    }
    paths
}

/// The fixture at `path`, arrived at from its parent, optionally searching.
fn bar_app(path: &Path, search: bool) -> App {
    let mut app = fixture(Appearance::Light, false, false);
    let tab = app.tabs.get_current_tab().expect("tab");
    if let Some(parent) = path.parent() {
        tab.history.visit(super::history::HistoryEntry::new(
            CurrentPath::One(parent.to_path_buf()),
            None,
        ));
    }
    tab.history.visit(super::history::HistoryEntry::new(
        CurrentPath::One(path.to_path_buf()),
        None,
    ));
    tab.current_path = CurrentPath::One(path.to_path_buf());
    tab.top_display_path.build(path);
    if search {
        tab.search = Some(Search {
            value: "invoice".into(),
            depth: 4,
            chips: crate::app::search::query::parse("ext:pdf").chips,
            ..Default::default()
        });
    }
    app
}

fn bar_harness(path: &Path, search: bool, fluent: bool, width: f32) -> Harness<'static, App> {
    let builder = Harness::builder().with_size(egui::vec2(width, 420.0));
    let app = bar_app(path, search);
    let mut harness = if fluent {
        builder.build_ui_state(draw_fluent, app)
    } else {
        builder.build_ui_state(draw, app)
    };
    harness.run_steps(12);
    harness
}

/// The controls of the bars above the file list: (role, name, rect).
fn bar_controls(harness: &Harness<'_, App>) -> Vec<(String, String, egui::Rect)> {
    use egui::accesskit::Role;
    use egui_kittest::kittest::NodeT as _;
    let nodes: Vec<_> = harness
        .root()
        .children_recursive()
        .filter(|node| node.accesskit_node().bounding_box().is_some())
        .collect();
    // The list's header row starts where the bars end.
    let list_top = nodes
        .iter()
        .filter(|node| {
            let ak = node.accesskit_node();
            ak.label()
                .or_else(|| ak.value())
                .is_some_and(|text| text.starts_with("Name"))
        })
        .map(|node| node.rect().top())
        .fold(f32::INFINITY, f32::min);
    nodes
        .iter()
        .filter(|node| {
            matches!(
                node.accesskit_node().role(),
                Role::Button
                    | Role::CheckBox
                    | Role::RadioButton
                    | Role::ComboBox
                    | Role::TextInput
            )
        })
        .map(|node| {
            let ak = node.accesskit_node();
            (
                format!("{:?}", ak.role()),
                ak.label().unwrap_or_default(),
                node.rect(),
            )
        })
        .filter(|(_, _, rect)| rect.top() < list_top && rect.width() > 0.0)
        .collect()
}

#[test]
fn top_bar_fits_at_minimum_width() {
    let _guard = egui_mev_test::snapshot_lock();
    for fluent in [false, true] {
        for width in [480.0, 800.0] {
            for path in bar_paths() {
                for search in [false, true] {
                    let harness = bar_harness(&path, search, fluent, width);
                    let controls = bar_controls(&harness);
                    let at = format!("{} at {width}px, fluent {fluent}", path.display());
                    for (i, (_, a, ra)) in controls.iter().enumerate() {
                        assert!(
                            ra.left() >= -0.5 && ra.right() <= width + 0.5,
                            "{a:?} leaves the window ({at})"
                        );
                        for (_, b, rb) in &controls[i + 1..] {
                            let overlap = ra.intersect(*rb);
                            // The path field holds its crumbs, which is not
                            // an overlap.
                            let nested = ra.contains_rect(*rb) || rb.contains_rect(*ra);
                            assert!(
                                overlap.width() <= 1.0 || overlap.height() <= 1.0 || nested,
                                "{a:?} overlaps {b:?} ({at})"
                            );
                        }
                    }
                    while COMMANDS_QUEUE.pop().is_some() {}
                }
            }
        }
    }
}

#[test]
fn top_bar_targets_are_large_enough() {
    let _guard = egui_mev_test::snapshot_lock();
    // WCAG 2.5.8: at least 24 by 24.
    for fluent in [false, true] {
        for path in bar_paths() {
            let harness = bar_harness(&path, true, fluent, 1024.0);
            for (role, name, rect) in bar_controls(&harness) {
                if role == "TextInput" {
                    // The search text sits inside the larger field.
                    continue;
                }
                assert!(
                    rect.width() >= 24.0 - 0.5 && rect.height() >= 24.0 - 0.5,
                    "{role} {name:?} is {:.0}x{:.0} ({})",
                    rect.width(),
                    rect.height(),
                    path.display()
                );
            }
            while COMMANDS_QUEUE.pop().is_some() {}
        }
    }
}

#[test]
fn top_bar_controls_are_named() {
    let _guard = egui_mev_test::snapshot_lock();
    let private_use = |c: char| ('\u{E000}'..='\u{F8FF}').contains(&c);
    for fluent in [false, true] {
        for width in [480.0, 1024.0] {
            for path in bar_paths() {
                let harness = bar_harness(&path, true, fluent, width);
                for (role, name, rect) in bar_controls(&harness) {
                    assert!(
                        !name.trim().is_empty() && !name.chars().any(private_use),
                        "{role} at x={:.0} is named {name:?} ({}, {width}px)",
                        rect.left(),
                        path.display()
                    );
                }
                while COMMANDS_QUEUE.pop().is_some() {}
            }
        }
    }
}

#[test]
fn top_bar_names_hidden_folders_and_marks_the_shown_panels() {
    let _guard = egui_mev_test::snapshot_lock();
    let deep = &bar_paths()[0];
    // Wide enough for the whole path: no "…".
    let harness = bar_harness(deep, false, false, 2560.0);
    assert!(harness.query_by_label("Acme Corp").is_some());
    assert!(harness.query_by_label("Final").is_some());
    assert!(harness.query_by_label("Folders in Retina").is_some());
    // Narrower: the middle collapses, the nearest parents stay.
    let harness = bar_harness(deep, false, false, 1024.0);
    assert!(harness.query_by_label("Acme Corp").is_none());
    assert!(harness.query_by_label("Retina").is_some());
    assert!(harness.query_by_label("Final").is_some());
    let more = harness
        .get_all_by(|node| node.label().is_some_and(|l| l.ends_with("more folders")))
        .count();
    assert_eq!(more, 1);
    // The panel toggles live in More and show what is on screen: below
    // 800px the sidebar is hidden whatever the preference.
    for (width, shown) in [(1100.0, true), (640.0, false)] {
        let mut harness = bar_harness(deep, false, false, width);
        assert!(harness.query_by_label("Toggle sidebar").is_none());
        harness.get_by_label("More actions").click();
        harness.run_steps(2);
        // Menu entries carry their shortcut in the name.
        let toggle = harness.get_by(|node| {
            node.label()
                .is_some_and(|l| l.starts_with("Toggle sidebar"))
        });
        let node = egui_kittest::kittest::NodeT::accesskit_node(&toggle);
        let on = node.is_selected() == Some(true)
            || node.toggled() == Some(egui::accesskit::Toggled::True);
        assert_eq!(on, shown, "sidebar at {width}px: {node:?}");
    }
    while COMMANDS_QUEUE.pop().is_some() {}
}

#[test]
fn theme_tokens_meet_contrast() {
    fn linear(c: u8) -> f64 {
        let c = f64::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    fn contrast(a: egui::Color32, b: egui::Color32) -> f64 {
        let lum = |c: egui::Color32| {
            0.2126f64.mul_add(
                linear(c.r()),
                0.7152f64.mul_add(linear(c.g()), 0.0722 * linear(c.b())),
            )
        };
        let (a, b) = (lum(a), lum(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
    for fluent in [false, true] {
        let ctx = egui::Context::default();
        ui::configure_test(&ctx, fluent);
        for dark in [false, true] {
            let p = egui_mev_theme::Theme::of(&ctx).palette(dark);
            let at = format!("fluent {fluent}, dark {dark}");
            assert!(contrast(p.text, p.toolbar_bg) >= 4.5, "text ({at})");
            assert!(
                contrast(p.text_weak, p.toolbar_bg) >= 4.5,
                "secondary text ({at})"
            );
            assert!(
                contrast(p.text_weak, p.card_bg) >= 4.5,
                "secondary on fields ({at})"
            );
            assert!(contrast(p.accent, p.toolbar_bg) >= 3.0, "focus ring ({at})");
        }
    }
}

#[test]
fn toolbar_search_field_opens_takes_typing_and_closes_with_escape() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let mut harness = bar_harness(&bar_paths()[0], false, false, 1100.0);
    let ctx = harness.ctx.clone();
    let run_queued = |harness: &mut Harness<'_, App>| {
        while let Some(action) = COMMANDS_QUEUE.pop() {
            harness.state_mut().handle_action(&ctx, action);
        }
        harness.run_steps(2);
    };
    let search = |harness: &mut Harness<'_, App>| {
        harness
            .state_mut()
            .tabs
            .get_current_tab()
            .expect("tab")
            .search
            .clone()
    };
    // Focusing the idle field in the bar opens the search there.
    harness.get_by_label("Search").click();
    harness.run_steps(2);
    run_queued(&mut harness);
    assert!(search(&mut harness).is_some());
    assert!(harness.query_by_label("+ Filter").is_some());
    harness.get_by_label("Search").type_text("notes");
    harness.run_steps(2);
    run_queued(&mut harness);
    assert_eq!(search(&mut harness).expect("open").value, "notes");
    // Esc clears the text, then closes.
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    run_queued(&mut harness);
    assert_eq!(search(&mut harness).expect("still open").value, "");
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    run_queued(&mut harness);
    assert!(search(&mut harness).is_none());
    assert!(harness.query_by_label("+ Filter").is_none());
    while COMMANDS_QUEUE.pop().is_some() {}
}

#[test]
fn breadcrumbs_keep_their_size_when_hovered() {
    let _guard = egui_mev_test::snapshot_lock();
    for fluent in [false, true] {
        let mut harness = bar_harness(&bar_paths()[0], false, fluent, 1100.0);
        let rects = |harness: &Harness<'_, App>| {
            ["Exports", "Folders in Exports", "Retina", "Final"]
                .map(|label| harness.get_by_label(label).rect())
        };
        let before = rects(&harness);
        for label in ["Exports", "Folders in Exports", "Retina"] {
            harness.get_by_label(label).hover();
            harness.run_steps(2);
            assert_eq!(
                rects(&harness),
                before,
                "hovering {label} (fluent {fluent})"
            );
        }
    }
    while COMMANDS_QUEUE.pop().is_some() {}
}

/// Items dropped onto a folder wait for confirmation, and Esc leaves them
/// where they were. The delete dialog uses the same button row.
#[test]
fn dropping_items_on_a_folder_asks_first() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut snapshots = egui_kittest::SnapshotResults::new();
    let mut harness = Harness::builder()
        .with_options(egui_mev_test::snapshot_options())
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw_fluent, fixture(Appearance::Dark, false, false));
    harness.run_steps(12);
    let ctx = harness.ctx.clone();
    harness.state_mut().drop_transfer(
        &ctx,
        vec![PathBuf::from("/virtual/Projects/DirFleet/Design notes.txt")],
        PathBuf::from("/virtual/Documents"),
        true,
    );
    harness.run_steps(4);
    assert_eq!(
        harness.state().display_modal,
        Some(ModalWindow::ConfirmDrop)
    );
    let _ = harness.get_by_label("Move");
    let _ = harness.get_by_label("Copy");
    #[cfg(windows)]
    snapshots.add(harness.try_snapshot("fluent_confirm_drop"));
    while COMMANDS_QUEUE.pop().is_some() {}
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    assert!(harness.state().pending_drop.is_none());
    let queued: Vec<_> = std::iter::from_fn(|| COMMANDS_QUEUE.pop()).collect();
    assert!(
        queued
            .iter()
            .any(|action| matches!(action, ActionToPerform::CloseActiveModalWindow))
    );
    assert!(
        !queued.iter().any(|action| matches!(
            action,
            ActionToPerform::FileOp(_) | ActionToPerform::FileOpEvent(_)
        )),
        "nothing was moved: {queued:?}"
    );

    harness.state_mut().pending_delete = file_ops::PendingDelete {
        paths: vec![PathBuf::from("/virtual/Projects/DirFleet/Design notes.txt")],
        permanent: true,
        ..Default::default()
    };
    harness.state_mut().display_modal = Some(ModalWindow::ConfirmDelete);
    harness.run_steps(4);
    let _ = harness.get_by_label("Delete");
    #[cfg(windows)]
    snapshots.add(harness.try_snapshot("fluent_confirm_delete"));
    while COMMANDS_QUEUE.pop().is_some() {}
    snapshots.unwrap();
}

/// Runs frames until `done`, reporting what the plugins were doing if never.
#[cfg(unix)]
fn wait(
    harness: &mut Harness<'_, App>,
    what: &str,
    mut done: impl FnMut(&Harness<'_, App>) -> bool,
) {
    for _ in 0..1500 {
        harness.step();
        if done(harness) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let plugins = &harness.state().plugins;
    let jobs: Vec<_> = plugins
        .finished
        .iter()
        .map(|job| (&job.summary, &job.failures))
        .collect();
    panic!(
        "timed out waiting for {what}; finished jobs: {jobs:?}, panels: {}, form open: {}, running: {}",
        plugins.panels.len(),
        plugins.form.is_some(),
        plugins.jobs.len(),
    );
}

/// A harness running the real app loop: queue, plugin workers and modals.
#[cfg(unix)]
fn plugin_harness(app: App) -> Harness<'static, App> {
    Harness::builder()
        .with_size(egui::vec2(1000.0, 700.0))
        // Fast enough for two clicks to make a double-click.
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |root, app: &mut App| {
                let ctx = root.ctx().clone();
                let id = egui::Id::unique("fonts_set");
                if !ctx.data(|data| data.get_temp::<bool>(id).unwrap_or(false)) {
                    ui::configure_test(&ctx, false);
                    ctx.data_mut(|data| data.insert_temp(id, true));
                    return;
                }
                app.drain_command_queue(&ctx);
                app.poll_plugins();
                // What the app itself draws, so a dialog only appears when
                // the app would really show it.
                app.render_window(root);
                ctx.request_repaint();
            },
            app,
        )
}

/// The app in `dir` with the bundled plugins, once they are running.
#[cfg(unix)]
fn running_plugins(dir: &Path) -> Harness<'static, App> {
    let mut tab = TabData::from_path(dir);
    tab.load = crate::app::dock::LoadState::Idle;
    tab.current_path = CurrentPath::One(dir.to_path_buf());
    let mut tabs = Workspace::from_tabs(vec![tab]);
    tabs.focused = true;
    let mut harness = plugin_harness(App {
        tabs,
        plugins: crate::plugins::PluginHost::bundled_only(),
        ..Default::default()
    });
    wait(&mut harness, "plugins to start", |harness| {
        harness
            .state()
            .plugins
            .plugins
            .iter()
            .all(|plugin| matches!(plugin.status, crate::plugins::host::Status::Running))
    });
    harness
}

/// Actions from the Commands window that open a form must leave it open,
/// both a form from the manifest and one Lua builds.
#[cfg(unix)]
#[test]
fn commands_window_opens_plugin_forms() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let dir = std::env::temp_dir()
        .canonicalize()
        .expect("temp dir")
        .join(format!("dirfleet_plugins_palette_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creates");
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"palette\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("writes");
    let mut harness = running_plugins(&dir);

    for (action, label) in [
        (
            "io.github.leinnan.folder-usage.scan",
            "Rust projects anywhere below",
        ),
        ("io.github.leinnan.rust-clean.clear_target", "Close"),
    ] {
        let app = harness.state_mut();
        app.command_palette.commands = vec![command_palette::ValidAction {
            action: ActionToPerform::RunPluginAction {
                action: action.to_owned(),
                context: crate::plugins::CallContext::default(),
            },
            name: action.to_owned().into(),
            shortcut: None,
        }];
        app.display_modal = Some(ModalWindow::Commands);
        for _ in 0..6 {
            harness.step();
        }
        harness.key_press(egui::Key::Enter);
        wait(&mut harness, action, |harness| {
            harness.state().display_modal.is_some()
                && harness.state().display_modal != Some(ModalWindow::Commands)
                && harness.query_by_label(label).is_some()
        });
        harness.state_mut().plugins.form = None;
        harness.state_mut().display_modal = None;
        for _ in 0..3 {
            harness.step();
        }
    }
    finish_plugins(harness);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A plugin whose job shows its option and folder, then runs until the file
/// `finish` exists in that folder, so a test decides how long it takes.
#[cfg(unix)]
fn slow_plugin() -> std::sync::Arc<crate::plugins::PluginCode> {
    let manifest = r#"
api_version = 1
id = "test.slow"
name = "Slow"
version = "1.0.0"
author = "Test"
permissions = ["fs.read"]

[activation]
always = true

[[action]]
id = "scan"
title = "Slow…"
targets = { kind = "current_dir" }
form = "static"
mode = "job"
results = true

[[action.field]]
id = "mode"
type = "radio"
options = [["first", "First option"], ["second", "Second option"]]
default = "first"

[[action]]
id = "refresh"
title = "Refresh"
palette = false
targets = { kind = "current_dir" }
mode = "job"

[[action]]
id = "confirm"
title = "Confirm"
palette = false
targets = { kind = "current_dir" }
form = "static"
mode = "job"

[[action.field]]
id = "sure"
type = "bool"
label = "Sure about it"
"#;
    let code = r#"
local M = { actions = {} }
local function show(ctx, values, stage, progress)
  dirfleet.app.show_table({
    title = "Slow",
    progress = progress,
    refresh_action = "refresh",
    columns = { { id = "name", label = "Name" }, { id = "size", label = "Bytes", kind = "size" } },
    rows = {
      { path = ctx.dir, cells = { name = values.mode .. " " .. stage .. " " .. dirfleet.path.name(ctx.dir), size = 2147483648 },
        menu = { { label = "Do it…", action = "test.slow.confirm", danger = true, busy = "Doing it…", title = "Do it?" } } },
      { path = dirfleet.path.join(ctx.dir, "sub"), cells = { name = "other", size = 1048576 },
        menu = { { label = "Do it…", action = "test.slow.confirm", danger = true, busy = "Doing it…" } } },
    },
    sort = { column = "size", descending = true },
  })
end
M.actions.scan = {
  run = function(ctx, values)
    show(ctx, values, "partial", "Scanning 1 of 2")
    while not dirfleet.fs.exists(dirfleet.path.join(ctx.dir, "finish")) do
      if dirfleet.job.cancelled() then
        return { skipped = true }
      end
    end
    show(ctx, values, "done")
  end,
}
M.actions.confirm = {
  run = function(ctx)
    while not dirfleet.fs.exists(dirfleet.path.join(ctx.dir, "release")) do
      if dirfleet.job.cancelled() then
        return { skipped = true }
      end
    end
    if dirfleet.fs.exists(dirfleet.path.join(ctx.dir, "fail")) then
      return { error = "boom" }
    end
  end,
}
M.actions.refresh = {
  run = function(ctx)
    dirfleet.app.update_table_row({ path = ctx.dir, cells = { size = 3072 } })
  end,
}
return M
"#;
    std::sync::Arc::new(crate::plugins::PluginCode {
        manifest: crate::plugins::manifest::parse(manifest).expect("manifest"),
        files: std::collections::BTreeMap::from([("main.lua".to_owned(), code.to_owned())]),
        source: crate::plugins::Source::Bundled,
    })
}

/// The app with the slow plugin running, and two folders to run it on.
#[cfg(unix)]
fn slow_harness(name: &str) -> (Harness<'static, App>, PathBuf, PathBuf, PathBuf) {
    let base = std::env::temp_dir()
        .canonicalize()
        .expect("temp dir")
        .join(format!("dirfleet_plugins_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (one, two) = (base.join("one"), base.join("two"));
    std::fs::create_dir_all(&one).expect("creates");
    std::fs::create_dir_all(&two).expect("creates");
    let mut tab = TabData::from_path(&one);
    tab.load = crate::app::dock::LoadState::Idle;
    tab.current_path = CurrentPath::One(one.clone());
    let mut tabs = Workspace::from_tabs(vec![tab]);
    tabs.focused = true;
    let mut harness = plugin_harness(App {
        tabs,
        plugins: crate::plugins::PluginHost::with_plugins(vec![slow_plugin()]),
        ..Default::default()
    });
    wait(&mut harness, "the plugin to start", |harness| {
        matches!(
            harness.state().plugins.plugins[0].status,
            crate::plugins::host::Status::Running
        )
    });
    (harness, base, one, two)
}

#[cfg(unix)]
fn open_slow(harness: &mut Harness<'_, App>, dir: &Path) {
    harness.state_mut().run_plugin_action(
        "test.slow.scan",
        crate::plugins::CallContext {
            dir: Some(dir.to_path_buf()),
            ..Default::default()
        },
    );
    settle(harness);
}

/// Ends a test with running plugins: nothing they still had going may reach
/// the next test, since the command queue is shared.
#[cfg(unix)]
fn finish_plugins(mut harness: Harness<'static, App>) {
    let jobs: Vec<u64> = harness
        .state()
        .plugins
        .jobs
        .iter()
        .map(|job| job.id)
        .collect();
    for job in jobs {
        harness.state_mut().plugins.cancel_job(job);
    }
    jobs_end(&mut harness);
    drop(harness);
    while COMMANDS_QUEUE.pop().is_some() {}
}

/// Clicks a status bar item once it has stopped moving: closing a dialog
/// refreshes the file list, and "Loading…" pushes the items along meanwhile.
#[cfg(unix)]
fn click_status_item(harness: &mut Harness<'_, App>, label: &str) {
    wait(harness, "the file list to settle", |harness| {
        harness
            .state()
            .tabs
            .tabs()
            .all(|tab| !tab.load.is_loading())
    });
    settle(harness);
    harness.get_by_label_contains(label).click();
    settle(harness);
}

/// Lets a dialog finish moving during its first frames, or anything queued land.
#[cfg(unix)]
fn settle(harness: &mut Harness<'_, App>) {
    for _ in 0..6 {
        harness.step();
    }
}

#[cfg(unix)]
fn click_run(harness: &mut Harness<'_, App>, button: &str, row: &str) {
    harness.get_by_label(button).click();
    wait(harness, row, |harness| {
        harness.query_by_label_contains(row).is_some()
    });
    settle(harness);
}

#[cfg(unix)]
fn jobs_end(harness: &mut Harness<'_, App>) {
    wait(harness, "the job to end", |harness| {
        harness.state().plugins.jobs.is_empty()
    });
    settle(harness);
}

/// Results show as they arrive, in the dialog with the options, with no other
/// window needed. Hiding leaves the job running and the status bar comes back.
#[cfg(unix)]
#[test]
fn results_dialog_runs_in_the_background() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let (mut harness, base, one, _) = slow_harness("background");

    open_slow(&mut harness, &one);
    assert_eq!(
        harness.state().display_modal,
        Some(ModalWindow::PluginResults)
    );
    assert!(harness.query_by_label("First option").is_some());
    assert!(
        harness.query_by_label("Stop").is_none(),
        "nothing to stop yet"
    );
    click_run(&mut harness, "Run", "first partial one");
    assert!(harness.query_by_label("Restart").is_some());
    assert!(harness.query_by_label("Scanning 1 of 2").is_some());
    harness.get_by_label("Bytes").click();
    settle(&mut harness);
    let sorted = harness.state().plugins.panels[0]
        .table
        .as_ref()
        .and_then(crate::plugins::table::TableView::sorted_by);

    // Esc hides, and the job goes on.
    harness.key_press(egui::Key::Escape);
    settle(&mut harness);
    assert_eq!(harness.state().display_modal, None);
    assert_eq!(harness.state().plugins.jobs.len(), 1, "still running");
    assert!(
        harness
            .query_by_label_contains("first partial one")
            .is_none()
    );

    // The status bar brings it back as it was.
    click_status_item(&mut harness, "Slow: Scanning 1 of 2");
    assert_eq!(
        harness.state().display_modal,
        Some(ModalWindow::PluginResults)
    );
    assert!(
        harness
            .query_by_label_contains("first partial one")
            .is_some()
    );
    let table = harness.state().plugins.panels[0].table.as_ref();
    assert_eq!(
        table.and_then(crate::plugins::table::TableView::sorted_by),
        sorted
    );

    // Hide with the button; it ends while hidden and doesn't pop up.
    harness.get_by_label("Run in background").click();
    settle(&mut harness);
    assert_eq!(harness.state().display_modal, None);
    std::fs::write(one.join("finish"), "").expect("writes");
    jobs_end(&mut harness);
    assert_eq!(harness.state().display_modal, None, "no pop-up");

    click_status_item(&mut harness, "Slow: results");
    assert!(harness.query_by_label_contains("first done one").is_some());
    assert!(harness.query_by_label_contains("Updated").is_some());
    assert!(harness.query_by_label("Run").is_some());

    // Opening a row hides the dialog, and the way back stays.
    // (Clicks soon after the last one would count as a triple click.)
    for _ in 0..60 {
        harness.step();
    }
    let row = harness.get_by_label_contains("first done one");
    row.click();
    harness.step();
    let row = harness.get_by_label_contains("first done one");
    row.click();
    settle(&mut harness);
    assert_eq!(harness.state().display_modal, None);
    assert!(
        harness
            .state()
            .tabs
            .tabs()
            .filter(|tab| tab.current_path.single_path() == Some(one.clone()))
            .count()
            >= 2,
        "opened in a new tab"
    );
    assert!(harness.query_by_label("Slow: results").is_some());

    // The action brings it back too, and the status bar's × forgets it.
    open_slow(&mut harness, &one);
    assert!(harness.query_by_label_contains("first done one").is_some());
    harness.key_press(egui::Key::Escape);
    settle(&mut harness);
    click_status_item(&mut harness, "Discard results");
    assert!(harness.state().plugins.panels.is_empty());
    assert!(harness.query_by_label("Slow: results").is_none());

    finish_plugins(harness);
    let _ = std::fs::remove_dir_all(&base);
}

/// Stop keeps what was found; Restart uses the current options, and the run it
/// replaced can't change the results any more.
#[cfg(unix)]
#[test]
fn results_dialog_stops_and_restarts() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let (mut harness, base, one, two) = slow_harness("restart");

    open_slow(&mut harness, &one);
    click_run(&mut harness, "Run", "first partial one");

    // While it runs, opening it from elsewhere stays on this run.
    harness.key_press(egui::Key::Escape);
    settle(&mut harness);
    open_slow(&mut harness, &two);
    assert_eq!(
        harness.state().plugins.panels[0].context.dir,
        Some(one.clone()),
        "a run in progress keeps its folder"
    );
    let old_job = harness.state().plugins.panels[0].job;

    // Restart with the other option.
    harness.get_by_label("Second option").click();
    settle(&mut harness);
    click_run(&mut harness, "Restart", "second partial one");
    assert_ne!(harness.state().plugins.panels[0].job, old_job);
    assert!(harness.query_by_label_contains("first partial").is_none());
    assert_eq!(harness.state().plugins.jobs.len(), 1);

    // A late update from the replaced run is dropped.
    let ctx = harness.ctx.clone();
    let spec = serde_json::from_value(serde_json::json!({
        "title": "Old",
        "columns": [{ "id": "name", "label": "Name" }],
        "rows": [{ "cells": { "name": "stale row" } }],
    }))
    .expect("spec");
    harness.state_mut().handle_plugin_message(
        &ctx,
        crate::plugins::PluginMsg::App(crate::plugins::AppRequest::ShowTable {
            plugin: "test.slow".into(),
            job: old_job,
            spec,
        }),
    );
    settle(&mut harness);
    assert!(harness.query_by_label_contains("stale row").is_none());
    assert!(
        harness
            .query_by_label_contains("second partial one")
            .is_some()
    );

    // Stop keeps the rows and says so.
    harness.get_by_label("Stop").click();
    jobs_end(&mut harness);
    assert!(
        harness
            .query_by_label_contains("second partial one")
            .is_some()
    );
    assert!(harness.query_by_label("Stopped early").is_some());
    assert!(harness.query_by_label("Run").is_some());
    assert_eq!(
        harness.state().display_modal,
        Some(ModalWindow::PluginResults)
    );

    // Idle now: from another folder the old results stay, labelled, until Run.
    harness.key_press(egui::Key::Escape);
    settle(&mut harness);
    open_slow(&mut harness, &two);
    assert!(
        harness
            .query_by_label_contains("second partial one")
            .is_some()
    );
    let note = format!("Showing {}. Run scans {}.", one.display(), two.display());
    assert!(harness.query_by_label(&note).is_some());
    click_run(&mut harness, "Run", "second partial two");
    assert!(harness.query_by_label(&note).is_none());

    // The status bar's × stops a running one, without losing the rows.
    harness.key_press(egui::Key::Escape);
    settle(&mut harness);
    click_status_item(&mut harness, "Cancel");
    jobs_end(&mut harness);
    assert_eq!(harness.state().plugins.panels.len(), 1);
    assert!(harness.query_by_label("Slow: results").is_some());

    finish_plugins(harness);
    let _ = std::fs::remove_dir_all(&base);
}

/// Typing a size hides smaller rows at once, without a new scan, and the
/// filter stays for the next run.
#[cfg(unix)]
#[test]
fn results_dialog_filters_by_size() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let (mut harness, base, one, _) = slow_harness("filter");
    open_slow(&mut harness, &one);
    click_run(&mut harness, "Run", "first partial one");
    assert!(harness.query_by_label_contains("other, ").is_some());
    let job = harness.state().plugins.panels[0].job;

    let amount = harness.get_by_label("Hide smaller than");
    amount.click();
    settle(&mut harness);
    let amount = harness.get_by_label("Hide smaller than");
    amount.type_text("1");
    harness.step();
    harness.key_press(egui::Key::Enter);
    settle(&mut harness);
    assert!(
        harness
            .query_by_label_contains("first partial one")
            .is_some()
    );
    assert!(harness.query_by_label_contains("other, ").is_none());
    assert!(harness.query_by_label("Showing 1 of 2").is_some());
    assert_eq!(harness.state().plugins.panels[0].job, job, "no new scan");

    // The next update and the next run keep it.
    std::fs::write(one.join("finish"), "").expect("writes");
    jobs_end(&mut harness);
    assert!(harness.query_by_label_contains("first done one").is_some());
    assert!(harness.query_by_label_contains("other, ").is_none());
    std::fs::remove_file(one.join("finish")).expect("removes");
    click_run(&mut harness, "Run", "first partial one");
    assert!(harness.query_by_label_contains("other, ").is_none());
    harness.get_by_label("Stop").click();
    jobs_end(&mut harness);

    // Back to zero shows everything.
    let amount = harness.get_by_label("Hide smaller than");
    amount.click();
    settle(&mut harness);
    let amount = harness.get_by_label("Hide smaller than");
    amount.type_text("0");
    harness.step();
    harness.key_press(egui::Key::Enter);
    settle(&mut harness);
    assert!(harness.query_by_label_contains("other, ").is_some());

    finish_plugins(harness);
    let _ = std::fs::remove_dir_all(&base);
}

/// Runs the real Folder Usage on `dir`, leaving its results on screen.
#[cfg(unix)]
fn usage_results(dir: &Path, option: &str, row: &str) -> Harness<'static, App> {
    let mut harness = running_plugins(dir);
    harness.state_mut().run_plugin_action(
        "io.github.leinnan.folder-usage.scan",
        crate::plugins::CallContext {
            dir: Some(dir.to_path_buf()),
            ..Default::default()
        },
    );
    settle(&mut harness);
    harness.get_by_label(option).click();
    settle(&mut harness);
    harness.get_by_label("Run").click();
    wait(&mut harness, "the results", |harness| {
        harness.state().plugins.jobs.is_empty() && harness.query_by_label_contains(row).is_some()
    });
    settle(&mut harness);
    harness
}

/// Right-clicks a row and chooses a menu entry.
#[cfg(unix)]
fn row_menu(harness: &mut Harness<'_, App>, row: &str, entry: &str) {
    harness.get_by_label_contains(row).click_secondary();
    settle(harness);
    harness.get_by_label(entry).click();
    settle(harness);
}

#[cfg(unix)]
fn row_text(harness: &Harness<'_, App>, row: &str) -> String {
    // Its label: kittest shows it only through the node's description.
    let node = format!("{:?}", harness.get_by_label_contains(row));
    node.split("label: \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap_or_default()
        .to_owned()
}

/// Waits for a row's text to change, once a removal or a new measure is done.
#[cfg(unix)]
fn row_changes(harness: &mut Harness<'_, App>, row: &str, before: &str) {
    wait(harness, "the row to change", |harness| {
        harness.query_by_label_contains(row).is_some() && row_text(harness, row) != before
    });
}

#[cfg(unix)]
fn back_at_results(harness: &mut Harness<'_, App>) {
    wait(harness, "the results to come back", |harness| {
        harness.state().display_modal == Some(ModalWindow::PluginResults)
    });
    settle(harness);
}

#[cfg(unix)]
fn fixture_project(dir: &Path, name: &str, build: bool) {
    std::fs::create_dir_all(dir.join("src")).expect("creates");
    std::fs::write(
        dir.join("Cargo.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
    )
    .expect("writes");
    std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").expect("writes");
    if build {
        cargo_build(dir);
    }
}

#[cfg(unix)]
fn cargo_build(dir: &Path) {
    let cargo = directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().join(".cargo/bin/cargo"))
        .filter(|path| path.is_file())
        .map_or_else(|| "cargo".into(), PathBuf::from);
    let status = std::process::Command::new(cargo)
        .args(["build", "--offline", "--quiet"])
        .current_dir(dir)
        .status()
        .expect("cargo runs");
    assert!(status.success());
}

#[cfg(unix)]
fn fixture_base(name: &str) -> PathBuf {
    let base = std::env::temp_dir()
        .canonicalize()
        .expect("temp dir")
        .join(format!("dirfleet_plugins_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("creates");
    base
}

/// The row menu through the real app, on real Cargo projects: each entry asks
/// first, says what goes, comes back to the results and updates the row.
#[cfg(unix)]
#[test]
#[allow(clippy::too_many_lines)]
fn row_menu_asks_then_removes_and_updates_the_row() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let base = fixture_base("row_menu");
    let project = base.join("games/menu_project");
    fixture_project(&project, "menu_project", true);
    let neighbour = base.join("games/menu_neighbour");
    fixture_project(&neighbour, "menu_neighbour", false);
    let incremental = project.join("target/debug/incremental");
    let row = "games/menu_project, ";
    let mut harness = usage_results(&base, "Rust projects anywhere below", row);

    // Asking, then Cancel: back at the results, nothing gone.
    row_menu(&mut harness, row, "Clear incremental…");
    wait(&mut harness, "the confirmation", |harness| {
        harness.query_by_label_contains("Delete ").is_some()
    });
    assert_eq!(harness.state().display_modal, Some(ModalWindow::PluginForm));
    settle(&mut harness);
    assert!(
        harness
            .query_by_label_contains(&incremental.display().to_string())
            .is_some(),
        "says which folder goes"
    );
    harness.get_by_label("Cancel").click();
    back_at_results(&mut harness);
    assert!(incremental.is_dir());
    assert!(harness.query_by_label_contains(row).is_some());

    // Esc in the confirmation does the same.
    row_menu(&mut harness, row, "Clear incremental…");
    wait(&mut harness, "the confirmation", |harness| {
        harness.query_by_label_contains("Delete ").is_some()
    });
    settle(&mut harness);
    harness.key_press(egui::Key::Escape);
    back_at_results(&mut harness);
    assert!(incremental.is_dir());

    // Confirming removes only the cache, and the row follows by itself.
    let before = row_text(&harness, row);
    row_menu(&mut harness, row, "Clear incremental…");
    wait(&mut harness, "the confirmation", |harness| {
        harness.query_by_label_contains("Delete ").is_some()
    });
    settle(&mut harness);
    harness.get_by_label_contains("Delete ").click();
    wait(&mut harness, "the cache to go", |_| !incremental.exists());
    back_at_results(&mut harness);
    row_changes(&mut harness, row, &before);
    assert!(project.join("target/debug/deps").is_dir());
    assert!(project.join("src/main.rs").is_file());
    cargo_build(&project);

    // Measure again sees what changed outside the app.
    let before = row_text(&harness, row);
    std::fs::write(project.join("big.bin"), vec![1u8; 8 << 20]).expect("writes");
    for _ in 0..60 {
        harness.step();
    }
    row_menu(&mut harness, row, "Measure again");
    row_changes(&mut harness, row, &before);
    assert_eq!(
        harness.state().display_modal,
        Some(ModalWindow::PluginResults)
    );

    // Clear target, through cargo clean.
    let before = row_text(&harness, row);
    row_menu(&mut harness, row, "Clear target…");
    wait(&mut harness, "the confirmation", |harness| {
        harness.query_by_label_contains("Delete ").is_some()
    });
    settle(&mut harness);
    assert!(
        harness
            .query_by_label_contains(&project.join("target").display().to_string())
            .is_some()
    );
    harness.get_by_label_contains("Delete ").click();
    let gone = project.join("target");
    wait(&mut harness, "target to go", |_| !gone.exists());
    back_at_results(&mut harness);
    row_changes(&mut harness, row, &before);
    assert!(project.join("src/main.rs").is_file());

    // Remove folder: says what goes, Trash chosen unless changed, then gone.
    row_menu(&mut harness, row, "Remove folder…");
    wait(&mut harness, "the confirmation", |harness| {
        harness.query_all_by_label("Move to Trash").next().is_some()
    });
    settle(&mut harness);
    assert!(
        harness
            .query_by_label_contains(&project.display().to_string())
            .is_some()
    );
    harness.get_by_label("Delete permanently").click();
    settle(&mut harness);
    // The radio reads "Delete …" too; the button comes after it.
    harness
        .query_all_by_label_contains("Delete ")
        .last()
        .expect("the button")
        .click();
    wait(&mut harness, "the folder to go", |_| !project.exists());
    back_at_results(&mut harness);
    wait(&mut harness, "the row to go", |harness| {
        harness.query_by_label_contains(row).is_none()
    });
    settle(&mut harness);
    assert!(
        neighbour.join("src/main.rs").is_file(),
        "its neighbour stays"
    );
    assert!(
        harness.query_by_label_contains("Projects: 1 · ").is_some(),
        "the totals follow"
    );

    finish_plugins(harness);
    let _ = std::fs::remove_dir_all(&base);
}

/// The safety checks of Rust Clean hold when reached from a row.
#[cfg(unix)]
#[test]
fn row_menu_keeps_the_rust_clean_checks() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let base = fixture_base("row_checks");
    std::fs::write(
        base.join("Cargo.toml"),
        "[workspace]\nmembers = [\"member\"]\nresolver = \"2\"\n",
    )
    .expect("writes");
    fixture_project(&base.join("member"), "member", false);
    cargo_build(&base);
    // Cargo can't read this one, and its `target` isn't provably Cargo's.
    let broken = base.join("broken");
    std::fs::create_dir_all(broken.join("target")).expect("creates");
    std::fs::write(broken.join("Cargo.toml"), "not toml [").expect("writes");
    std::fs::write(broken.join("target/precious.txt"), "keep").expect("writes");

    let member_row = "member, ";
    let mut harness = usage_results(&base, "Subfolders of this folder", member_row);

    // A member clears the workspace's folder, and is told so.
    row_menu(&mut harness, member_row, "Clear target…");
    wait(&mut harness, "the confirmation", |harness| {
        harness.query_by_label_contains("Delete ").is_some()
    });
    settle(&mut harness);
    assert!(
        harness
            .query_by_label_contains(&base.join("target").display().to_string())
            .is_some(),
        "names the workspace's target"
    );
    assert!(
        harness
            .query_by_label_contains("part of the workspace")
            .is_some()
    );
    harness.get_by_label("Cancel").click();
    back_at_results(&mut harness);
    assert!(base.join("target").is_dir());

    // A folder that isn't provably Cargo's is refused.
    row_menu(&mut harness, "broken, ", "Clear target…");
    wait(&mut harness, "the refusal", |harness| {
        harness
            .query_by_label_contains("doesn't look like")
            .is_some()
    });
    settle(&mut harness);
    assert!(
        harness.query_by_label_contains("Delete ").is_none(),
        "no way to go ahead"
    );
    assert!(harness.state().plugins.jobs.is_empty(), "nothing started");
    assert!(broken.join("target/precious.txt").is_file());
    harness.get_by_label("Close").click();
    back_at_results(&mut harness);

    finish_plugins(harness);
    let _ = std::fs::remove_dir_all(&base);
}

/// While a scan runs its rows can't be acted on; opening them still works.
#[cfg(unix)]
#[test]
fn row_menu_waits_for_the_scan() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let (mut harness, base, one, _) = slow_harness("row_wait");
    open_slow(&mut harness, &one);
    click_run(&mut harness, "Run", "first partial one");

    row_menu(&mut harness, "first partial one", "Do it…");
    assert_eq!(
        harness.state().display_modal,
        Some(ModalWindow::PluginResults),
        "disabled during the scan"
    );
    assert!(harness.state().plugins.form.is_none());
    // Esc closes the menu, and only the next one hides the dialog.
    harness
        .get_by_label_contains("first partial one")
        .click_secondary();
    settle(&mut harness);
    assert!(
        harness.query_by_label("Copy path").is_some(),
        "menu is open"
    );
    harness.key_press(egui::Key::Escape);
    settle(&mut harness);
    assert!(harness.query_by_label("Copy path").is_none(), "menu closed");
    assert_eq!(
        harness.state().display_modal,
        Some(ModalWindow::PluginResults),
        "the dialog stays"
    );
    harness.key_press(egui::Key::Escape);
    settle(&mut harness);
    assert_eq!(harness.state().display_modal, None);
    click_status_item(&mut harness, "Slow: Scanning");

    harness.get_by_label("Stop").click();
    jobs_end(&mut harness);
    row_menu(&mut harness, "first partial one", "Do it…");
    assert_eq!(harness.state().display_modal, Some(ModalWindow::PluginForm));
    assert!(harness.query_by_label("Sure about it").is_some());
    harness.get_by_label("Cancel").click();
    back_at_results(&mut harness);

    // Copy path and Open in new tab, which need no confirmation.
    let row = "first partial one";
    harness.get_by_label_contains(row).click_secondary();
    settle(&mut harness);
    harness.get_by_label("Copy path").click();
    harness.step();
    let copied = harness.output().platform_output.commands.iter().any(|command| {
        matches!(command, egui::OutputCommand::CopyText(text) if text == &one.display().to_string())
    });
    assert!(copied, "the row's path is on the clipboard");
    settle(&mut harness);
    row_menu(&mut harness, row, "Open in new tab");
    assert_eq!(harness.state().display_modal, None);
    assert!(
        harness
            .state()
            .tabs
            .tabs()
            .filter(|tab| tab.current_path.single_path() == Some(one.clone()))
            .count()
            >= 2
    );

    finish_plugins(harness);
    let _ = std::fs::remove_dir_all(&base);
}

/// Starts the slow plugin's confirm action from a row, through its form.
#[cfg(unix)]
fn confirm_row(harness: &mut Harness<'_, App>, row: &str) {
    row_menu(harness, row, "Do it…");
    wait(harness, "the form", |harness| {
        harness.query_by_label("Sure about it").is_some()
    });
    settle(harness);
    harness.get_by_label("Run").click();
    back_at_results(harness);
}

/// What the results show from the moment a removal is confirmed until its
/// new sizes are in.
#[cfg(unix)]
#[test]
fn rows_show_what_is_happening_to_them() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let (mut harness, base, one, _) = slow_harness("row_status");
    let sub = one.join("sub");
    std::fs::create_dir_all(&sub).expect("creates");
    std::fs::write(one.join("finish"), "").expect("writes");
    open_slow(&mut harness, &one);
    harness.get_by_label("Run").click();
    wait(&mut harness, "the results", |harness| {
        harness.state().plugins.jobs.is_empty()
            && harness.query_by_label_contains("first done one").is_some()
    });
    settle(&mut harness);
    let row = "first done one";

    // Running shows at once, and only on that row.
    confirm_row(&mut harness, row);
    wait(&mut harness, "the row to say so", |harness| {
        row_text(harness, row).contains("Doing it…")
    });
    assert!(
        row_text(&harness, row).contains("2.00 GB"),
        "sizes stay for now"
    );
    assert!(!row_text(&harness, "other, ").contains('…'));

    // A second one waits its turn, as the plugin runs one at a time.
    confirm_row(&mut harness, "other, ");
    wait(&mut harness, "the row to wait", |harness| {
        row_text(harness, "other, ").contains("Waiting…")
    });

    // Neither can be started again meanwhile.
    for _ in 0..60 {
        harness.step();
    }
    row_menu(&mut harness, row, "Do it…");
    assert_eq!(
        harness.state().display_modal,
        Some(ModalWindow::PluginResults)
    );
    assert!(harness.state().plugins.form.is_none(), "disabled");

    // Done: on to measuring, and the next one's turn. The plugin's single job
    // slot means the measuring waits for that one here.
    std::fs::write(one.join("release"), "").expect("writes");
    wait(&mut harness, "the second row's turn", |harness| {
        row_text(harness, "other, ").contains("Doing it…")
    });
    assert!(row_text(&harness, row).contains("Measuring…"));

    // Failing says so on the row, and keeps its sizes.
    std::fs::write(sub.join("fail"), "").expect("writes");
    std::fs::write(sub.join("release"), "").expect("writes");
    wait(&mut harness, "the failure", |harness| {
        row_text(harness, "other, ").contains("Failed")
    });
    assert!(row_text(&harness, "other, ").contains("1.00 MB"));

    // With the slot free, the first row gets its new sizes and loses its status.
    wait(&mut harness, "new sizes", |harness| {
        let text = row_text(harness, row);
        text.contains("3.00 KB") && !text.contains('…')
    });
    jobs_end(&mut harness);

    // A failed row can be tried again.
    std::fs::remove_file(sub.join("fail")).expect("removes");
    for _ in 0..60 {
        harness.step();
    }
    confirm_row(&mut harness, "other, ");
    wait(&mut harness, "new sizes", |harness| {
        row_text(harness, "other, ").contains("3.00 KB")
    });
    jobs_end(&mut harness);

    // A file operation still going: the row waits for it, not just the job.
    harness.state_mut().plugins.file_op_requested();
    for _ in 0..60 {
        harness.step();
    }
    let before = row_text(&harness, row);
    confirm_row(&mut harness, row);
    jobs_end(&mut harness);
    assert!(
        row_text(&harness, row).contains("Doing it…"),
        "still busy: {before}"
    );
    harness.state_mut().plugins.file_op_finished();
    wait(&mut harness, "the row to clear", |harness| {
        !row_text(harness, row).contains('…')
    });

    finish_plugins(harness);
    let _ = std::fs::remove_dir_all(&base);
}

/// The dialog during a scan, with a row being cleared and one waiting, at the
/// default width and in a narrow window.
#[cfg(not(windows))]
#[test]
fn results_dialog_scanning_snapshots() {
    use crate::plugins::panel::JobView;
    use crate::plugins::table::RowStatus;
    let _guard = egui_mev_test::snapshot_lock();
    let mut snapshots = egui_kittest::SnapshotResults::new();
    for (name, size) in [
        ("plugin_results_busy", egui::vec2(1100.0, 720.0)),
        ("plugin_results_narrow", egui::vec2(620.0, 520.0)),
    ] {
        let mut panel = results_panel_fixture(Some("Scanning 42 of 136"));
        if let Some(table) = panel.table.as_mut() {
            table.set_status(
                Path::new("/d/slavic"),
                Some(RowStatus::Working("Clearing target…".into())),
            );
            table.set_status(Path::new("/d/carrspg"), Some(RowStatus::Waiting));
            table.set_status(
                Path::new("/d/notes"),
                Some(RowStatus::Failed("boom".into())),
            );
        }
        let job = JobView {
            waiting: false,
            fraction: 0.4,
            label: "IDEAS/slowo".into(),
        };
        let mut harness = Harness::builder()
            .with_options(egui_mev_test::snapshot_options())
            .with_size(size)
            .build_ui(move |ui| {
                let ctx = ui.ctx().clone();
                let id = egui::Id::unique("fonts_set");
                if !ctx.data(|data| data.get_temp::<bool>(id).unwrap_or(false)) {
                    ui::configure_test(&ctx, false);
                    ctx.data_mut(|data| data.insert_temp(id, true));
                    return;
                }
                panel.ui(&ctx, Some(&job));
            });
        harness.run_steps(12);
        snapshots.add(harness.try_snapshot(name));
    }
    drop(snapshots);
}

/// A results dialog with a filled table, for pictures of it.
#[cfg(not(windows))]
fn results_panel_fixture(progress: Option<&str>) -> crate::plugins::panel::ResultsPanel {
    let form: crate::plugins::form::Form = serde_json::from_value(serde_json::json!({
        "fields": [{
            "id": "show", "type": "radio", "label": "Show",
            "options": [["subfolders", "Subfolders of this folder"], ["rust", "Rust projects anywhere below"]],
            "default": "rust",
        }],
    }))
    .expect("form");
    let mut panel = crate::plugins::panel::ResultsPanel {
        plugin: "test.slow".into(),
        action: "scan".into(),
        title: "Folder Usage".into(),
        context: crate::plugins::CallContext {
            dir: Some("/virtual/Documents".into()),
            ..Default::default()
        },
        state: crate::plugins::form::FormState::new(form, None),
        request: None,
        job: None,
        results_dir: Some("/virtual/Documents".into()),
        table: None,
        last_sort: None,
        last_filter: crate::plugins::table::SizeFilter::default(),
    };
    panel.show(
        serde_json::from_value(serde_json::json!({
            "title": "Folder Usage",
            "subtitle": "Projects: 3 · 201.0 GB",
            "progress": progress,
            "columns": [
                { "id": "name", "label": "Folder" },
                { "id": "size", "label": "Size", "kind": "size" },
                { "id": "target", "label": "target", "kind": "size" },
                { "id": "modified", "label": "Last change", "kind": "time" },
            ],
            "rows": [
                { "path": "/d/slavic", "cells": { "name": "GAMES/slavic_castles", "size": 366_000_000_000_i64, "target": 364_000_000_000_i64, "modified": 1_793_700_000 } },
                { "path": "/d/carrspg", "cells": { "name": "IDEAS/carrspg", "size": 220_000_000_000_i64, "target": 58_700_000_000_i64, "modified": 1_793_780_000 } },
                { "path": "/d/notes", "cells": { "name": "NOTES", "size": 19_000_000, "modified": 1_754_000_000 } },
                { "path": "/d/empty", "cells": { "name": "UTILS/rpack" } },
            ],
            "sort": { "column": "size", "descending": true },
        }))
        .expect("spec"),
    );
    panel
}

/// The dialog with its options and results.
#[cfg(not(windows))]
#[test]
fn results_dialog_snapshot() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}
    let panel = results_panel_fixture(None);
    let mut app = fixture(Appearance::Dark, false, false);
    app.plugins.panels.push(panel);
    app.plugins.show_panel("test.slow.scan");
    app.display_modal = Some(ModalWindow::PluginResults);
    let mut harness = Harness::builder()
        .with_options(egui_mev_test::snapshot_options())
        .with_size(egui::vec2(1000.0, 700.0))
        .build_ui_state(draw, app);
    harness.run_steps(12);
    let snapshot = harness.try_snapshot("plugin_results_dialog");
    drop(harness);
    while COMMANDS_QUEUE.pop().is_some() {}
    snapshot.expect("results dialog snapshot");
}

/// The bundled plugins through the real app: a form's button, the job, the
/// file operation worker and the results window. Only touches a temp folder.
#[cfg(unix)]
#[test]
#[allow(clippy::too_many_lines)]
fn folder_usage_and_rust_clean_through_the_app() {
    let _guard = egui_mev_test::snapshot_lock();
    while COMMANDS_QUEUE.pop().is_some() {}

    let base = std::env::temp_dir()
        .canonicalize()
        .expect("temp dir")
        .join(format!("dirfleet_plugins_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let project = base.join("games/e2e_project");
    std::fs::create_dir_all(project.join("src")).expect("creates");
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"e2e_project\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("writes");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("writes");
    let cargo = directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().join(".cargo/bin/cargo"))
        .filter(|path| path.is_file())
        .map_or_else(|| "cargo".into(), PathBuf::from);
    let build = || {
        let status = std::process::Command::new(&cargo)
            .args(["build", "--offline", "--quiet"])
            .current_dir(&project)
            .status()
            .expect("cargo runs");
        assert!(status.success());
    };
    build();
    let incremental = project.join("target/debug/incremental");
    assert!(incremental.is_dir());

    let mut tab = TabData::from_path(&base);
    tab.load = crate::app::dock::LoadState::Idle;
    tab.current_path = CurrentPath::One(base.clone());
    let mut tabs = Workspace::from_tabs(vec![tab]);
    tabs.focused = true;
    let app = App {
        tabs,
        plugins: crate::plugins::PluginHost::bundled_only(),
        ..Default::default()
    };
    let mut harness = plugin_harness(app);
    let running = |harness: &Harness<'_, App>, id: &str| {
        harness.state().plugins.plugins.iter().any(|plugin| {
            plugin.id() == id && matches!(plugin.status, crate::plugins::host::Status::Running)
        })
    };
    wait(&mut harness, "plugins to start", |harness| {
        running(harness, "io.github.leinnan.folder-usage")
            && running(harness, "io.github.leinnan.rust-clean")
    });

    // Folder Usage on the parent folder: the project is one level down.
    harness.state_mut().run_plugin_action(
        "io.github.leinnan.folder-usage.scan",
        crate::plugins::CallContext {
            dir: Some(base.clone()),
            ..Default::default()
        },
    );
    wait(&mut harness, "the scan form", |harness| {
        harness
            .query_by_label("Rust projects anywhere below")
            .is_some()
    });
    // A modal moves while its first frames lay it out.
    for _ in 0..6 {
        harness.step();
    }
    harness.get_by_label("Rust projects anywhere below").click();
    for _ in 0..4 {
        harness.step();
    }
    harness.get_by_label("Run").click();
    // The bug this guards against: with no other dialog open, the results
    // must be on screen as soon as the job has sent them, and stay there.
    wait(&mut harness, "the results", |harness| {
        harness.state().plugins.jobs.is_empty()
            && harness
                .query_by_label_contains("games/e2e_project, ")
                .is_some()
    });
    for _ in 0..8 {
        harness.step();
    }
    assert!(
        harness
            .query_by_label_contains("games/e2e_project")
            .is_some()
    );
    harness.get_by_label("Last change").click();
    harness.step();
    harness.get_by_label("target").click();
    harness.step();
    // Clicks soon after the headers' would count as a triple click.
    for _ in 0..60 {
        harness.step();
    }
    let row = harness.get_by_label_contains("games/e2e_project");
    row.click();
    harness.step();
    let row = harness.get_by_label_contains("games/e2e_project");
    row.click();
    harness.step();
    let wanted = project.clone();
    for _ in 0..6 {
        harness.step();
    }
    let tabs: Vec<_> = harness
        .state()
        .tabs
        .tabs()
        .map(|tab| tab.current_path.single_path())
        .collect();
    assert!(
        tabs.contains(&Some(wanted)),
        "double-click opens the project: {tabs:?}"
    );

    // Clear incremental: only the cache goes, and the project still builds.
    assert_eq!(
        harness.state().display_modal,
        None,
        "opening a row hides the dialog"
    );
    harness.state_mut().run_plugin_action(
        "io.github.leinnan.rust-clean.clear_incremental",
        crate::plugins::CallContext {
            dir: Some(project.clone()),
            ..Default::default()
        },
    );
    wait(&mut harness, "the incremental form", |harness| {
        harness.query_by_label_contains("Delete ").is_some()
    });
    for _ in 0..6 {
        harness.step();
    }
    harness.get_by_label_contains("Delete ").click();
    let path = incremental.clone();
    wait(&mut harness, "the cache to go", |_| !path.exists());
    assert!(project.join("target/debug/deps").is_dir());
    assert!(project.join("src/main.rs").is_file());
    build();
    assert!(incremental.is_dir());

    // Clear target: cargo clean through the job.
    wait(&mut harness, "the first job to end", |harness| {
        harness.state().plugins.jobs.is_empty()
    });
    harness.state_mut().run_plugin_action(
        "io.github.leinnan.rust-clean.clear_target",
        crate::plugins::CallContext {
            dir: Some(project.clone()),
            ..Default::default()
        },
    );
    wait(&mut harness, "the target form", |harness| {
        harness.query_by_label_contains("Delete ").is_some()
    });
    for _ in 0..6 {
        harness.step();
    }
    harness.get_by_label_contains("Delete ").click();
    let path = project.join("target");
    wait(&mut harness, "target to go", |_| !path.exists());
    assert!(project.join("src/main.rs").is_file());
    assert!(project.join("Cargo.toml").is_file());

    finish_plugins(harness);
    let _ = std::fs::remove_dir_all(&base);
}
