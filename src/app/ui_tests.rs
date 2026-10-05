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
        #[cfg(not(target_os = "macos"))]
        drives_locations: Locations::default(),
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
            ctx.data_set_path(
                &tab.current_path,
                Selected {
                    selected_fields: ctx
                        .data(|data| {
                            data.get_temp::<Vec<usize>>(egui::Id::new("initial_selection"))
                        })
                        .unwrap_or_else(|| vec![1]),
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
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
        harness.run_steps(2);
    }
    // Add a second dock pane and render with the changed text cache and theme.
    let mut second = TabData::from_path(Path::new("/virtual/Downloads"));
    second.current_path = CurrentPath::One("/virtual/Downloads".into());
    second.list = vec![DirEntry::test_new("/virtual/Downloads/Download.txt")];
    second.visible_entries = vec![0];
    second.loading = false;
    let nodes = harness
        .state_mut()
        .tabs
        .dock_state
        .main_surface_mut()
        .split_right(egui_dock::NodeIndex::root(), 0.5, vec![second]);
    harness
        .state_mut()
        .tabs
        .dock_state
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
    harness.state_mut().display_modal = Some(ModalWindow::Rename);
    ctx.data_mut(|data| {
        data.insert_temp(
            egui::Id::new(ModalWindow::Rename),
            DirEntry::test_new("/virtual/Download.txt"),
        )
    });
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
        ..Default::default()
    });
    let mut harness = Harness::builder()
        .with_options(crate::app::ui::snapshot_options())
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw, app);
    harness.run_steps(12);
    harness.get_by_label("Options •").click();
    harness.run_steps(4);
    snapshots.add(harness.try_snapshot("dirfleet_search_options"));
    let mut app = fixture(Appearance::Light, false, false);
    app.display_modal = Some(ModalWindow::Settings);
    let mut harness = Harness::builder()
        .with_options(crate::app::ui::snapshot_options())
        .with_size(egui::vec2(1100.0, 720.0))
        .build_ui_state(draw, app);
    harness.run_steps(12);
    snapshots.add(harness.try_snapshot("dirfleet_settings_light"));
    harness.get_by_label("Appearance").click();
    harness.run_steps(4);
    snapshots.add(harness.try_snapshot("dirfleet_settings_appearance"));
    harness.get_by_label("Files").click();
    harness.run_steps(4);
    snapshots.add(harness.try_snapshot("dirfleet_settings_files"));
    let mut app = fixture(Appearance::Dark, false, false);
    app.inspector_overlay = true;
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
    assert!(app.sidebar_overlay && app.inspector_overlay);
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
    second.list = vec![DirEntry::test_new(
        "/virtual/Downloads/A long downloaded file name.txt",
    )];
    second.visible_entries = vec![0];
    second.loading = false;
    let nodes = app.tabs.dock_state.main_surface_mut().split_right(
        egui_dock::NodeIndex::root(),
        0.5,
        vec![second],
    );
    app.tabs
        .dock_state
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
                        0 => assets::HoverPreview::Pending,
                        1 => assets::HoverPreview::Unavailable {
                            reason: "Decoder unavailable".into(),
                            retry_after: None,
                        },
                        _ => assets::HoverPreview::Ready(ui.ctx().load_texture(
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
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
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
    assert!(ctx.data_get_tab::<DirectoryPathInfo>(id).is_none());
    harness
        .state_mut()
        .handle_action(&ctx, ActionToPerform::ToggleSidebar);
    harness.run_steps(12);
    assert!(harness.state().sidebar_overlay);
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    assert!(!harness.state().sidebar_overlay);
    assert!(harness.state().settings.sidebar_visible);
}
