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
        .data(|data| data.get_temp::<bool>(egui::Id::new("fixture_initialized")))
        .unwrap_or(false);
    if !initialized {
        let mut fonts = egui::FontDefinitions::default();
        ui::register_icons(&mut fonts);
        ctx.set_fonts(fonts);
        ui::configure_test(
            ctx,
            ctx.data(|data| data.get_temp::<bool>(egui::Id::new("fluent_fixture")))
                .unwrap_or(false),
        );
        if let Some(tab) = app.tabs.get_current_tab() {
            tab.selection.set_rows(
                ctx.data(|data| data.get_temp::<Vec<usize>>(egui::Id::new("initial_selection")))
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
        ctx.data_mut(|data| data.insert_temp(egui::Id::new("fixture_initialized"), true));
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
        .data_mut(|data| data.insert_temp(egui::Id::new("fluent_fixture"), true));
    draw(root, app);
}

#[cfg(windows)]
#[test]
fn fluent_windows_grids_compact_settings_and_appearance_switching() {
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut snapshots = egui_kittest::SnapshotResults::new();
    for (suffix, appearance) in [("dark", Appearance::Dark), ("light", Appearance::Light)] {
        for (layout, size, grid) in [
            ("full", egui::vec2(1100.0, 720.0), false),
            ("grid", egui::vec2(1100.0, 720.0), true),
            ("compact", egui::vec2(640.0, 420.0), false),
        ] {
            let mut harness = Harness::builder()
                .with_options(ui::snapshot_options())
                .with_size(size)
                .build_ui_state(draw_fluent, fixture(appearance, grid, true));
            harness.run_steps(12);
            snapshots.add(harness.try_snapshot(format!("fluent_{layout}_{suffix}")));
        }
        let mut app = fixture(appearance, false, false);
        app.display_modal = Some(ModalWindow::Settings);
        let mut harness = Harness::builder()
            .with_options(ui::snapshot_options())
            .with_size(egui::vec2(1100.0, 720.0))
            .build_ui_state(draw_fluent, app);
        harness.run_steps(12);
        snapshots.add(harness.try_snapshot(format!("fluent_settings_{suffix}")));
        harness.get_by_label("Appearance").click();
        harness.run_steps(4);
        snapshots.add(harness.try_snapshot(format!("fluent_settings_appearance_{suffix}")));
    }
    let mut harness = Harness::builder()
        .with_options(ui::snapshot_options())
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
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while COMMANDS_QUEUE.pop().is_some() {}
    let mut harness = Harness::builder()
        .with_options(ui::snapshot_options())
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
            .with_options(crate::app::ui::snapshot_options())
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
        chips: crate::app::search::query::parse("ext:pdf -draft").chips,
        ..Default::default()
    });
    let mut harness = Harness::builder()
        .with_options(crate::app::ui::snapshot_options())
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
        .with_options(crate::app::ui::snapshot_options())
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
        .with_options(crate::app::ui::snapshot_options())
        .with_size(egui::vec2(640.0, 420.0))
        .build_ui_state(draw, app);
    harness.run_steps(12);
    snapshots.add(harness.try_snapshot("dirfleet_settings_compact"));
    let mut app = fixture(Appearance::Dark, false, false);
    app.tabs.window_mut().inspector_overlay = true;
    let mut harness = Harness::builder()
        .with_options(crate::app::ui::snapshot_options())
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
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut snapshots = egui_kittest::SnapshotResults::new();
    for (name, indices) in [
        ("dirfleet_inspector_empty", Vec::<usize>::new()),
        ("dirfleet_inspector_multiple", vec![1usize, 2]),
        ("dirfleet_inspector_folder", vec![0usize]),
    ] {
        let mut harness = Harness::builder()
            .with_options(crate::app::ui::snapshot_options())
            .with_size(egui::vec2(1100.0, 720.0))
            .build_ui_state(
                move |root, app| {
                    root.ctx().data_mut(|data| {
                        data.insert_temp(egui::Id::new("initial_selection"), indices.clone())
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
        .with_options(crate::app::ui::snapshot_options())
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw, app);
    harness.run_steps(12);
    snapshots.add(harness.try_snapshot("dirfleet_split_panes"));
    snapshots.unwrap();
}

#[test]
fn inspector_preview_loading_ready_and_failure() {
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut snapshots = egui_kittest::SnapshotResults::new();
    for (name, state) in [
        ("dirfleet_preview_loading", 0),
        ("dirfleet_preview_failure", 1),
        ("dirfleet_preview_ready", 2),
    ] {
        let mut assets = assets::AssetManager::default();
        let mut harness = Harness::builder()
            .with_options(crate::app::ui::snapshot_options())
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
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut harness = Harness::builder()
        .with_options(crate::app::ui::snapshot_options())
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw, fixture(Appearance::Dark, false, true));
    harness.run_steps(12);
    harness.state_mut().settings.appearance = Appearance::Light;
    harness.run_steps(12);
    harness.snapshot("dirfleet_appearance_changed_to_light");
}

#[test]
fn keyboard_selection_and_search_follow_the_active_pane() {
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut harness = Harness::builder()
        .with_options(crate::app::ui::snapshot_options())
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
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while COMMANDS_QUEUE.pop().is_some() {}
    let mut app = fixture(Appearance::Dark, false, false);
    app.display_modal = Some(ModalWindow::Commands);
    app.command_palette.commands = vec![
        ActionToPerform::ToggleSidebar.into(),
        ActionToPerform::ToggleInspector.into(),
    ];
    let mut harness = Harness::builder()
        .with_options(crate::app::ui::snapshot_options())
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw, app);
    harness.run_steps(12);
    while COMMANDS_QUEUE.pop().is_some() {}
    let snapshot = harness.try_snapshot("dirfleet_commands");
    harness.key_press(egui::Key::ArrowDown);
    harness.run_steps(2);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(2);
    let action = COMMANDS_QUEUE.pop();
    assert!(
        matches!(action, Some(ActionToPerform::ToggleInspector)),
        "received {action:?}"
    );
    assert!(matches!(
        COMMANDS_QUEUE.pop(),
        Some(ActionToPerform::CloseActiveModalWindow)
    ));
    snapshot.expect("command palette snapshot");
}

/// A second window draws, gets the commands scheduled from it, and closes
/// with its tabs. (Tests draw one window per frame: embedded viewports
/// would share the main window's panel ids.)
#[test]
fn second_window_draws_takes_its_commands_and_closes() {
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while COMMANDS_QUEUE.pop().is_some() {}
    let shown = egui::Id::new("shown_window");
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
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut harness = Harness::builder()
        .with_options(crate::app::ui::snapshot_options())
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
        Some(egui::Id::new(("path_editor", id)))
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

#[test]
fn keyboard_selects_and_renames_without_hovering_the_list() {
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while COMMANDS_QUEUE.pop().is_some() {}
    let mut app = fixture(Appearance::Dark, false, false);
    app.display_modal = Some(ModalWindow::Settings);
    let mut harness = Harness::builder()
        .with_options(ui::snapshot_options())
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
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while COMMANDS_QUEUE.pop().is_some() {}
    let mut harness = Harness::builder()
        .with_options(ui::snapshot_options())
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
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        .with_options(ui::snapshot_options())
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
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while COMMANDS_QUEUE.pop().is_some() {}
    let mut snapshots = egui_kittest::SnapshotResults::new();
    let mut harness = Harness::builder()
        .with_options(crate::app::ui::snapshot_options())
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
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
            let p = ui::Palette::for_context(&ctx, dark);
            let at = format!("fluent {fluent}, dark {dark}");
            assert!(contrast(p.text, p.toolbar) >= 4.5, "text ({at})");
            assert!(
                contrast(p.secondary, p.toolbar) >= 4.5,
                "secondary text ({at})"
            );
            assert!(
                contrast(p.secondary, p.card) >= 4.5,
                "secondary on fields ({at})"
            );
            assert!(contrast(p.accent, p.toolbar) >= 3.0, "focus ring ({at})");
        }
    }
}

#[test]
fn toolbar_search_field_opens_takes_typing_and_closes_with_escape() {
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    let _guard = dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
