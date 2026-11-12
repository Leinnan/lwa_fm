use super::*;
use crate::listing::{DirEntry, TimestampSeconds};
use crux_core::Request;
use std::path::{Path, PathBuf};

fn entry(name: &str, is_dir: bool) -> DirEntry {
    let mut entry = DirEntry::test_new(&format!("/x/{name}"));
    if is_dir {
        entry.meta.entry_type = EntryType::Directory;
    } else {
        entry.meta.size = 10;
    }
    entry
}

/// A listing shown in the order given, as if sorted with `view`.
fn listing(entries: &[DirEntry], view: ListView) -> ListDirOutput {
    let order = (0..entries.len() as u32).collect();
    ListDirOutput::Listed {
        listing: Arc::new(Listing::from_entries(entries)),
        order,
        view,
        opened: Arc::default(),
    }
}

/// Opens a window without a folder and returns the number of its tab.
fn window(app: &DirFleet, model: &mut Model) -> u32 {
    let _ = app.update(Event::NewWindow { path: None }, model);
    let view = app.view(model);
    view.windows.last().expect("a window").active.id
}

/// The first window's visible tab.
fn shown(app: &DirFleet, model: &Model) -> TabViewModel {
    app.view(model).windows[0].active.clone()
}

/// Navigates `tab` to `path` and answers its listing with `entries`.
fn open(app: &DirFleet, model: &mut Model, tab: u32, path: &str, entries: &[DirEntry]) {
    let mut cmd = app.update(
        Event::Navigate {
            tab,
            path: path.into(),
        },
        model,
    );
    let mut list = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::ListDir(request) => Some(request),
            _ => None,
        })
        .expect("a listing is requested");
    assert_eq!(list.operation.path, path);
    let view = list.operation.view.clone();
    list.resolve(listing(entries, view))
        .expect("listing resolves");
    let event = cmd.events().next().expect("the listing comes back");
    let _ = app.update(event, model);
}

#[test]
fn a_new_window_asks_the_shell_to_show_it() {
    let app = DirFleet;
    let mut model = Model::default();
    let mut cmd = app.update(
        Event::NewWindow {
            path: Some("/a".into()),
        },
        &mut model,
    );
    let effects: Vec<_> = cmd.effects().collect();
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::OpenWindow(r) if r.operation.window == 1))
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::ListDir(r) if r.operation.path == "/a"))
    );
    let view = app.view(&model);
    assert_eq!(view.windows.len(), 1);
    assert_eq!(view.windows[0].tabs.len(), 1);
    assert_eq!(view.windows[0].active.path, "/a");
}

#[test]
fn navigating_lists_and_watches_the_folder() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let mut cmd = app.update(
        Event::Navigate {
            tab,
            path: "/a".into(),
        },
        &mut model,
    );
    let effects: Vec<_> = cmd.effects().collect();
    assert!(matches!(effects[0], Effect::Render(_)));
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::ListDir(r) if r.operation.path == "/a"))
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::Watch(r) if r.operation.path == "/a"))
    );
    assert!(shown(&app, &model).loading);
}

#[test]
fn a_listing_shows_its_rows() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/a",
        &[entry("sub", true), entry("x.txt", false)],
    );
    let view = shown(&app, &model);
    assert!(!view.loading);
    assert_eq!(view.title, "a");
    assert_eq!(view.total_rows, 2);
    assert_eq!(view.rows[0].name, "sub");
    assert_eq!(view.rows[1].cells[1], "10 B", "Modified, then Size");
}

#[test]
fn a_folder_that_cannot_be_read_shows_why() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let mut cmd = app.update(
        Event::Navigate {
            tab,
            path: "/locked".into(),
        },
        &mut model,
    );
    let mut list = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::ListDir(request) => Some(request),
            _ => None,
        })
        .expect("a listing is requested");
    list.resolve(ListDirOutput::Failed("/locked: permission denied".into()))
        .expect("listing resolves");
    let event = cmd.events().next().expect("the failure comes back");
    let _ = app.update(event, &mut model);
    let view = shown(&app, &model);
    assert_eq!(view.error.as_deref(), Some("/locked: permission denied"));
    assert!(!view.loading);
}

#[test]
fn a_late_listing_of_the_previous_folder_is_dropped() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let mut first = app.update(
        Event::Navigate {
            tab,
            path: "/old".into(),
        },
        &mut model,
    );
    let _ = app.update(
        Event::Navigate {
            tab,
            path: "/new".into(),
        },
        &mut model,
    );
    let mut list = first
        .effects()
        .find_map(|effect| match effect {
            Effect::ListDir(request) => Some(request),
            _ => None,
        })
        .expect("listing of /old");
    list.resolve(listing(&[entry("stale", false)], ListView::default()))
        .expect("listing resolves");
    let event = first.events().next().expect("the stale listing comes back");
    let mut cmd = app.update(event, &mut model);
    assert!(cmd.effects().next().is_none(), "nothing is redrawn");
    assert_eq!(shown(&app, &model).total_rows, 0);
    assert!(shown(&app, &model).loading, "still waiting for /new");
}

#[test]
fn opening_a_folder_row_navigates_into_it_and_back_returns() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/a",
        &[entry("sub", true), entry("x.txt", false)],
    );

    let mut cmd = app.update(Event::OpenRow { tab, row: 1 }, &mut model);
    assert!(
        !cmd.events().any(|e| matches!(e, Event::Navigate { .. })),
        "a file is not navigated into"
    );

    let mut cmd = app.update(Event::OpenRow { tab, row: 0 }, &mut model);
    let event = cmd.events().next().expect("navigates");
    assert_eq!(
        event,
        Event::Navigate {
            tab,
            path: "/a/sub".into()
        }
    );
    let mut cmd = app.update(event, &mut model);
    assert!(
        cmd.effects()
            .any(|e| matches!(e, Effect::Unwatch(r) if r.operation.id == 1)),
        "the previous folder's watch is stopped"
    );
    assert!(shown(&app, &model).nav.back);

    let mut cmd = app.update(Event::Back { tab }, &mut model);
    assert!(
        cmd.effects()
            .any(|e| matches!(e, Effect::ListDir(r) if r.operation.path == "/a"))
    );
    assert!(!shown(&app, &model).nav.back);
}

#[test]
fn a_change_in_the_folder_lists_it_again() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/a", &[entry("one", false)]);
    let generation = model.generation;
    let mut cmd = app.update(
        Event::Watched {
            tab,
            generation,
            event: WatchEvent::Changed,
        },
        &mut model,
    );
    assert_eq!(cmd.events().next(), Some(Event::Refresh { tab }));
    let mut cmd = app.update(Event::Refresh { tab }, &mut model);
    let mut list = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::ListDir(request) => Some(request),
            _ => None,
        })
        .expect("the folder is read again");
    assert_eq!(list.operation.path, "/a");

    // More changes during that read don't start another, which would throw
    // this one away: one more read follows it.
    let reading = model.generation;
    for _ in 0..3 {
        let mut during = app.update(
            Event::Watched {
                tab,
                generation,
                event: WatchEvent::Changed,
            },
            &mut model,
        );
        assert_eq!(during.events().next(), None);
    }
    assert_eq!(model.generation, reading);
    let view = list.operation.view.clone();
    list.resolve(listing(&[entry("one", false), entry("two", false)], view))
        .expect("listing resolves");
    let event = cmd.events().next().expect("the listing comes back");
    let mut listed = app.update(event, &mut model);
    assert_eq!(shown(&app, &model).total_rows, 2, "the read shows");
    assert_eq!(listed.events().next(), Some(Event::Refresh { tab }));
}

#[test]
fn a_ready_watch_lists_the_folder_again() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/a", &[entry("one", false)]);
    let generation = model.generation;
    let mut cmd = app.update(
        Event::Watched {
            tab,
            generation,
            event: WatchEvent::Ready,
        },
        &mut model,
    );
    assert_eq!(
        cmd.events().next(),
        Some(Event::Refresh { tab }),
        "changes made before the watcher started are picked up"
    );
}

#[test]
fn a_watch_ready_during_the_first_read_keeps_that_read_and_reads_again_after_it() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let mut cmd = app.update(
        Event::Navigate {
            tab,
            path: "/a".into(),
        },
        &mut model,
    );
    let mut list = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::ListDir(request) => Some(request),
            _ => None,
        })
        .expect("a listing is requested");
    let generation = model.generation;
    let mut ready = app.update(
        Event::Watched {
            tab,
            generation,
            event: WatchEvent::Ready,
        },
        &mut model,
    );
    assert_eq!(
        ready.events().next(),
        None,
        "the read under way isn't thrown away"
    );
    assert_eq!(model.generation, generation);

    let view = list.operation.view.clone();
    list.resolve(listing(&[entry("one", false)], view))
        .expect("listing resolves");
    let event = cmd.events().next().expect("the listing comes back");
    let mut listed = app.update(event, &mut model);
    assert_eq!(shown(&app, &model).total_rows, 1, "its rows show at once");
    assert_eq!(
        listed.events().next(),
        Some(Event::Refresh { tab }),
        "then the folder is read again, for changes made before the watcher started"
    );
    // That second read asks for no third one.
    let mut again = app.update(Event::Refresh { tab }, &mut model);
    let mut list = again
        .effects()
        .find_map(|effect| match effect {
            Effect::ListDir(request) => Some(request),
            _ => None,
        })
        .expect("the folder is read again");
    let view = list.operation.view.clone();
    list.resolve(listing(&[entry("one", false)], view))
        .expect("listing resolves");
    let event = again
        .events()
        .next()
        .expect("the second listing comes back");
    let mut second = app.update(event, &mut model);
    assert_eq!(second.events().next(), None);
}

#[test]
fn a_folder_that_cannot_be_watched_stays_listed_and_warns_once() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/a", &[entry("x", false)]);
    let generation = model.generation;
    let fail = |model: &mut Model| {
        app.update(
            Event::Watched {
                tab,
                generation,
                event: WatchEvent::Failed("too many open files".into()),
            },
            model,
        )
    };
    let mut cmd = fail(&mut model);
    let view = app.view(&model);
    assert_eq!(
        view.toasts
            .iter()
            .map(|toast| (toast.kind, toast.text.as_str()))
            .collect::<Vec<_>>(),
        [(ToastKind::Warning, "Live updates unavailable")]
    );
    assert!(cmd.effects().any(|e| matches!(e, Effect::Delay(_))));
    let shown = shown(&app, &model);
    assert_eq!(shown.error, None, "the listing isn't replaced by an error");
    assert_eq!(shown.rows.len(), 1);
    let _ = fail(&mut model);
    assert_eq!(app.view(&model).toasts.len(), 1, "a repeat isn't stacked");
}

#[test]
fn the_view_holds_only_the_rows_near_the_viewport() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let entries: Vec<_> = (0..100_000)
        .map(|i| entry(&format!("f{i:06}"), false))
        .collect();
    open(&app, &mut model, tab, "/big", &entries);
    let _ = app.update(
        Event::ViewportChanged {
            tab,
            first_row: 50_000,
            rows: 40,
        },
        &mut model,
    );
    // The visible rows, a screen before and after, and half a screen for
    // the steps the window moves in.
    let view = shown(&app, &model);
    assert_eq!(view.total_rows, 100_000);
    assert_eq!(view.first_row, 50_000 - 40);
    assert_eq!(view.rows.len(), 3 * 40 + 20);
    assert_eq!(view.rows[0].name, format!("f{:06}", 50_000 - 40));
    // Scrolling less than half a screen needs no new view.
    let mut cmd = app.update(
        Event::ViewportChanged {
            tab,
            first_row: 50_015,
            rows: 40,
        },
        &mut model,
    );
    assert!(
        !cmd.effects()
            .any(|effect| matches!(effect, Effect::Render(_)))
    );
    assert_eq!(shown(&app, &model).first_row, 50_000 - 40);
    let mut cmd = app.update(
        Event::ViewportChanged {
            tab,
            first_row: 50_020,
            rows: 40,
        },
        &mut model,
    );
    assert!(
        cmd.effects()
            .any(|effect| matches!(effect, Effect::Render(_)))
    );
    assert_eq!(shown(&app, &model).first_row, 50_020 - 40);
}

#[test]
fn windows_show_their_own_folders() {
    let app = DirFleet;
    let mut model = Model::default();
    let first = window(&app, &mut model);
    let second = window(&app, &mut model);
    assert_ne!(first, second);
    open(&app, &mut model, first, "/a", &[entry("in_a", false)]);
    open(&app, &mut model, second, "/b", &[entry("in_b", false)]);
    let view = app.view(&model);
    assert_eq!(view.windows[0].active.rows[0].name, "in_a");
    assert_eq!(view.windows[1].active.rows[0].name, "in_b");
    assert_eq!(view.window(2).map(|w| w.active.id), Some(second));
}

#[test]
fn closing_a_window_stops_its_watch_and_drops_its_tabs() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let _ = window(&app, &mut model);
    open(&app, &mut model, tab, "/a", &[]);
    let watch = model.generation;

    let mut cmd = app.update(Event::WindowClosed { window: 1 }, &mut model);
    assert!(
        cmd.effects()
            .any(|e| matches!(e, Effect::Unwatch(r) if r.operation.id == watch))
    );
    let view = app.view(&model);
    assert_eq!(view.windows.len(), 1);
    assert_eq!(view.windows[0].id, 2);

    let mut cmd = app.update(
        Event::Navigate {
            tab,
            path: "/b".into(),
        },
        &mut model,
    );
    assert!(
        cmd.effects().next().is_none(),
        "a closed window's tab is gone"
    );
}

#[test]
fn opening_a_folder_asks_the_shell_and_shows_the_answer() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let mut cmd = app.update(Event::OpenFolder { tab }, &mut model);
    let mut pick = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::PickFolder(request) => Some(request),
            _ => None,
        })
        .expect("a folder is asked for");
    pick.resolve(PickFolderOutput::Picked("/chosen".into()))
        .expect("the pick resolves");
    let picked = cmd.events().next().expect("the answer comes back");
    let mut navigate = app.update(picked, &mut model);
    assert_eq!(
        navigate.events().next(),
        Some(Event::Navigate {
            tab,
            path: "/chosen".into()
        })
    );
}

#[test]
fn a_cancelled_pick_changes_nothing() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let mut cmd = app.update(
        Event::FolderPicked {
            tab,
            output: PickFolderOutput::Cancelled,
        },
        &mut model,
    );
    assert!(cmd.effects().next().is_none());
    assert!(cmd.events().next().is_none());
}

/// Starts the app and answers its load with `loaded`; returns the effects.
fn start(
    app: &DirFleet,
    model: &mut Model,
    open: Option<&str>,
    loaded: LoadedState,
) -> Vec<Effect> {
    let mut cmd = app.update(
        Event::Started {
            open: open.map(Into::into),
            home: "/home".into(),
        },
        model,
    );
    let mut load = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::LoadState(request) => Some(request),
            _ => None,
        })
        .expect("the saved state is read");
    load.resolve(loaded).expect("load resolves");
    let event = cmd.events().next().expect("the saved state comes back");
    app.update(event, model).effects().collect()
}

fn saved_windows(effects: &[Effect]) -> Option<Vec<PersistedWindow>> {
    effects.iter().find_map(|effect| match effect {
        Effect::SaveState(request) => Some(request.operation.state.windows.clone()),
        _ => None,
    })
}

fn state(windows: Vec<(Vec<&str>, u32)>) -> LoadedState {
    LoadedState::Found(PersistedState {
        version: STATE_VERSION,
        windows: windows
            .into_iter()
            .map(|(tabs, active)| PersistedWindow {
                tabs: tabs.into_iter().map(Into::into).collect(),
                active,
                ..PersistedWindow::default()
            })
            .collect(),
        ..PersistedState::default()
    })
}

#[test]
fn a_first_start_opens_home_and_saves_it() {
    let app = DirFleet;
    let mut model = Model::default();
    let effects = start(&app, &mut model, None, LoadedState::Missing);
    assert!(effects.iter().any(|e| matches!(e, Effect::OpenWindow(_))));
    assert_eq!(shown(&app, &model).path, "/home");
    assert_eq!(
        saved_windows(&effects),
        Some(vec![PersistedWindow {
            tabs: vec!["/home".into()],
            active: 0,
            ..PersistedWindow::default()
        }])
    );
}

#[test]
fn saved_windows_come_back_with_only_their_visible_tabs_listed() {
    let app = DirFleet;
    let mut model = Model::default();
    let effects = start(
        &app,
        &mut model,
        None,
        state(vec![(vec!["/a", "/b"], 1), (vec!["/c"], 0)]),
    );
    let opened = effects
        .iter()
        .filter(|e| matches!(e, Effect::OpenWindow(_)))
        .count();
    assert_eq!(opened, 2);
    let listed: Vec<_> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::ListDir(r) => Some(r.operation.path.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(listed, ["/b", "/c"], "hidden tabs are not read");
    let view = app.view(&model);
    assert_eq!(view.windows[0].tabs.len(), 2);
    assert_eq!(view.windows[0].active.path, "/b");
    assert_eq!(
        saved_windows(&effects),
        None,
        "restoring changes nothing worth saving"
    );
    let mut cmd = app.update(Event::ClearOpenHistory, &mut model);
    assert_eq!(
        cmd.events()
            .filter(|event| matches!(event, Event::Refresh { .. }))
            .count(),
        2,
        "clearing the open history doesn't read hidden tabs either"
    );
}

#[test]
fn a_folder_the_app_was_started_with_replaces_the_saved_windows() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = start(
        &app,
        &mut model,
        Some("/given"),
        state(vec![(vec!["/a"], 0)]),
    );
    let view = app.view(&model);
    assert_eq!(view.windows.len(), 1);
    assert_eq!(view.windows[0].active.path, "/given");
}

#[test]
fn a_broken_saved_state_falls_back_to_home() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = start(&app, &mut model, None, LoadedState::Failed("bad".into()));
    assert_eq!(shown(&app, &model).path, "/home");
}

#[test]
fn navigating_saves_the_new_folder_and_scrolling_saves_nothing() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = start(&app, &mut model, None, LoadedState::Missing);
    let tab = shown(&app, &model).id;
    let effects: Vec<_> = app
        .update(
            Event::Navigate {
                tab,
                path: "/b".into(),
            },
            &mut model,
        )
        .effects()
        .collect();
    assert_eq!(
        saved_windows(&effects).map(|w| w[0].tabs.clone()),
        Some(vec!["/b".into()])
    );
    let effects: Vec<_> = app
        .update(
            Event::ViewportChanged {
                tab,
                first_row: 5,
                rows: 10,
            },
            &mut model,
        )
        .effects()
        .collect();
    assert_eq!(saved_windows(&effects), None);
}

#[test]
fn nothing_is_saved_before_the_saved_state_is_read() {
    let app = DirFleet;
    let mut model = Model::default();
    let effects: Vec<_> = app
        .update(
            Event::NewWindow {
                path: Some("/a".into()),
            },
            &mut model,
        )
        .effects()
        .collect();
    assert_eq!(saved_windows(&effects), None);
}

#[test]
fn windows_closed_while_quitting_stay_saved() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = start(&app, &mut model, None, LoadedState::Missing);
    let _ = app.update(Event::Quitting, &mut model);
    let effects: Vec<_> = app
        .update(Event::WindowClosed { window: 1 }, &mut model)
        .effects()
        .collect();
    assert_eq!(saved_windows(&effects), None);
    assert!(
        effects.iter().any(|e| matches!(e, Effect::Unwatch(_))),
        "the closed window still stops watching"
    );
}

#[test]
fn closing_a_window_normally_forgets_it() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = start(
        &app,
        &mut model,
        None,
        state(vec![(vec!["/a"], 0), (vec!["/b"], 0)]),
    );
    let effects: Vec<_> = app
        .update(Event::WindowClosed { window: 1 }, &mut model)
        .effects()
        .collect();
    assert_eq!(
        saved_windows(&effects).map(|w| w.len()),
        Some(1),
        "only the open window is saved"
    );
}

/// The `OrderListing` requests in `cmd`.
fn reorders(cmd: &mut Command<Effect, Event>) -> Vec<Request<OrderListing>> {
    cmd.effects()
        .filter_map(|effect| match effect {
            Effect::OrderListing(request) => Some(request),
            _ => None,
        })
        .collect()
}

#[test]
fn sorting_reorders_the_listing_without_reading_it_again() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/a",
        &[entry("x", false), entry("y", false)],
    );
    let mut cmd = app.update(
        Event::SortBy {
            tab,
            sort: Sort::Size,
        },
        &mut model,
    );
    let mut requests = reorders(&mut cmd);
    assert_eq!(requests.len(), 1);
    assert!(
        !cmd.effects().any(|e| matches!(e, Effect::ListDir(_))),
        "the folder is not read again"
    );
    let request = &mut requests[0];
    assert_eq!(request.operation.view.sort, Sort::Size);
    let listing = Arc::clone(&request.operation.listing);
    let view = request.operation.view.clone();
    request
        .resolve(Ordered {
            listing,
            order: vec![1, 0],
            view,
        })
        .expect("order resolves");
    let event = cmd.events().next().expect("the order comes back");
    let _ = app.update(event, &mut model);
    let view = shown(&app, &model);
    assert_eq!(view.view.sort, Sort::Size);
    assert_eq!(view.rows[0].name, "y");
}

#[test]
fn choosing_the_current_sort_again_reverses_it() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/a", &[]);
    let _ = app.update(
        Event::SortBy {
            tab,
            sort: Sort::Name,
        },
        &mut model,
    );
    assert!(shown(&app, &model).view.invert);
    let _ = app.update(
        Event::SetReverse {
            tab,
            reverse: false,
        },
        &mut model,
    );
    assert!(!shown(&app, &model).view.invert);
}

#[test]
fn a_folder_keeps_its_view_and_others_use_the_default() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/a", &[]);
    let _ = app.update(Event::SetShowHidden { tab, show: true }, &mut model);
    open(&app, &mut model, tab, "/b", &[]);
    assert!(
        !shown(&app, &model).view.show_hidden,
        "/b has no view of its own"
    );
    let mut cmd = app.update(Event::Back { tab }, &mut model);
    assert!(cmd.effects().any(|e| matches!(
        e,
        Effect::ListDir(r) if r.operation.path == "/a" && r.operation.view.show_hidden
    )));
    assert!(shown(&app, &model).view.show_hidden);
}

#[test]
fn every_tab_showing_the_folder_changes_with_it() {
    let app = DirFleet;
    let mut model = Model::default();
    let first = window(&app, &mut model);
    let second = window(&app, &mut model);
    open(&app, &mut model, first, "/a", &[entry("x", false)]);
    open(&app, &mut model, second, "/a", &[entry("x", false)]);
    let mut cmd = app.update(
        Event::SetReverse {
            tab: first,
            reverse: true,
        },
        &mut model,
    );
    assert_eq!(reorders(&mut cmd).len(), 2);
    let view = app.view(&model);
    assert!(view.windows.iter().all(|w| w.active.view.invert));
}

#[test]
fn an_order_for_a_replaced_listing_is_dropped() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/a",
        &[entry("x", false), entry("y", false)],
    );
    let mut cmd = app.update(
        Event::SortBy {
            tab,
            sort: Sort::Size,
        },
        &mut model,
    );
    let mut request = reorders(&mut cmd).remove(0);
    // The folder is read again before the order arrives.
    open(
        &app,
        &mut model,
        tab,
        "/a",
        &[entry("x", false), entry("y", false)],
    );
    let stale = Arc::clone(&request.operation.listing);
    let view = request.operation.view.clone();
    request
        .resolve(Ordered {
            listing: stale,
            order: vec![1, 0],
            view,
        })
        .expect("order resolves");
    let event = cmd.events().next().expect("the stale order comes back");
    let mut cmd = app.update(event, &mut model);
    assert!(cmd.effects().next().is_none());
    assert_eq!(shown(&app, &model).rows[0].name, "x");
}

#[test]
fn switching_list_and_grid_only_redraws() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/a", &[entry("x", false)]);
    let mut cmd = app.update(
        Event::SetDisplay {
            tab,
            display: DisplayType::Icons,
        },
        &mut model,
    );
    assert!(reorders(&mut cmd).is_empty());
    assert_eq!(shown(&app, &model).view.display, DisplayType::Icons);
}

#[test]
fn folder_views_are_saved_and_come_back() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = start(&app, &mut model, None, LoadedState::Missing);
    let tab = shown(&app, &model).id;
    let mut cmd = app.update(Event::SetShowHidden { tab, show: true }, &mut model);
    let saved = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::SaveState(request) => Some(request.operation.state),
            _ => None,
        })
        .expect("the view is saved");
    assert!(saved.folder_views["/home"].show_hidden);

    let mut model = Model::default();
    let _ = start(&app, &mut model, None, LoadedState::Found(saved));
    assert!(shown(&app, &model).view.show_hidden);
}

#[test]
fn a_listing_read_with_an_old_view_is_reordered_and_the_tab_waits_for_it() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/a", &[entry("x", false)]);
    let mut refresh = app.update(Event::Refresh { tab }, &mut model);
    let mut list = refresh
        .effects()
        .find_map(|effect| match effect {
            Effect::ListDir(request) => Some(request),
            _ => None,
        })
        .expect("the folder is read again");
    // The sort changes while that read runs.
    let _ = app.update(
        Event::SortBy {
            tab,
            sort: Sort::Size,
        },
        &mut model,
    );
    let old_view = list.operation.view.clone();
    list.resolve(listing(&[entry("x", false)], old_view))
        .expect("listing resolves");
    let event = refresh.events().next().expect("the listing comes back");
    let mut cmd = app.update(event, &mut model);
    let requests = reorders(&mut cmd);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].operation.view.sort, Sort::Size);
    assert!(
        shown(&app, &model).loading,
        "busy until the new order arrives"
    );
}

#[test]
fn rows_carry_every_column_and_columns_follow_the_choice() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/a", &[entry("photo.png", false)]);
    let view = app.view(&model);
    assert_eq!(view.windows[0].active.rows[0].cells.len(), 2);
    let titles = |view: &ViewModel| {
        view.columns
            .iter()
            .map(|c| c.title.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        titles(&view),
        ["Name", "Modified", "Size"],
        "egui's defaults"
    );

    let _ = app.update(
        Event::ToggleColumn {
            column: Column::Kind,
        },
        &mut model,
    );
    let _ = app.update(
        Event::ToggleColumn {
            column: Column::Modified,
        },
        &mut model,
    );
    let view = app.view(&model);
    assert_eq!(titles(&view), ["Name", "Kind", "Size"]);
    assert_eq!(view.windows[0].active.rows[0].cells, ["PNG image", "10 B"]);
    assert!(
        view.optional_columns
            .iter()
            .any(|c| c.column == Column::Kind && c.shown)
    );

    let _ = app.update(
        Event::SetColumnWidth {
            column: Column::Kind,
            width: 5000,
        },
        &mut model,
    );
    let kind = app.view(&model).columns[1].clone();
    assert_eq!(kind.width, 600, "widths are clamped like egui's");
    let _ = app.update(Event::ResetColumns, &mut model);
    assert_eq!(titles(&app.view(&model)), ["Name", "Modified", "Size"]);
}

#[test]
fn name_cannot_be_hidden() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = app.update(
        Event::ToggleColumn {
            column: Column::Name,
        },
        &mut model,
    );
    assert_eq!(app.view(&model).columns[0].column, Column::Name);
}

#[test]
fn columns_are_saved_and_come_back() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = start(&app, &mut model, None, LoadedState::Missing);
    let mut cmd = app.update(
        Event::ToggleColumn {
            column: Column::Kind,
        },
        &mut model,
    );
    let saved = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::SaveState(request) => Some(request.operation.state),
            _ => None,
        })
        .expect("the columns are saved");
    assert!(saved.columns.is_shown(Column::Kind));
    let mut model = Model::default();
    let _ = start(&app, &mut model, None, LoadedState::Found(saved));
    assert!(
        app.view(&model)
            .columns
            .iter()
            .any(|c| c.column == Column::Kind)
    );
}

#[test]
fn an_empty_view_says_how_many_items_are_hidden() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let mut cmd = app.update(
        Event::Navigate {
            tab,
            path: "/a".into(),
        },
        &mut model,
    );
    let mut list = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::ListDir(request) => Some(request),
            _ => None,
        })
        .expect("a listing is requested");
    // The folder holds two hidden items, so nothing is shown.
    let entries = [entry(".git", true), entry(".env", false)];
    list.resolve(ListDirOutput::Listed {
        listing: Arc::new(Listing::from_entries(&entries)),
        order: Vec::new(),
        view: ListView::default(),
        opened: Arc::default(),
    })
    .expect("listing resolves");
    let event = cmd.events().next().expect("the listing comes back");
    let _ = app.update(event, &mut model);
    let view = shown(&app, &model);
    assert_eq!((view.total_rows, view.hidden_count), (0, 2));
}

fn rows_selected(app: &DirFleet, model: &Model) -> Vec<String> {
    shown(app, model)
        .rows
        .iter()
        .filter(|row| row.selected)
        .map(|row| row.name.clone())
        .collect()
}

fn click(app: &DirFleet, model: &mut Model, tab: u32, row: u32, command: bool, shift: bool) {
    let _ = app.update(
        Event::Click {
            tab,
            row,
            command,
            shift,
        },
        model,
    );
}

#[test]
fn clicks_select_toggle_and_extend_like_egui() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let names = ["a", "b", "c", "d"].map(|name| entry(name, false));
    open(&app, &mut model, tab, "/x", &names);
    click(&app, &mut model, tab, 1, false, false);
    assert_eq!(rows_selected(&app, &model), ["b"]);
    click(&app, &mut model, tab, 3, true, false);
    assert_eq!(rows_selected(&app, &model), ["b", "d"]);
    // Shift selects from the anchor (the last click, "d") to "a", replacing.
    click(&app, &mut model, tab, 0, false, true);
    assert_eq!(rows_selected(&app, &model), ["a", "b", "c", "d"]);
    click(&app, &mut model, tab, 2, true, false);
    assert_eq!(rows_selected(&app, &model), ["a", "b", "d"]);
    let view = shown(&app, &model);
    assert_eq!(view.cursor, Some(2));
    assert_eq!(view.selected_count, 3);
    assert_eq!(
        view.selection,
        [RowRange { start: 0, end: 2 }, RowRange { start: 3, end: 4 }]
    );
}

#[test]
fn select_all_is_one_range_and_invert_and_clear_follow() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let entries: Vec<_> = (0..1000)
        .map(|i| entry(&format!("f{i:04}"), false))
        .collect();
    open(&app, &mut model, tab, "/x", &entries);
    let _ = app.update(Event::SelectAll { tab }, &mut model);
    let view = shown(&app, &model);
    assert_eq!(
        view.selection,
        [RowRange {
            start: 0,
            end: 1000
        }]
    );
    assert_eq!(view.selected_count, 1000);
    click(&app, &mut model, tab, 5, true, false);
    let _ = app.update(Event::InvertSelection { tab }, &mut model);
    assert_eq!(
        shown(&app, &model).selection,
        [RowRange { start: 5, end: 6 }]
    );
    let _ = app.update(Event::ClearSelection { tab }, &mut model);
    let view = shown(&app, &model);
    assert_eq!(view.selected_count, 0);
    assert_eq!(view.cursor, Some(5), "clearing keeps the cursor");
}

#[test]
fn the_selection_tells_apart_files_of_one_name_in_different_folders() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("a", false)]);
    let _ = app.update(Event::SetSearchDepth { tab, depth: 3 }, &mut model);
    let mut cmd = search_text(&app, &mut model, tab, "note");
    let mut walk = walks(&mut cmd).pop().expect("subfolders are walked");
    let view = walk.operation.view.clone();
    let found = [
        DirEntry::test_new("/x/notes.txt"),
        DirEntry::test_new("/x/deep/notes.txt"),
    ];
    walk.resolve(WalkUpdate {
        output: listing(&found, view),
        done: true,
        root: "/x".into(),
        matched: Arc::default(),
        scanned: 0,
    })
    .expect("done");
    let event = cmd.events().next().expect("all results");
    let _ = app.update(event, &mut model);

    // The one in "deep" is selected, then the rows swap places.
    click(&app, &mut model, tab, 1, false, false);
    let mut cmd = app.update(
        Event::SortBy {
            tab,
            sort: Sort::Size,
        },
        &mut model,
    );
    let mut request = reorders(&mut cmd).remove(0);
    let read = Arc::clone(&request.operation.listing);
    let view = request.operation.view.clone();
    request
        .resolve(Ordered {
            listing: read,
            order: vec![1, 0],
            view,
        })
        .expect("order resolves");
    let event = cmd.events().next().expect("the order comes back");
    let _ = app.update(event, &mut model);
    let view = shown(&app, &model);
    assert_eq!(view.selection, [RowRange { start: 0, end: 1 }]);
    assert_eq!(view.cursor, Some(0), "the cursor follows it too");
    assert_ne!(view.rows[0].folder, "", "the selected one is in \"deep\"");
}

#[test]
fn the_selection_stays_on_its_entries_when_rows_move() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[entry("a", false), entry("b", false), entry("c", false)],
    );
    click(&app, &mut model, tab, 0, false, false);
    click(&app, &mut model, tab, 2, true, false);
    let mut cmd = app.update(
        Event::SortBy {
            tab,
            sort: Sort::Name,
        },
        &mut model,
    );
    let mut request = reorders(&mut cmd).remove(0);
    let read = Arc::clone(&request.operation.listing);
    let view = request.operation.view.clone();
    request
        .resolve(Ordered {
            listing: read,
            order: vec![2, 1, 0],
            view,
        })
        .expect("order resolves");
    let event = cmd.events().next().expect("the order comes back");
    let _ = app.update(event, &mut model);
    assert_eq!(rows_selected(&app, &model), ["c", "a"]);
    let view = shown(&app, &model);
    assert_eq!(
        view.selection,
        [RowRange { start: 0, end: 1 }, RowRange { start: 2, end: 3 }]
    );
    assert_eq!(view.cursor, Some(0), "the cursor follows \"c\"");

    // A refresh without "a" drops it and keeps "c".
    let mut refresh = app.update(Event::Refresh { tab }, &mut model);
    let mut list = refresh
        .effects()
        .find_map(|effect| match effect {
            Effect::ListDir(request) => Some(request),
            _ => None,
        })
        .expect("the folder is read again");
    let view = list.operation.view.clone();
    list.resolve(listing(&[entry("b", false), entry("c", false)], view))
        .expect("listing resolves");
    let event = refresh.events().next().expect("the listing comes back");
    let _ = app.update(event, &mut model);
    assert_eq!(rows_selected(&app, &model), ["c"]);

    // Opening the folder again starts unselected.
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[entry("b", false), entry("c", false)],
    );
    assert_eq!(rows_selected(&app, &model), Vec::<String>::new());
}

#[test]
fn a_shell_list_reports_its_selection() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[entry("a", false), entry("b", false)],
    );
    let _ = app.update(
        Event::SetSelection {
            tab,
            rows: vec![1, 7],
        },
        &mut model,
    );
    assert_eq!(
        rows_selected(&app, &model),
        ["b"],
        "rows past the end are ignored"
    );
}

#[test]
fn opening_a_selected_folder_navigates_into_it() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[entry("sub", true), entry("f", false)],
    );
    click(&app, &mut model, tab, 0, false, false);
    let mut cmd = app.update(Event::OpenSelection { tab }, &mut model);
    assert_eq!(
        cmd.events().next(),
        Some(Event::Navigate {
            tab,
            path: "/x/sub".into()
        })
    );
}

#[test]
fn quick_look_shows_the_selection_from_the_cursor_and_opens_what_it_shows() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[
            entry("a.png", false),
            entry("b.png", false),
            entry("c.png", false),
        ],
    );
    let mut cmd = app.update(Event::QuickLook { tab }, &mut model);
    assert!(cmd.effects().next().is_none(), "nothing to show yet");
    click(&app, &mut model, tab, 0, false, false);
    click(&app, &mut model, tab, 2, true, false);
    let mut cmd = app.update(Event::QuickLook { tab }, &mut model);
    let look = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::QuickLook(request) => Some(request.operation),
            _ => None,
        })
        .expect("Quick Look is asked for");
    assert_eq!(look.paths, ["/x/a.png", "/x/c.png"]);
    assert_eq!(look.index, 1, "starting at the cursor, c.png");
    assert_eq!(look.tab, tab);

    let mut cmd = app.update(
        Event::OpenPreviewed {
            tab,
            path: "/x/b.png".into(),
        },
        &mut model,
    );
    assert_eq!(
        cmd.events().next(),
        Some(Event::OpenRow { tab, row: 1 }),
        "Return opens the item the panel shows"
    );
    let mut cmd = app.update(
        Event::OpenPreviewed {
            tab,
            path: "/x/gone.png".into(),
        },
        &mut model,
    );
    assert!(cmd.events().next().is_none());
}

#[test]
fn a_view_changed_while_an_order_was_made_is_ordered_again() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[entry("a", false), entry("b", false)],
    );
    let mut first = app.update(
        Event::SortBy {
            tab,
            sort: Sort::Size,
        },
        &mut model,
    );
    // Reversed before the first order arrives: no second order is asked yet.
    let mut second = app.update(Event::SetReverse { tab, reverse: true }, &mut model);
    assert!(reorders(&mut second).is_empty());
    let mut request = reorders(&mut first).remove(0);
    let (listing, view) = (
        Arc::clone(&request.operation.listing),
        request.operation.view.clone(),
    );
    assert!(!view.invert);
    request
        .resolve(Ordered {
            listing,
            order: vec![0, 1],
            view,
        })
        .expect("order resolves");
    let event = first.events().next().expect("the order comes back");
    let mut cmd = app.update(event, &mut model);
    let again = reorders(&mut cmd);
    assert_eq!(again.len(), 1, "ordered again for the current view");
    assert!(again[0].operation.view.invert);
    assert!(shown(&app, &model).loading);
}

#[test]
fn the_keyboard_moves_the_cursor_and_shift_extends() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let names = ["a", "b", "c", "d"].map(|name| entry(name, false));
    open(&app, &mut model, tab, "/x", &names);
    let key = |model: &mut Model, row: u32, command: bool, shift: bool| {
        let _ = app.update(
            Event::MoveCursor {
                tab,
                row,
                command,
                shift,
            },
            model,
        );
    };
    key(&mut model, 1, false, false);
    assert_eq!(rows_selected(&app, &model), ["b"]);
    key(&mut model, 3, false, true);
    assert_eq!(rows_selected(&app, &model), ["b", "c", "d"]);
    key(&mut model, 0, true, false);
    assert_eq!(
        rows_selected(&app, &model),
        ["b", "c", "d"],
        "Cmd moves only the cursor"
    );
    assert_eq!(shown(&app, &model).cursor, Some(0));
    key(&mut model, 9, false, false);
    assert_eq!(
        shown(&app, &model).cursor,
        Some(0),
        "past the end is ignored"
    );
}

// Tabs and panes.

/// The ids of the window's tabs, pane by pane, and the focused tab.
fn tabs_of(app: &DirFleet, model: &Model, window: usize) -> (Vec<u32>, u32) {
    let view = app.view(model);
    let window = &view.windows[window];
    (window.tabs.iter().map(|t| t.id).collect(), window.active.id)
}

/// Lists in `cmd` (paths), resolving none.
fn listed_paths(cmd: &mut Command<Effect, Event>) -> Vec<String> {
    cmd.effects()
        .filter_map(|effect| match effect {
            Effect::ListDir(request) => Some(request.operation.path),
            _ => None,
        })
        .collect()
}

#[test]
fn a_new_tab_goes_to_the_end_of_the_focused_pane_at_its_folder() {
    let app = DirFleet;
    let mut model = Model::default();
    let first = window(&app, &mut model);
    open(&app, &mut model, first, "/a", &[]);
    let mut cmd = app.update(
        Event::NewTab {
            window: 1,
            path: None,
        },
        &mut model,
    );
    assert_eq!(listed_paths(&mut cmd), ["/a"], "the focused tab's folder");
    let mut cmd = app.update(
        Event::NewTab {
            window: 1,
            path: Some("/b".into()),
        },
        &mut model,
    );
    assert_eq!(listed_paths(&mut cmd), ["/b"]);
    let (tabs, focused) = tabs_of(&app, &model, 0);
    assert_eq!(tabs.len(), 3);
    assert_eq!(focused, tabs[2], "the new tab is shown and focused");
}

#[test]
fn closing_shows_the_left_neighbour_and_reopen_brings_it_back() {
    let app = DirFleet;
    let mut model = Model::default();
    let first = window(&app, &mut model);
    open(&app, &mut model, first, "/a", &[]);
    let _ = app.update(
        Event::NewTab {
            window: 1,
            path: Some("/b".into()),
        },
        &mut model,
    );
    let (tabs, _) = tabs_of(&app, &model, 0);
    let mut cmd = app.update(Event::CloseTab { tab: tabs[1] }, &mut model);
    assert!(
        cmd.effects().any(|e| matches!(e, Effect::Unwatch(_))),
        "the closed tab stops watching"
    );
    assert_eq!(tabs_of(&app, &model, 0), (vec![tabs[0]], tabs[0]));
    let mut cmd = app.update(Event::ReopenClosedTab { window: 1 }, &mut model);
    assert_eq!(listed_paths(&mut cmd), ["/b"]);
    assert_eq!(tabs_of(&app, &model, 0).0.len(), 2);
}

#[test]
fn the_first_window_keeps_its_last_tab_and_others_close_with_theirs() {
    let app = DirFleet;
    let mut model = Model::default();
    let first = window(&app, &mut model);
    let second = window(&app, &mut model);
    let _ = app.update(Event::CloseTab { tab: first }, &mut model);
    assert_eq!(app.view(&model).windows.len(), 2, "the first window stays");
    let _ = app.update(Event::CloseTab { tab: second }, &mut model);
    assert_eq!(app.view(&model).windows.len(), 1);
}

#[test]
fn tabs_cycle_and_go_to_by_number() {
    let app = DirFleet;
    let mut model = Model::default();
    let first = window(&app, &mut model);
    open(&app, &mut model, first, "/a", &[]);
    for path in ["/b", "/c"] {
        let _ = app.update(
            Event::NewTab {
                window: 1,
                path: Some(path.into()),
            },
            &mut model,
        );
    }
    let (tabs, _) = tabs_of(&app, &model, 0);
    let _ = app.update(
        Event::CycleTab {
            window: 1,
            forward: true,
        },
        &mut model,
    );
    assert_eq!(tabs_of(&app, &model, 0).1, tabs[0], "wraps around");
    let _ = app.update(
        Event::CycleTab {
            window: 1,
            forward: false,
        },
        &mut model,
    );
    assert_eq!(tabs_of(&app, &model, 0).1, tabs[2]);
    let _ = app.update(
        Event::GoToTab {
            window: 1,
            number: 2,
        },
        &mut model,
    );
    assert_eq!(tabs_of(&app, &model, 0).1, tabs[1]);
    let _ = app.update(
        Event::GoToTab {
            window: 1,
            number: 9,
        },
        &mut model,
    );
    assert_eq!(tabs_of(&app, &model, 0).1, tabs[2], "9 is the last");
}

#[test]
fn splitting_shows_two_panes_each_with_its_rows() {
    let app = DirFleet;
    let mut model = Model::default();
    let first = window(&app, &mut model);
    open(&app, &mut model, first, "/a", &[entry("in_a", false)]);
    let _ = app.update(
        Event::NewTab {
            window: 1,
            path: Some("/b".into()),
        },
        &mut model,
    );
    let second = tabs_of(&app, &model, 0).1;
    let _ = app.update(
        Event::SplitTab {
            tab: second,
            side: Side::Right,
        },
        &mut model,
    );
    let view = app.view(&model);
    let window = &view.windows[0];
    assert_eq!(window.panes.len(), 2);
    assert_eq!(
        window.panes[0].shown.rows[0].name, "in_a",
        "both panes show rows"
    );
    assert_eq!(window.panes[1].shown.id, second);
    assert_eq!(window.layout.splits.len(), 1);
    let _ = app.update(
        Event::ResizeSplit {
            window: 1,
            split: 0,
            permille: 700,
        },
        &mut model,
    );
    assert_eq!(app.view(&model).windows[0].layout.splits[0].permille, 700);
    let _ = app.update(Event::CloseTab { tab: second }, &mut model);
    assert_eq!(
        app.view(&model).windows[0].panes.len(),
        1,
        "the panes merge back"
    );
}

#[test]
fn tabs_move_to_a_new_window_and_beside_another_tab() {
    let app = DirFleet;
    let mut model = Model::default();
    let first = window(&app, &mut model);
    open(&app, &mut model, first, "/a", &[]);
    let _ = app.update(
        Event::NewTab {
            window: 1,
            path: Some("/b".into()),
        },
        &mut model,
    );
    let moved = tabs_of(&app, &model, 0).1;
    let mut cmd = app.update(
        Event::MoveTab {
            tab: moved,
            to: TabTarget::NewWindow,
        },
        &mut model,
    );
    assert!(cmd.effects().any(|e| matches!(e, Effect::OpenWindow(_))));
    let view = app.view(&model);
    assert_eq!(view.windows.len(), 2);
    assert_eq!(view.windows[1].active.id, moved);

    let _ = app.update(
        Event::MoveTab {
            tab: moved,
            to: TabTarget::Beside {
                tab: first,
                index: 0,
            },
        },
        &mut model,
    );
    let view = app.view(&model);
    assert_eq!(view.windows.len(), 1, "the emptied window goes");
    assert_eq!(tabs_of(&app, &model, 0), (vec![moved, first], moved));
}

#[test]
fn a_shell_layout_is_adopted_only_with_the_same_tabs() {
    let app = DirFleet;
    let mut model = Model::default();
    let first = window(&app, &mut model);
    open(&app, &mut model, first, "/a", &[]);
    let _ = app.update(
        Event::NewTab {
            window: 1,
            path: Some("/b".into()),
        },
        &mut model,
    );
    let second = tabs_of(&app, &model, 0).1;
    let split = FlatLayout {
        root: NodeRef::Split(0),
        panes: vec![
            FlatPane {
                tabs: vec![second],
                active: 0,
            },
            FlatPane {
                tabs: vec![first],
                active: 0,
            },
        ],
        splits: vec![FlatSplit {
            vertical: true,
            permille: 300,
            first: NodeRef::Pane(0),
            second: NodeRef::Pane(1),
        }],
    };
    let mut wrong = split.clone();
    wrong.panes[0].tabs = vec![999];
    let _ = app.update(
        Event::SetLayout {
            window: 1,
            layout: wrong,
        },
        &mut model,
    );
    assert_eq!(
        app.view(&model).windows[0].panes.len(),
        1,
        "an unknown tab is refused"
    );
    let _ = app.update(
        Event::SetLayout {
            window: 1,
            layout: split.clone(),
        },
        &mut model,
    );
    assert_eq!(app.view(&model).windows[0].layout, split);
}

#[test]
fn duplicating_and_closing_the_others() {
    let app = DirFleet;
    let mut model = Model::default();
    let first = window(&app, &mut model);
    open(&app, &mut model, first, "/a", &[]);
    let mut cmd = app.update(Event::DuplicateTab { tab: first }, &mut model);
    assert_eq!(listed_paths(&mut cmd), ["/a"]);
    let (tabs, focused) = tabs_of(&app, &model, 0);
    assert_eq!(tabs, vec![first, focused], "the copy follows the original");
    let _ = app.update(Event::CloseOtherTabs { tab: first }, &mut model);
    assert_eq!(tabs_of(&app, &model, 0), (vec![first], first));
}

#[test]
fn split_layouts_are_saved_and_hidden_tabs_load_when_shown() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = start(&app, &mut model, None, LoadedState::Missing);
    for path in ["/b", "/c"] {
        let _ = app.update(
            Event::NewTab {
                window: 1,
                path: Some(path.into()),
            },
            &mut model,
        );
    }
    let third = tabs_of(&app, &model, 0).1;
    let mut cmd = app.update(
        Event::SplitTab {
            tab: third,
            side: Side::Below,
        },
        &mut model,
    );
    let saved = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::SaveState(request) => Some(request.operation.state),
            _ => None,
        })
        .expect("the layout is saved");
    let window = &saved.windows[0];
    assert_eq!(window.tabs, ["/home", "/b", "/c"]);
    assert!(window.layout.is_some());
    assert_eq!(
        window.active, 2,
        "the focused tab, the one split off, is saved too"
    );

    let mut model = Model::default();
    let effects = start(&app, &mut model, None, LoadedState::Found(saved));
    let listed: Vec<_> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::ListDir(r) => Some(r.operation.path.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(listed, ["/b", "/c"], "each pane's shown tab; /home waits");
    let view = app.view(&model);
    assert_eq!(view.windows[0].panes.len(), 2);
    let hidden = view.windows[0].tabs[0].id;
    let mut cmd = app.update(Event::ActivateTab { tab: hidden }, &mut model);
    assert_eq!(listed_paths(&mut cmd), ["/home"], "shown, so listed");
}

// History.

/// Answers the listing `cmd` asks for with `entries`.
fn answer(
    app: &DirFleet,
    model: &mut Model,
    mut cmd: Command<Effect, Event>,
    entries: &[DirEntry],
) {
    let mut list = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::ListDir(request) => Some(request),
            _ => None,
        })
        .expect("a listing is requested");
    let view = list.operation.view.clone();
    list.resolve(listing(entries, view))
        .expect("listing resolves");
    let event = cmd.events().next().expect("the listing comes back");
    let _ = app.update(event, model);
}

#[test]
fn back_and_forward_return_to_where_you_were() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let in_a = ["a1", "a2", "a3"].map(|name| entry(name, false));
    open(&app, &mut model, tab, "/a", &in_a);
    click(&app, &mut model, tab, 2, false, false);
    let _ = app.update(
        Event::ViewportChanged {
            tab,
            first_row: 1,
            rows: 10,
        },
        &mut model,
    );
    open(&app, &mut model, tab, "/b", &[]);
    let view = shown(&app, &model);
    assert!(view.nav.back);
    assert!(!view.nav.forward);

    let back = app.update(Event::Back { tab }, &mut model);
    answer(&app, &mut model, back, &in_a);
    let view = shown(&app, &model);
    assert_eq!(view.path, "/a");
    assert_eq!(
        rows_selected(&app, &model),
        ["a3"],
        "the selection comes back"
    );
    assert_eq!(view.scroll_to, Some(1), "and the scroll row");
    assert!(view.nav.forward);
    let count = view.scroll_count;

    let forward = app.update(Event::Forward { tab }, &mut model);
    answer(&app, &mut model, forward, &[]);
    let view = shown(&app, &model);
    assert_eq!(view.path, "/b");
    assert!(!view.nav.forward);
    assert_ne!(view.scroll_count, count, "each return asks to scroll again");
}

#[test]
fn going_somewhere_new_after_back_drops_forward() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/a", &[]);
    open(&app, &mut model, tab, "/b", &[]);
    let back = app.update(Event::Back { tab }, &mut model);
    answer(&app, &mut model, back, &[]);
    open(&app, &mut model, tab, "/c", &[]);
    assert!(!shown(&app, &model).nav.forward);
}

#[test]
fn up_shows_the_enclosing_folder() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let below = Path::new("/")
        .join("outer")
        .join("inner")
        .to_string_lossy()
        .into_owned();
    open(&app, &mut model, tab, &below, &[]);
    assert!(shown(&app, &model).nav.up);
    let mut cmd = app.update(Event::Up { tab }, &mut model);
    let outer = Path::new("/").join("outer").to_string_lossy().into_owned();
    assert_eq!(listed_paths(&mut cmd), [outer]);
    assert!(shown(&app, &model).nav.back, "Up is a visit like any other");
}

#[test]
fn a_split_from_a_single_tab_copies_it_into_a_new_pane() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/a", &[]);
    let mut cmd = app.update(
        Event::SplitNew {
            tab,
            side: Side::Right,
        },
        &mut model,
    );
    assert_eq!(listed_paths(&mut cmd), ["/a"]);
    let window = app.view(&model).windows[0].clone();
    assert_eq!(window.panes.len(), 2);
    assert_eq!(window.panes[0].shown.id, tab);
    assert_eq!(
        window.active.id, window.panes[1].shown.id,
        "the new pane is focused"
    );
}

#[test]
fn a_closed_window_can_be_reopened_as_a_window_but_not_after_quitting() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = window(&app, &mut model);
    let second = window(&app, &mut model);
    open(&app, &mut model, second, "/b", &[]);
    let _ = app.update(Event::WindowClosed { window: 2 }, &mut model);
    let mut cmd = app.update(Event::ReopenClosedAsWindow, &mut model);
    assert!(cmd.effects().any(|e| matches!(e, Effect::OpenWindow(_))));
    assert_eq!(app.view(&model).windows.len(), 2);

    let _ = app.update(Event::Quitting, &mut model);
    let _ = app.update(Event::WindowClosed { window: 3 }, &mut model);
    let mut cmd = app.update(Event::ReopenClosedAsWindow, &mut model);
    assert!(
        cmd.effects().next().is_none(),
        "windows closing on the way out are not reopen-able"
    );
}

#[test]
fn a_window_group_is_shown_and_saved() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = start(&app, &mut model, None, LoadedState::Missing);
    let mut cmd = app.update(
        Event::WindowGrouped {
            window: 1,
            group: Some(1),
        },
        &mut model,
    );
    let effects: Vec<_> = cmd.effects().collect();
    assert!(effects.iter().any(|e| matches!(e, Effect::Render(_))));
    assert_eq!(saved_windows(&effects).map(|w| w[0].group), Some(Some(1)));
    assert_eq!(app.view(&model).windows[0].group, Some(1));
}

// Tabs a shell made itself (egui).

fn adopted(tab: u32, path: &str) -> AdoptedTab {
    AdoptedTab {
        tab,
        path: Some(path.into()),
    }
}

#[test]
fn a_shell_s_own_tabs_are_adopted_with_their_ids() {
    let app = DirFleet;
    let mut model = Model::default();
    let mut cmd = app.update(
        Event::AdoptWindow {
            tabs: vec![adopted(7, "/a"), adopted(9, "/b")],
            active: 1,
        },
        &mut model,
    );
    assert!(
        cmd.effects()
            .any(|e| matches!(e, Effect::OpenWindow(r) if r.operation.window == 1))
    );
    assert_eq!(tabs_of(&app, &model, 0), (vec![7, 9], 9));
    let _ = app.update(
        Event::AdoptTab {
            window: 1,
            tab: adopted(12, "/c"),
        },
        &mut model,
    );
    assert_eq!(tabs_of(&app, &model, 0), (vec![7, 9, 12], 12));
    let _ = app.update(
        Event::AdoptTab {
            window: 1,
            tab: adopted(9, "/again"),
        },
        &mut model,
    );
    assert_eq!(tabs_of(&app, &model, 0).0.len(), 3, "a taken id is refused");
    let _ = app.update(
        Event::NewTab {
            window: 1,
            path: None,
        },
        &mut model,
    );
    let (tabs, _) = tabs_of(&app, &model, 0);
    assert_eq!(
        tabs[3], 13,
        "ids the core hands out come after adopted ones"
    );
}

#[test]
fn closing_the_first_window_hands_it_the_second_one_s_tabs() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = app.update(
        Event::AdoptWindow {
            tabs: vec![adopted(1, "/a")],
            active: 0,
        },
        &mut model,
    );
    let _ = app.update(
        Event::AdoptWindow {
            tabs: vec![adopted(2, "/b")],
            active: 0,
        },
        &mut model,
    );
    let _ = app.update(Event::CloseFirstWindow, &mut model);
    let view = app.view(&model);
    assert_eq!(view.windows.len(), 1);
    assert_eq!(view.windows[0].id, 1, "the first window stays");
    assert_eq!(tabs_of(&app, &model, 0), (vec![2], 2));
    let mut again = app.update(Event::CloseFirstWindow, &mut model);
    assert!(again.effects().next().is_none(), "alone, it stays as it is");
}

/// Opens the Trash in `tab` and answers its listing with `entries`.
fn open_trash(app: &DirFleet, model: &mut Model, tab: u32, entries: &[DirEntry]) {
    let mut cmd = app.update(Event::OpenTrash { tab }, model);
    let mut list = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::ListTrash(request) => Some(request),
            Effect::ListDir(_) | Effect::Watch(_) => {
                panic!("the Trash is neither read nor watched")
            }
            _ => None,
        })
        .expect("the Trash is listed");
    let view = list.operation.view.clone();
    list.resolve(listing(entries, view))
        .expect("listing resolves");
    let event = cmd.events().next().expect("the listing comes back");
    let _ = app.update(event, model);
}

fn trash_op(cmd: &mut Command<Effect, Event>) -> Option<Request<TrashOp>> {
    cmd.effects().find_map(|effect| match effect {
        Effect::Trash(request) => Some(request),
        _ => None,
    })
}

#[test]
fn the_trash_opens_in_a_tab_and_back_returns_to_the_folder() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/a", &[entry("x.txt", false)]);
    open_trash(&app, &mut model, tab, &[entry("gone.txt", false)]);

    let view = shown(&app, &model);
    assert!(view.in_trash);
    assert_eq!(view.title, TrashInfo::here().label);
    assert_eq!(view.rows[0].name, "gone.txt");
    assert!(view.nav.back);
    assert!(!view.nav.up, "the Trash has no parent");

    let mut cmd = app.update(Event::Back { tab }, &mut model);
    assert!(
        cmd.effects()
            .any(|e| matches!(e, Effect::ListDir(r) if r.operation.path == "/a"))
    );
    assert!(!shown(&app, &model).in_trash);

    let mut cmd = app.update(Event::Forward { tab }, &mut model);
    assert!(
        cmd.effects().any(|e| matches!(e, Effect::ListTrash(_))),
        "Forward goes back into the Trash"
    );
    assert!(shown(&app, &model).in_trash);
}

#[test]
fn the_trash_keeps_its_own_view() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open_trash(&app, &mut model, tab, &[entry("gone.txt", false)]);
    let _ = app.update(
        Event::SetDisplay {
            tab,
            display: DisplayType::Icons,
        },
        &mut model,
    );
    open(&app, &mut model, tab, "/a", &[entry("x.txt", false)]);
    assert_ne!(shown(&app, &model).view.display, DisplayType::Icons);
    open_trash(&app, &mut model, tab, &[]);
    assert_eq!(shown(&app, &model).view.display, DisplayType::Icons);
}

#[test]
fn deleting_trashed_items_for_good_lists_the_trash_again() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open_trash(
        &app,
        &mut model,
        tab,
        &[entry("a.txt", false), entry("b.txt", false)],
    );

    let mut cmd = app.update(Event::PurgeSelection { tab }, &mut model);
    assert!(trash_op(&mut cmd).is_none(), "nothing is selected");

    click(&app, &mut model, tab, 1, false, false);
    let mut cmd = app.update(Event::PurgeSelection { tab }, &mut model);
    let mut purge = trash_op(&mut cmd).expect("the selection is purged");
    let TrashOp::Purge(keys) = &purge.operation else {
        panic!("a purge, not {:?}", purge.operation);
    };
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].path, Path::new("/x/b.txt"));

    purge
        .resolve(TrashDone(Err("in use".into())))
        .expect("resolves");
    let event = cmd.events().next().expect("the change comes back");
    let mut cmd = app.update(event, &mut model);
    assert!(cmd.effects().any(|e| matches!(e, Effect::ListTrash(_))));
    assert_eq!(shown(&app, &model).error.as_deref(), Some("in use"));
}

#[test]
fn emptying_the_trash_asks_the_effects_and_folders_cannot_be_purged() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let mut cmd = app.update(Event::EmptyTrash, &mut model);
    assert_eq!(
        trash_op(&mut cmd).map(|request| request.operation),
        Some(TrashOp::Empty)
    );

    open(&app, &mut model, tab, "/a", &[entry("x.txt", false)]);
    click(&app, &mut model, tab, 0, false, false);
    for event in [
        Event::PurgeSelection { tab },
        Event::RestoreSelection { tab },
    ] {
        let mut cmd = app.update(event, &mut model);
        assert!(
            trash_op(&mut cmd).is_none(),
            "a folder's rows are not trashed items"
        );
    }
}

#[test]
fn restoring_is_offered_where_the_platform_allows_it() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open_trash(&app, &mut model, tab, &[entry("a.txt", false)]);
    click(&app, &mut model, tab, 0, false, false);
    let mut cmd = app.update(Event::RestoreSelection { tab }, &mut model);
    let restore = trash_op(&mut cmd).map(|request| request.operation);
    if TrashInfo::here().can_restore {
        assert!(matches!(restore, Some(TrashOp::Restore(keys)) if keys.len() == 1));
    } else {
        assert_eq!(restore, None);
    }
}

fn search_text(app: &DirFleet, model: &mut Model, tab: u32, text: &str) -> Command<Effect, Event> {
    app.update(
        Event::SetSearchText {
            tab,
            text: text.into(),
        },
        model,
    )
}

fn walks(cmd: &mut Command<Effect, Event>) -> Vec<Request<SearchWalk>> {
    cmd.effects()
        .filter_map(|effect| match effect {
            Effect::SearchWalk(request) => Some(request),
            _ => None,
        })
        .collect()
}

#[test]
fn typing_filters_the_listing_without_reading_it_again() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[entry("report.pdf", false), entry("notes.txt", false)],
    );

    let mut cmd = search_text(&app, &mut model, tab, "rep");
    let mut effects: Vec<Effect> = cmd.effects().collect();
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::ListDir(_) | Effect::SearchWalk(_))),
        "the folder is not read again"
    );
    let order = effects
        .iter_mut()
        .find_map(|effect| match effect {
            Effect::OrderListing(request) => Some(request),
            _ => None,
        })
        .expect("the listing is filtered");
    let filter = order.operation.view.filter.clone().expect("a filter");
    assert_eq!(filter.value, "rep");
    let listing = Arc::clone(&order.operation.listing);
    let view = order.operation.view.clone();
    order
        .resolve(Ordered {
            listing,
            order: vec![0],
            view,
        })
        .expect("resolves");
    let event = cmd.events().next().expect("the order comes back");
    let _ = app.update(event, &mut model);

    let shown = shown(&app, &model);
    assert_eq!(shown.total_rows, 1);
    assert_eq!(shown.search.expect("open").text, "rep");
    assert_eq!(
        shown.rows[0].highlights,
        vec![TextRange { start: 0, end: 3 }]
    );
    assert!(!shown.show_folder);
}

#[test]
fn an_order_for_an_older_query_is_made_again() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("report.pdf", false)]);
    let mut first = search_text(&app, &mut model, tab, "r");
    let mut second = search_text(&app, &mut model, tab, "re");
    assert!(
        reorders(&mut second).is_empty(),
        "one order at a time: the newer query waits for the older one"
    );
    let mut request = reorders(&mut first).remove(0);
    let (listing, view) = (
        Arc::clone(&request.operation.listing),
        request.operation.view.clone(),
    );
    request
        .resolve(Ordered {
            listing,
            order: vec![0],
            view,
        })
        .expect("resolves");
    let event = first.events().next().expect("the order comes back");
    let mut cmd = app.update(event, &mut model);
    let again = reorders(&mut cmd);
    assert_eq!(again.len(), 1, "ordered again for \"re\"");
    assert_eq!(
        again[0]
            .operation
            .view
            .filter
            .as_ref()
            .map(|f| f.value.as_str()),
        Some("re")
    );
}

#[test]
fn a_pause_in_typing_records_the_search_and_back_returns_to_the_folder() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("a", false)]);

    let mut first = search_text(&app, &mut model, tab, "a");
    let mut second = search_text(&app, &mut model, tab, "ab");
    let delays = |cmd: &mut Command<Effect, Event>| -> Vec<Request<Delay>> {
        cmd.effects()
            .filter_map(|effect| match effect {
                Effect::Delay(request) => Some(request),
                _ => None,
            })
            .collect()
    };
    let mut early = delays(&mut first).remove(0);
    early.resolve(()).expect("resolves");
    let event = first.events().next().expect("the pause comes back");
    let _ = app.update(event, &mut model);
    assert!(
        !shown(&app, &model).nav.back,
        "typing went on: nothing recorded yet"
    );

    let mut late = delays(&mut second).remove(0);
    late.resolve(()).expect("resolves");
    let event = second.events().next().expect("the pause comes back");
    let _ = app.update(event, &mut model);
    assert!(shown(&app, &model).nav.back, "the search is recorded");

    let mut cmd = app.update(Event::Back { tab }, &mut model);
    assert!(
        cmd.effects()
            .any(|e| matches!(e, Effect::ListDir(r) if r.operation.view.filter.is_none()))
    );
    assert_eq!(shown(&app, &model).search, None);

    let mut cmd = app.update(Event::Forward { tab }, &mut model);
    assert!(cmd.effects().any(
        |e| matches!(e, Effect::ListDir(r) if r.operation.view.filter.as_ref().is_some_and(|f| f.value == "ab"))
    ));
    assert_eq!(shown(&app, &model).search.expect("back").text, "ab");
}

#[test]
fn searching_subfolders_walks_them_and_rows_say_where_they_are() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("a", false)]);
    let _ = app.update(Event::SetSearchDepth { tab, depth: 3 }, &mut model);

    let mut cmd = search_text(&app, &mut model, tab, "note");
    let mut walk = walks(&mut cmd).pop().expect("subfolders are walked");
    assert_eq!(walk.operation.roots, vec!["/x".to_owned()]);
    assert_eq!(walk.operation.depth, 3);
    let view = walk.operation.view.clone();
    // "/x" is a symlink: the walk names entries by where they really are.
    let found = [
        DirEntry::test_new("/real/x/notes.txt"),
        DirEntry::test_new("/real/x/deep/inner/notes.md"),
    ];
    walk.resolve(WalkUpdate {
        output: listing(&found[..1], view.clone()),
        done: false,
        root: "/real/x".into(),
        matched: Arc::default(),
        scanned: 0,
    })
    .expect("partial");
    let event = cmd.events().next().expect("partial results");
    let _ = app.update(event, &mut model);
    let partial = shown(&app, &model);
    assert_eq!(partial.total_rows, 1);
    assert!(partial.search.expect("open").searching);
    assert!(partial.loading);
    assert!(partial.show_folder);
    assert_eq!(partial.progress, "Scanning 1 folder…");
    assert_eq!(
        partial.rows[0].detail, "",
        "results show their folder instead"
    );

    walk.resolve(WalkUpdate {
        output: listing(&found, view),
        done: true,
        root: "/real/x".into(),
        matched: Arc::default(),
        scanned: 0,
    })
    .expect("done");
    let event = cmd.events().next().expect("all results");
    let _ = app.update(event, &mut model);
    let done = shown(&app, &model);
    assert!(!done.search.expect("open").searching);
    assert!(!done.loading);
    assert_eq!(done.progress, "");
    let folders: Vec<_> = done.rows.iter().map(|row| row.folder.clone()).collect();
    assert_eq!(
        folders,
        vec![
            String::new(),
            Path::new("deep")
                .join("inner")
                .to_string_lossy()
                .into_owned()
        ]
    );

    click(&app, &mut model, tab, 1, false, false);
    let mut cmd = app.update(Event::QuickLook { tab }, &mut model);
    let look = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::QuickLook(request) => Some(request.operation),
            _ => None,
        })
        .expect("previewed");
    assert_eq!(
        look.paths[look.index as usize],
        Path::new("/real/x/deep/inner/notes.md")
            .to_string_lossy()
            .into_owned(),
        "a result's path is in its own folder"
    );
}

#[test]
fn stopping_keeps_what_was_found_and_closing_shows_the_folder_again() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("a", false)]);
    let _ = app.update(Event::SetSearchDepth { tab, depth: 2 }, &mut model);
    let mut cmd = search_text(&app, &mut model, tab, "a");
    let mut walk = walks(&mut cmd).pop().expect("a walk");
    let id = walk.operation.id;
    let view = walk.operation.view.clone();
    walk.resolve(WalkUpdate {
        output: listing(&[entry("a", false)], view),
        done: false,
        root: "/x".into(),
        matched: Arc::default(),
        scanned: 0,
    })
    .expect("partial");
    let event = cmd.events().next().expect("partial results");
    let _ = app.update(event, &mut model);

    let mut cmd = app.update(Event::StopSearch { tab }, &mut model);
    assert!(
        cmd.effects()
            .any(|e| matches!(e, Effect::StopWalk(r) if r.operation.id == id)),
        "the handler is told to stop"
    );
    let stopped = shown(&app, &model);
    assert_eq!(stopped.total_rows, 1);
    assert!(!stopped.search.expect("open").searching);

    let mut cmd = app.update(Event::CloseSearch { tab }, &mut model);
    assert!(
        cmd.effects()
            .any(|e| matches!(e, Effect::ListDir(r) if r.operation.path == "/x" && r.operation.view.filter.is_none())),
        "the folder alone is read again"
    );
    assert_eq!(shown(&app, &model).search, None);
}

#[test]
fn typed_text_becomes_chips_and_filters_can_be_added_and_removed() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("a", false)]);
    let _ = search_text(&app, &mut model, tab, "report ext:pdf");
    let _ = app.update(Event::CommitSearchText { tab }, &mut model);
    let _ = app.update(
        Event::ToggleSearchFilter {
            tab,
            token: "kind:image".into(),
        },
        &mut model,
    );
    let _ = app.update(
        Event::ToggleSearchFilter {
            tab,
            token: "nonsense".into(),
        },
        &mut model,
    );
    let search = shown(&app, &model).search.expect("open");
    let tokens: Vec<_> = search
        .chips
        .iter()
        .map(|chip| chip.token.as_str())
        .collect();
    assert_eq!(tokens, ["report", "ext:pdf", "kind:image"]);
    assert_eq!(search.text, "");

    let _ = app.update(Event::RemoveSearchChip { tab, index: 1 }, &mut model);
    let _ = search_text(&app, &mut model, tab, "/[a-/");
    let _ = app.update(Event::CommitSearchText { tab }, &mut model);
    let search = shown(&app, &model).search.expect("open");
    assert_eq!(search.chips.len(), 3);
    assert!(
        search.chips[2].error.is_some(),
        "a broken pattern says why: {:?}",
        search.chips[2]
    );
}

#[test]
fn preset_filters_turn_on_and_off_and_say_which_are_on() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("a", false)]);
    let filters = app.view(&model).search_filters;
    let token = |group: &str, label: &str| {
        filters
            .iter()
            .find(|g| g.label == group)
            .and_then(|g| g.choices.iter().find(|c| c.label == label))
            .map(|c| c.token.clone())
            .expect("a preset")
    };
    let toggle = |model: &mut Model, token: String| {
        let _ = app.update(Event::ToggleSearchFilter { tab, token }, model);
    };
    toggle(&mut model, token("Type", "Image"));
    toggle(&mut model, token("Type", "Video"));
    toggle(&mut model, token("Size", "Over 1 GB"));
    let search = shown(&app, &model).search.expect("open");
    assert_eq!(search.chips.len(), 2, "kinds share one chip: {search:?}");
    assert_eq!(search.filters_on.len(), 3);

    toggle(&mut model, token("Type", "Image"));
    toggle(&mut model, token("Size", "Over 1 GB"));
    let search = shown(&app, &model).search.expect("open");
    assert_eq!(search.filters_on, vec![token("Type", "Video")]);
}

#[test]
fn opening_a_folder_ends_the_search_and_back_brings_it_back() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("sub", true)]);
    let _ = search_text(&app, &mut model, tab, "su");
    let _ = app.update(Event::CommitSearchText { tab }, &mut model);
    let mut cmd = app.update(Event::OpenRow { tab, row: 0 }, &mut model);
    let event = cmd.events().next().expect("navigates");
    let _ = app.update(event, &mut model);
    assert_eq!(shown(&app, &model).search, None);

    let _ = app.update(Event::Back { tab }, &mut model);
    let search = shown(&app, &model).search.expect("history keeps it");
    assert_eq!(search.chips[0].token, "su");
}

fn sidebar(app: &DirFleet, model: &Model) -> Vec<SidebarSectionView> {
    app.view(model).windows[0].sidebar.clone()
}

fn section(app: &DirFleet, model: &Model, which: SidebarSection) -> Vec<SidebarItem> {
    sidebar(app, model)
        .into_iter()
        .find(|s| s.section == which)
        .map(|s| s.items)
        .unwrap_or_default()
}

fn titles(items: &[SidebarItem]) -> Vec<&str> {
    items.iter().map(|item| item.title.as_str()).collect()
}

#[test]
fn starting_lists_the_user_s_folders_and_the_drives() {
    let app = DirFleet;
    let mut model = Model::default();
    let mut cmd = app.update(
        Event::Started {
            open: None,
            home: "/home".into(),
        },
        &mut model,
    );
    let mut effects: Vec<Effect> = cmd.effects().collect();
    let folders = effects.iter_mut().find_map(|effect| match effect {
        Effect::UserFolders(request) => Some(request),
        _ => None,
    });
    folders
        .expect("the user's folders are asked for")
        .resolve(vec![
            Bookmark {
                name: "User".into(),
                path: "/home".into(),
            },
            Bookmark {
                name: "Documents".into(),
                path: "/home/Documents".into(),
            },
        ])
        .expect("resolves");
    let drives = effects.iter_mut().find_map(|effect| match effect {
        Effect::WatchDrives(request) => Some(request),
        _ => None,
    });
    drives
        .expect("the drives are watched")
        .resolve(DriveList(vec![
            crate::drives::test_drive("/", "Macintosh HD", crate::drives::DriveKind::Fixed),
            crate::drives::test_drive("/Volumes/USB", "USB", crate::drives::DriveKind::Removable),
        ]))
        .expect("resolves");
    let events: Vec<Event> = cmd
        .events()
        .filter(|event| !matches!(event, Event::StateLoaded { .. }))
        .collect();
    for event in events {
        let _ = app.update(event, &mut model);
    }
    let _ = window(&app, &mut model);

    let locations = section(&app, &model, SidebarSection::Locations);
    assert_eq!(titles(&locations), ["User", "Documents", "Trash"]);
    assert_eq!(locations[1].icon, SidebarIcon::Documents);
    assert!(locations[2].trash);
    let drives = section(&app, &model, SidebarSection::Drives);
    assert_eq!(titles(&drives), ["Macintosh HD", "USB"]);
    assert_eq!(drives[0].eject, None);
    assert_eq!(drives[1].eject.as_deref(), Some("Eject"));
    assert_eq!(drives[1].used_permille, Some(750));
    assert_eq!(drives[1].detail, "16.0 GB free of 64.0 GB");
}

#[test]
fn recent_folders_leave_out_the_one_shown() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    for path in ["/a", "/b", "/c", "/a"] {
        open(&app, &mut model, tab, path, &[]);
    }
    let recent = section(&app, &model, SidebarSection::Recent);
    assert_eq!(titles(&recent), ["c", "b"]);
    assert!(recent.iter().all(|item| !item.active));
}

#[test]
fn favorites_are_added_moved_removed_and_kept() {
    let app = DirFleet;
    let mut model = Model::default();
    let effects = start(&app, &mut model, Some("/a"), LoadedState::Missing);
    assert!(saved_windows(&effects).is_some());
    for path in ["/a", "/data/b", "/a"] {
        let _ = app.update(Event::AddFavorite { path: path.into() }, &mut model);
    }
    let favorites = section(&app, &model, SidebarSection::Favorites);
    assert_eq!(titles(&favorites), ["a", "b"]);
    assert!(favorites[0].active, "the tab shows /a");
    assert!(favorites[0].favorite);

    let _ = app.update(Event::MoveFavorite { from: 1, to: 0 }, &mut model);
    let mut cmd = app.update(
        Event::SetSectionCollapsed {
            section: SidebarSection::Recent,
            collapsed: true,
        },
        &mut model,
    );
    let saved = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::SaveState(request) => Some(request.operation.state),
            _ => None,
        })
        .expect("saved");
    assert_eq!(saved.favorites.len(), 2);
    assert_eq!(saved.favorites[0].path, "/data/b");
    assert_eq!(saved.collapsed_sections, vec![SidebarSection::Recent]);

    let mut restored = Model::default();
    let _ = start(&app, &mut restored, None, LoadedState::Found(saved));
    let favorites = section(&app, &restored, SidebarSection::Favorites);
    assert_eq!(titles(&favorites), ["b", "a"]);

    let _ = app.update(Event::RemoveFavorite { path: "/a".into() }, &mut restored);
    assert_eq!(
        titles(&section(&app, &restored, SidebarSection::Favorites)),
        ["b"]
    );
}

#[test]
fn ejecting_moves_tabs_on_the_drive_home_and_a_refusal_says_why() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = start(
        &app,
        &mut model,
        Some("/Volumes/USB/photos"),
        LoadedState::Missing,
    );
    let tab = shown(&app, &model).id;

    let mut cmd = app.update(
        Event::EjectDrive {
            mount: "/Volumes/USB".into(),
        },
        &mut model,
    );
    let mut eject = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::Eject(request) => Some(request),
            _ => None,
        })
        .expect("the drive is ejected");
    assert_eq!(eject.operation.mount, "/Volumes/USB");
    eject
        .resolve(EjectDone(Err("in use by Preview".into())))
        .expect("resolves");
    let event = cmd.events().next().expect("the answer comes back");
    let _ = app.update(event, &mut model);
    let toasts = app.view(&model).toasts;
    assert_eq!(toasts.len(), 1);
    assert_eq!(
        (toasts[0].text.as_str(), toasts[0].kind),
        ("in use by Preview", ToastKind::Error)
    );
    let _ = app.update(Event::DismissToast { id: toasts[0].id }, &mut model);
    assert_eq!(app.view(&model).toasts, Vec::new());

    let mut cmd = app.update(
        Event::Ejected {
            mount: "/Volumes/USB".into(),
            done: EjectDone(Ok(())),
        },
        &mut model,
    );
    assert_eq!(
        cmd.events().next(),
        Some(Event::Navigate {
            tab,
            path: "/home".into()
        })
    );
}

#[test]
fn the_favorites_can_be_searched_instead_of_the_folder() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("a", false)]);
    for path in ["/f1", "/f2"] {
        let _ = app.update(Event::AddFavorite { path: path.into() }, &mut model);
    }
    let _ = search_text(&app, &mut model, tab, "a");
    let mut cmd = app.update(Event::SetSearchFavorites { tab, on: true }, &mut model);
    let walk = walks(&mut cmd).pop().expect("the favorites are read");
    assert_eq!(
        walk.operation.roots,
        vec!["/f1".to_owned(), "/f2".to_owned()]
    );
    assert_eq!(walk.operation.depth, 1);
    assert_eq!(
        shown(&app, &model).search.expect("open").scope,
        SearchScope::Favorites
    );

    let mut cmd = app.update(Event::CloseSearch { tab }, &mut model);
    assert!(
        cmd.effects()
            .any(|e| matches!(e, Effect::ListDir(r) if r.operation.path == "/x")),
        "closing goes back to the folder"
    );
}

fn system_ops(cmd: &mut Command<Effect, Event>) -> Vec<SystemOp> {
    cmd.effects()
        .filter_map(|effect| match effect {
            Effect::System(request) => Some(request.operation),
            _ => None,
        })
        .collect()
}

fn package(name: &str) -> DirEntry {
    let mut entry = DirEntry::test_new(&format!("/x/{name}"));
    entry.meta.entry_type = EntryType::Package;
    entry
}

#[test]
fn opening_a_file_hands_it_to_its_app_and_packages_open_too() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[entry("notes.txt", false), package("Tool.app")],
    );
    let mut cmd = app.update(Event::OpenRow { tab, row: 0 }, &mut model);
    assert_eq!(
        system_ops(&mut cmd),
        vec![SystemOp::Open(vec![path("/x/notes.txt")])]
    );
    let mut cmd = app.update(Event::OpenRow { tab, row: 1 }, &mut model);
    assert_eq!(
        system_ops(&mut cmd),
        vec![SystemOp::Open(vec![path("/x/Tool.app")])]
    );

    click(&app, &mut model, tab, 1, false, false);
    let mut cmd = app.update(Event::ShowPackageContents { tab }, &mut model);
    assert_eq!(
        cmd.events().next(),
        Some(Event::Navigate {
            tab,
            path: path("/x/Tool.app")
        })
    );
}

fn path(text: &str) -> String {
    Path::new(text).to_string_lossy().into_owned()
}

#[test]
fn opening_several_folders_opens_tabs_and_files_open_together() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[
            entry("a", true),
            entry("b", true),
            entry("c.txt", false),
            entry("d.txt", false),
        ],
    );
    let _ = app.update(Event::SelectAll { tab }, &mut model);
    let mut cmd = app.update(Event::OpenSelection { tab }, &mut model);
    assert_eq!(
        system_ops(&mut cmd),
        vec![SystemOp::Open(vec![path("/x/c.txt"), path("/x/d.txt")])]
    );
    let tabs: Vec<_> = cmd
        .events()
        .filter_map(|event| match event {
            Event::NewTab { path, .. } => path,
            _ => None,
        })
        .collect();
    assert_eq!(tabs, vec![path("/x/a"), path("/x/b")]);
}

#[test]
fn reveal_info_terminal_and_copy_act_on_the_selection_or_the_folder() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[entry("a.txt", false), entry("b.txt", false)],
    );
    let mut cmd = app.update(Event::RevealSelection { tab }, &mut model);
    assert_eq!(
        system_ops(&mut cmd),
        vec![SystemOp::Reveal(vec!["/x".into()])]
    );

    let _ = app.update(Event::SelectAll { tab }, &mut model);
    let mut cmd = app.update(Event::CopyPaths { tab }, &mut model);
    assert_eq!(
        system_ops(&mut cmd),
        vec![SystemOp::CopyText(format!(
            "{}\n{}",
            path("/x/a.txt"),
            path("/x/b.txt")
        ))]
    );
    let mut cmd = app.update(Event::GetInfo { tab }, &mut model);
    assert!(
        matches!(system_ops(&mut cmd).as_slice(), [SystemOp::GetInfo(paths)] if paths.len() == 2)
    );
    let mut cmd = app.update(Event::OpenInTerminal { tab }, &mut model);
    assert_eq!(
        system_ops(&mut cmd),
        vec![SystemOp::Terminal {
            folder: "/x".into(),
            app: Settings::default().terminal
        }]
    );
}

#[test]
fn open_with_asks_for_an_app_and_a_failure_becomes_a_toast() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("a.txt", false)]);
    click(&app, &mut model, tab, 0, false, false);
    let mut cmd = app.update(Event::OpenWith { tab }, &mut model);
    let mut pick = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::PickApp(request) => Some(request),
            _ => None,
        })
        .expect("an app is asked for");
    pick.resolve(PickFolderOutput::Picked(
        "/Applications/TextEdit.app".into(),
    ))
    .expect("resolves");
    let event = cmd.events().next().expect("the app comes back");
    let mut cmd = app.update(event, &mut model);
    assert_eq!(
        system_ops(&mut cmd),
        vec![SystemOp::OpenWith {
            app: "/Applications/TextEdit.app".into(),
            paths: vec![path("/x/a.txt")]
        }]
    );

    let _ = app.update(
        Event::SystemDone {
            done: SystemDone(Err("no such app".into())),
        },
        &mut model,
    );
    assert_eq!(
        app.view(&model)
            .toasts
            .first()
            .map(|toast| toast.text.clone()),
        Some("no such app".into())
    );
}

fn file_ops(cmd: &mut Command<Effect, Event>) -> Vec<Request<RunFileOp>> {
    cmd.effects()
        .filter_map(|effect| match effect {
            Effect::RunFileOp(request) => Some(request),
            _ => None,
        })
        .collect()
}

fn outcome(op_id: u64, origin: crate::fileops::Origin) -> crate::fileops::Outcome {
    crate::fileops::Outcome {
        id: op_id,
        origin,
        description: String::new(),
        errors: Vec::new(),
        cancelled: false,
        undo: None,
        affected_dirs: vec![PathBuf::from("/x")],
        created: Vec::new(),
        trash_changed: false,
        verified: false,
        needs_password: None,
    }
}

/// Answers the operation in `cmd` with `outcome` and feeds the result back.
fn finish(
    app: &DirFleet,
    model: &mut Model,
    cmd: &mut Command<Effect, Event>,
    outcome: crate::fileops::Outcome,
) -> Command<Effect, Event> {
    let mut run = file_ops(cmd).pop().expect("an operation runs");
    let id = run.operation.id;
    run.resolve(FileOpUpdate(crate::fileops::FileOpEvent::Finished(
        crate::fileops::Outcome { id, ..outcome },
    )))
    .expect("resolves");
    let event = cmd.events().next().expect("the outcome comes back");
    app.update(event, model)
}

/// Two drives: the disk everything is on, and a stick.
fn two_drives(app: &DirFleet, model: &mut Model) {
    let usb = crate::drives::test_drive("/Volumes/USB", "USB", crate::drives::DriveKind::Removable);
    let disk = crate::drives::test_drive("/", "Macintosh HD", crate::drives::DriveKind::Fixed);
    let _ = app.update(
        Event::DrivesChanged {
            drives: DriveList(vec![disk, usb]),
        },
        model,
    );
}

/// Duplicates the first row of `folder` (a copy on the folder's drive): the
/// command, and the operation's request when it was started at once.
fn duplicate_in(
    app: &DirFleet,
    model: &mut Model,
    tab: u32,
    folder: &str,
) -> (Command<Effect, Event>, Option<Request<RunFileOp>>) {
    open(app, model, tab, folder, &[entry("a", false)]);
    click(app, model, tab, 0, false, false);
    let mut cmd = app.update(Event::Duplicate { tab }, model);
    let run = file_ops(&mut cmd).pop();
    (cmd, run)
}

/// Ends the operation `run` started by `cmd` and feeds the outcome back.
fn end(
    app: &DirFleet,
    model: &mut Model,
    cmd: &mut Command<Effect, Event>,
    run: &mut Request<RunFileOp>,
) -> Command<Effect, Event> {
    let id = run.operation.id;
    run.resolve(FileOpUpdate(crate::fileops::FileOpEvent::Finished(
        outcome(id, crate::fileops::Origin::User),
    )))
    .expect("resolves");
    let event = cmd.events().next().expect("the outcome comes back");
    app.update(event, model)
}

fn states(app: &DirFleet, model: &Model) -> Vec<OperationState> {
    app.view(model)
        .operations
        .iter()
        .map(|operation| operation.state)
        .collect()
}

fn pauses(cmd: &mut Command<Effect, Event>) -> Vec<PauseFileOp> {
    cmd.effects()
        .filter_map(|effect| match effect {
            Effect::PauseFileOp(request) => Some(request.operation),
            _ => None,
        })
        .collect()
}

fn started_ids(cmd: &mut Command<Effect, Event>) -> Vec<u64> {
    file_ops(cmd).iter().map(|run| run.operation.id).collect()
}

#[test]
fn copies_on_one_drive_take_turns_and_those_on_another_start_at_once() {
    use OperationState::{Running, Waiting};
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    two_drives(&app, &mut model);

    let (mut first_cmd, first) = duplicate_in(&app, &mut model, tab, "/x");
    let mut first = first.expect("the first starts at once");
    let (_, second) = duplicate_in(&app, &mut model, tab, "/x");
    assert!(second.is_none(), "the second waits for the disk");
    let (_, third) = duplicate_in(&app, &mut model, tab, "/Volumes/USB/y");
    assert!(third.is_some(), "the stick is free");
    assert_eq!(states(&app, &model), [Running, Waiting, Running]);
    let waiting = &app.view(&model).operations[1];
    assert_eq!(waiting.detail, "Waiting");
    assert!(waiting.can_pause);

    // Making a folder is not a copy: it never waits.
    open(&app, &mut model, tab, "/x", &[entry("a", false)]);
    let mut cmd = app.update(Event::NewFolder { tab }, &mut model);
    let folder = file_ops(&mut cmd).pop().expect("made at once");
    assert!(!folder.operation.op.is_bulk());
    assert!(!app.view(&model).operations[3].can_pause);

    // The first ends: the second has the disk.
    let mut next = end(&app, &mut model, &mut first_cmd, &mut first);
    assert_eq!(started_ids(&mut next), [2]);
    assert_eq!(states(&app, &model), [Running, Running, Running]);
}

#[test]
fn the_queue_setting_says_how_many_copies_run() {
    use OperationState::{Running, Waiting};
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    two_drives(&app, &mut model);
    let settings = |queue_mode| Event::ChangeSettings {
        settings: Settings {
            queue_mode,
            ..Settings::default()
        },
    };

    let _ = app.update(settings(QueueMode::OneAtATime), &mut model);
    let (_, first) = duplicate_in(&app, &mut model, tab, "/x");
    assert!(first.is_some());
    let (_, second) = duplicate_in(&app, &mut model, tab, "/Volumes/USB/y");
    assert!(second.is_none(), "one at a time, whatever the drive");
    let (_, third) = duplicate_in(&app, &mut model, tab, "/x");
    assert!(third.is_none());
    assert_eq!(states(&app, &model), [Running, Waiting, Waiting]);

    // One for each drive lets the stick's copy go; the disk's still waits.
    let mut cmd = app.update(settings(QueueMode::PerVolume), &mut model);
    assert_eq!(started_ids(&mut cmd), [2]);
    let mut cmd = app.update(settings(QueueMode::Unlimited), &mut model);
    assert_eq!(started_ids(&mut cmd), [3], "all at once");
    let (_, fourth) = duplicate_in(&app, &mut model, tab, "/x");
    assert!(fourth.is_some());
}

#[test]
fn a_paused_operation_lets_the_next_go_and_resumes() {
    use OperationState::{Paused, Running, Waiting};
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let (_, first) = duplicate_in(&app, &mut model, tab, "/x");
    assert!(first.is_some());
    let (_, second) = duplicate_in(&app, &mut model, tab, "/x");
    assert!(second.is_none());
    let (_, third) = duplicate_in(&app, &mut model, tab, "/x");
    assert!(third.is_none());

    // One that waits is paused without a word to the worker, and loses its turn.
    let mut cmd = app.update(Event::PauseOperation { id: 2 }, &mut model);
    assert_eq!(pauses(&mut cmd), Vec::new());
    assert_eq!(states(&app, &model), [Running, Paused, Waiting]);
    assert_eq!(app.view(&model).operations[1].detail, "Paused");

    // The running one is held where it is, and the next in line starts.
    let mut cmd = app.update(Event::PauseOperation { id: 1 }, &mut model);
    let (mut held, mut begun) = (Vec::new(), Vec::new());
    for effect in cmd.effects() {
        match effect {
            Effect::PauseFileOp(request) => held.push(request.operation),
            Effect::RunFileOp(request) => begun.push(request.operation.id),
            _ => {}
        }
    }
    assert_eq!(
        held,
        [PauseFileOp {
            id: 1,
            paused: true
        }]
    );
    assert_eq!(begun, [3], "the paused second is passed over");
    assert_eq!(states(&app, &model), [Paused, Paused, Running]);

    // Resumed, the first goes on; the second waits its turn again.
    let mut cmd = app.update(Event::ResumeOperation { id: 1 }, &mut model);
    assert_eq!(
        pauses(&mut cmd),
        [PauseFileOp {
            id: 1,
            paused: false
        }]
    );
    let mut cmd = app.update(Event::ResumeOperation { id: 2 }, &mut model);
    assert!(
        !cmd.effects()
            .any(|effect| matches!(effect, Effect::PauseFileOp(_) | Effect::RunFileOp(_))),
        "nothing is told and nothing starts"
    );
    assert_eq!(states(&app, &model), [Running, Waiting, Running]);

    // All at once: only those the worker has are told.
    let mut cmd = app.update(Event::PauseAllOperations, &mut model);
    let told: Vec<u64> = pauses(&mut cmd).iter().map(|pause| pause.id).collect();
    assert_eq!(told, [1, 3]);
    assert_eq!(states(&app, &model), [Paused, Paused, Paused]);
    let mut cmd = app.update(Event::ResumeAllOperations, &mut model);
    assert_eq!(pauses(&mut cmd).len(), 2);
    assert_eq!(states(&app, &model), [Running, Waiting, Running]);
}

#[test]
fn a_waiting_operation_can_be_started_now_or_dropped() {
    use OperationState::{Running, Waiting};
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let (_, first) = duplicate_in(&app, &mut model, tab, "/x");
    assert!(first.is_some());
    let _ = duplicate_in(&app, &mut model, tab, "/x");
    let _ = duplicate_in(&app, &mut model, tab, "/x");
    assert_eq!(states(&app, &model), [Running, Waiting, Waiting]);

    let mut cmd = app.update(Event::StartOperationNow { id: 3 }, &mut model);
    assert_eq!(started_ids(&mut cmd), [3]);
    assert_eq!(states(&app, &model), [Running, Waiting, Running]);

    // Stopping one that never started tells the worker nothing.
    let mut cmd = app.update(Event::CancelOperation { id: 2 }, &mut model);
    assert!(
        !cmd.effects()
            .any(|effect| matches!(effect, Effect::CancelFileOp(_)))
    );
    let view = app.view(&model);
    assert_eq!(view.operations.len(), 2);
    assert_eq!(view.activity[0].state, ActivityState::Stopped);
    assert_eq!(view.activity[0].text, "Stopped: Duplicating 1 item");

    // One that runs is told to stop.
    let mut cmd = app.update(Event::CancelOperation { id: 1 }, &mut model);
    assert!(
        cmd.effects()
            .any(|effect| matches!(effect, Effect::CancelFileOp(_)))
    );
}

#[test]
fn a_copy_says_how_much_is_done_and_how_fast() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let (mut cmd, run) = duplicate_in(&app, &mut model, tab, "/x");
    let mut run = run.expect("it starts");
    let mut progress = |done, rate| {
        run.resolve(FileOpUpdate(crate::fileops::FileOpEvent::Progress {
            id: 1,
            fraction: 0.5,
            current: "a".into(),
            bytes_done: done,
            bytes_total: 4_000_000_000,
            bytes_per_second: rate,
        }))
        .expect("progress");
        let event = cmd.events().next().expect("progress comes back");
        let _ = app.update(event, &mut model);
        app.view(&model).operations[0].detail.clone()
    };
    assert_eq!(progress(1_200_000_000, 0), "1.2 GB of 4.0 GB");
    assert_eq!(
        progress(2_000_000_000, 85_000_000),
        "2.0 GB of 4.0 GB \u{b7} 85.0 MB/s"
    );
}

/// The checksums the inspector shows for the tab's one selected file.
fn checksums(app: &DirFleet, model: &Model) -> ChecksumsView {
    shown(app, model)
        .inspector
        .and_then(|inspector| inspector.item)
        .and_then(|item| item.checksums)
        .expect("a file's checksums")
}

/// The file reads `cmd` asked for.
fn hashes(cmd: &mut Command<Effect, Event>) -> Vec<Request<HashFile>> {
    cmd.effects()
        .filter_map(|effect| match effect {
            Effect::HashFile(request) => Some(request),
            _ => None,
        })
        .collect()
}

/// Answers the read `hash` started by `cmd` and feeds the answer back.
fn hashed(
    app: &DirFleet,
    model: &mut Model,
    cmd: &mut Command<Effect, Event>,
    hash: &mut Request<HashFile>,
    update: HashUpdate,
) -> Command<Effect, Event> {
    hash.resolve(update).expect("resolves");
    let event = cmd.events().next().expect("the update comes back");
    app.update(event, model)
}

const SHA_OF_A: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

/// A tab on `/x` with `a.txt` selected and the inspector shown.
fn inspecting(app: &DirFleet, model: &mut Model, entries: &[DirEntry]) -> u32 {
    let tab = window(app, model);
    open(app, model, tab, "/x", entries);
    click(app, model, tab, 0, false, false);
    let _ = app.update(Event::SetInspector { shown: true }, model);
    tab
}

#[test]
fn a_file_s_checksum_is_worked_out_on_request_and_copied() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = inspecting(
        &app,
        &mut model,
        &[entry("a.txt", false), entry("sub", true)],
    );
    let view = checksums(&app, &model);
    let labels: Vec<&str> = view.rows.iter().map(|row| row.label.as_str()).collect();
    assert_eq!(labels, ["CRC32", "MD5", "SHA-1", "SHA-256", "BLAKE3"]);
    assert!(
        view.rows
            .iter()
            .all(|row| row.value.is_empty() && !row.computing)
    );
    assert_eq!(view.progress, None);

    let path = "/x/a.txt".to_owned();
    let mut cmd = app.update(
        Event::ComputeChecksum {
            path: path.clone(),
            algorithm: Algorithm::Sha256,
        },
        &mut model,
    );
    let mut hash = hashes(&mut cmd).pop().expect("the file is read");
    assert_eq!(hash.operation.path, path);
    assert_eq!(hash.operation.algorithms, [Algorithm::Sha256]);
    let view = checksums(&app, &model);
    assert!(view.rows[3].computing);
    assert_eq!(view.progress, Some(0));

    let _ = hashed(
        &app,
        &mut model,
        &mut cmd,
        &mut hash,
        HashUpdate::Progress { done: 5, total: 10 },
    );
    assert_eq!(checksums(&app, &model).progress, Some(500));
    let _ = hashed(
        &app,
        &mut model,
        &mut cmd,
        &mut hash,
        HashUpdate::Done(vec![(Algorithm::Sha256, SHA_OF_A.into())]),
    );
    let view = checksums(&app, &model);
    assert_eq!(view.rows[3].value, SHA_OF_A);
    assert!(!view.rows[3].computing);
    assert_eq!(view.progress, None);

    // Asked again, nothing is read; copied, it goes to the clipboard as text.
    let mut again = app.update(
        Event::ComputeChecksum {
            path: path.clone(),
            algorithm: Algorithm::Sha256,
        },
        &mut model,
    );
    assert!(hashes(&mut again).is_empty());
    let mut copy = app.update(
        Event::CopyChecksum {
            path,
            algorithm: Algorithm::Sha256,
        },
        &mut model,
    );
    assert!(copy.effects().any(|effect| matches!(
        effect,
        Effect::System(request) if request.operation == SystemOp::CopyText(SHA_OF_A.into())
    )));

    // A folder has no checksums.
    click(&app, &mut model, tab, 1, false, false);
    let folder = shown(&app, &model)
        .inspector
        .and_then(|inspector| inspector.item)
        .expect("the folder");
    assert_eq!(folder.checksums, None);
}

#[test]
fn a_pasted_checksum_is_compared_with_the_file_s() {
    let app = DirFleet;
    let mut model = Model::default();
    inspecting(&app, &mut model, &[entry("a.txt", false)]);
    let path = "/x/a.txt".to_owned();
    let verify = |expected: &str| Event::VerifyChecksum {
        path: path.clone(),
        expected: expected.into(),
    };

    // 64 digits may be SHA-256 or BLAKE3: one read works out both.
    let pasted = format!("SHA256: {}", SHA_OF_A.to_uppercase());
    let mut cmd = app.update(verify(&pasted), &mut model);
    let mut hash = hashes(&mut cmd).pop().expect("the file is read");
    assert_eq!(
        hash.operation.algorithms,
        [Algorithm::Sha256, Algorithm::Blake3]
    );
    let view = checksums(&app, &model);
    assert_eq!(view.expected, SHA_OF_A);
    assert_eq!(view.verdict, ChecksumVerdict::Checking);
    let _ = hashed(
        &app,
        &mut model,
        &mut cmd,
        &mut hash,
        HashUpdate::Done(vec![
            (Algorithm::Sha256, SHA_OF_A.into()),
            (Algorithm::Blake3, "0".repeat(64)),
        ]),
    );
    let view = checksums(&app, &model);
    assert_eq!(view.verdict, ChecksumVerdict::Match);
    assert_eq!(view.message, "Matches the file's SHA-256");

    // Another of the same length matches neither: nothing is read again.
    let mut cmd = app.update(verify(&"f".repeat(64)), &mut model);
    assert!(hashes(&mut cmd).is_empty());
    let view = checksums(&app, &model);
    assert_eq!(view.verdict, ChecksumVerdict::Mismatch);
    assert_eq!(view.message, "Does not match the file's SHA-256 or BLAKE3");

    // Text that holds no checksum says so; empty text ends the comparison.
    let _ = app.update(verify("hello"), &mut model);
    assert_eq!(checksums(&app, &model).verdict, ChecksumVerdict::Unknown);
    let _ = app.update(verify(""), &mut model);
    let view = checksums(&app, &model);
    assert_eq!(view.verdict, ChecksumVerdict::None);
    assert_eq!(view.expected, "");
}

#[test]
fn reading_a_file_for_its_checksums_can_be_stopped_and_can_fail() {
    let app = DirFleet;
    let mut model = Model::default();
    inspecting(&app, &mut model, &[entry("a.txt", false)]);
    let path = "/x/a.txt".to_owned();
    let compute = |algorithm| Event::ComputeChecksum {
        path: path.clone(),
        algorithm,
    };
    let stops = |cmd: &mut Command<Effect, Event>| -> Vec<u64> {
        cmd.effects()
            .filter_map(|effect| match effect {
                Effect::StopHash(request) => Some(request.operation.id),
                _ => None,
            })
            .collect()
    };

    let mut first = app.update(compute(Algorithm::Md5), &mut model);
    let first_id = hashes(&mut first).pop().expect("read").operation.id;
    // Another asked for meanwhile: one new read serves both.
    let mut second = app.update(compute(Algorithm::Sha1), &mut model);
    let mut started = Vec::new();
    let mut stopped = Vec::new();
    for effect in second.effects() {
        match effect {
            Effect::HashFile(request) => started.push(request),
            Effect::StopHash(request) => stopped.push(request.operation.id),
            _ => {}
        }
    }
    assert_eq!(stopped, [first_id]);
    let hash = started.pop().expect("read again");
    assert_eq!(hash.operation.algorithms, [Algorithm::Sha1, Algorithm::Md5]);

    let mut cancel = app.update(Event::CancelChecksum { path: path.clone() }, &mut model);
    assert_eq!(stops(&mut cancel), [hash.operation.id]);
    let view = checksums(&app, &model);
    assert_eq!(view.progress, None);
    assert!(view.rows.iter().all(|row| !row.computing));

    // A read that fails says why.
    let mut cmd = app.update(compute(Algorithm::Crc32), &mut model);
    let mut hash = hashes(&mut cmd).pop().expect("read");
    let _ = hashed(
        &app,
        &mut model,
        &mut cmd,
        &mut hash,
        HashUpdate::Failed("a.txt can't be read: denied".into()),
    );
    let view = checksums(&app, &model);
    assert_eq!(view.message, "a.txt can't be read: denied");
    assert_eq!(view.progress, None);
}

#[test]
fn a_checksum_file_beside_a_file_is_offered_and_compared_with() {
    let app = DirFleet;
    let mut model = Model::default();
    inspecting(
        &app,
        &mut model,
        &[
            entry("a.txt", false),
            entry("a.txt.sha256", false),
            entry("SHA256SUMS", false),
            entry("b.txt.md5", false),
        ],
    );
    assert_eq!(
        checksums(&app, &model).sidecars,
        ["SHA256SUMS", "a.txt.sha256"]
    );

    let path = "/x/a.txt".to_owned();
    let mut cmd = app.update(
        Event::VerifySidecar {
            path: path.clone(),
            sidecar: "SHA256SUMS".into(),
        },
        &mut model,
    );
    let mut read = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::ReadSidecar(request) => Some(request),
            _ => None,
        })
        .expect("the checksum file is read");
    assert_eq!(read.operation.sidecar, "/x/SHA256SUMS");
    assert_eq!(read.operation.name, "a.txt");
    read.resolve(Some(SHA_OF_A.into())).expect("resolves");
    let event = cmd.events().next().expect("its checksum comes back");
    let mut cmd = app.update(event, &mut model);
    assert_eq!(hashes(&mut cmd).len(), 1, "the file is read to compare");
    assert_eq!(checksums(&app, &model).expected, SHA_OF_A);

    // A checksum file with nothing for this file says so.
    let _ = app.update(
        Event::SidecarRead {
            path,
            sidecar: "a.txt.sha256".into(),
            expected: None,
        },
        &mut model,
    );
    let view = checksums(&app, &model);
    assert_eq!(view.message, "a.txt.sha256 has no checksum for this file");
    assert_eq!(view.expected, "");
}

#[test]
fn checksums_of_an_earlier_version_of_a_file_are_not_shown() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = inspecting(&app, &mut model, &[entry("a.txt", false)]);
    let path = "/x/a.txt".to_owned();
    let mut cmd = app.update(
        Event::ComputeChecksum {
            path,
            algorithm: Algorithm::Md5,
        },
        &mut model,
    );
    let mut hash = hashes(&mut cmd).pop().expect("read");
    let _ = hashed(
        &app,
        &mut model,
        &mut cmd,
        &mut hash,
        HashUpdate::Done(vec![(Algorithm::Md5, "0".repeat(32))]),
    );
    assert_eq!(checksums(&app, &model).rows[1].value, "0".repeat(32));

    // The file grew: what was worked out is about what it used to hold.
    let mut changed = entry("a.txt", false);
    changed.meta.size = 11;
    open(&app, &mut model, tab, "/x", &[changed]);
    click(&app, &mut model, tab, 0, false, false);
    assert_eq!(checksums(&app, &model).rows[1].value, "");
}

#[test]
fn copies_are_checked_when_the_setting_says_so() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let (_, plain) = duplicate_in(&app, &mut model, tab, "/x");
    assert!(!plain.expect("starts").operation.verify);

    let _ = app.update(
        Event::ChangeSettings {
            settings: Settings {
                verify_copies: true,
                queue_mode: QueueMode::Unlimited,
                ..Settings::default()
            },
        },
        &mut model,
    );
    let (mut cmd, checked) = duplicate_in(&app, &mut model, tab, "/x");
    let mut checked = checked.expect("starts");
    assert!(checked.operation.verify);
    let id = checked.operation.id;
    checked
        .resolve(FileOpUpdate(crate::fileops::FileOpEvent::Finished(
            crate::fileops::Outcome {
                verified: true,
                ..outcome(id, crate::fileops::Origin::User)
            },
        )))
        .expect("resolves");
    let event = cmd.events().next().expect("the outcome comes back");
    let _ = app.update(event, &mut model);
    let view = app.view(&model);
    assert_eq!(
        view.toasts.last().map(|toast| toast.text.as_str()),
        Some("Duplicated 1 item (verified)")
    );
    assert_eq!(view.activity[0].text, "Duplicated 1 item (verified)");
}

/// The files the system is asked to open by `cmd`.
fn opened(cmd: &mut Command<Effect, Event>) -> Vec<String> {
    cmd.effects()
        .filter_map(|effect| match effect {
            Effect::System(request) => match request.operation {
                SystemOp::Open(paths) => Some(paths),
                _ => None,
            },
            _ => None,
        })
        .flatten()
        .collect()
}

#[test]
fn an_archive_opens_as_a_folder_and_up_leaves_it() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let files = [entry("pack.zip", false), entry("notes.txt", false)];
    open(&app, &mut model, tab, "/x", &files);
    let here = shown(&app, &model);
    assert_eq!(here.location.kind, crate::location::LocationKind::Local);
    assert!(here.location.caps.writable);

    let mut cmd = app.update(Event::OpenRow { tab, row: 0 }, &mut model);
    let inside = "archive:/x/pack.zip!/";
    assert_eq!(
        cmd.events().next(),
        Some(Event::Navigate {
            tab,
            path: inside.into()
        })
    );
    // Another file opens in its app, as ever.
    let mut cmd = app.update(Event::OpenRow { tab, row: 1 }, &mut model);
    assert_eq!(opened(&mut cmd), ["/x/notes.txt"]);

    open(
        &app,
        &mut model,
        tab,
        inside,
        &[entry("docs", true), entry("readme.txt", false)],
    );
    let view = shown(&app, &model);
    assert_eq!(view.title, "pack.zip");
    assert_eq!(view.location.kind, crate::location::LocationKind::Archive);
    assert!(!view.location.caps.writable && !view.location.caps.real_paths);
    let crumbs: Vec<(&str, &str)> = view
        .crumbs
        .iter()
        .map(|crumb| (crumb.title.as_str(), crumb.path.as_str()))
        .collect();
    assert_eq!(
        crumbs.last(),
        Some(&("pack.zip", inside)),
        "the archive's crumb leads to its top: {crumbs:?}"
    );
    assert!(view.nav.up);

    // A folder inside is browsed into; its crumb follows the archive's.
    let mut cmd = app.update(Event::OpenRow { tab, row: 0 }, &mut model);
    let docs = "archive:/x/pack.zip!/docs";
    assert_eq!(
        cmd.events().next(),
        Some(Event::Navigate {
            tab,
            path: docs.into()
        })
    );
    open(&app, &mut model, tab, docs, &[entry("a.md", false)]);
    let view = shown(&app, &model);
    assert_eq!(view.title, "docs");
    assert_eq!(
        view.crumbs.last().map(|crumb| crumb.path.as_str()),
        Some(docs)
    );

    // Up goes to the archive's top, then out to the folder it is in.
    let mut cmd = app.update(Event::Up { tab }, &mut model);
    assert_eq!(listed_paths(&mut cmd), [inside]);
    open(&app, &mut model, tab, inside, &[entry("docs", true)]);
    let mut cmd = app.update(Event::Up { tab }, &mut model);
    assert_eq!(listed_paths(&mut cmd), ["/x"]);
}

#[test]
fn archives_open_in_their_app_when_the_setting_says_so() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let _ = app.update(
        Event::ChangeSettings {
            settings: Settings {
                browse_archives: false,
                ..Settings::default()
            },
        },
        &mut model,
    );
    open(&app, &mut model, tab, "/x", &[entry("pack.zip", false)]);
    let mut cmd = app.update(Event::OpenRow { tab, row: 0 }, &mut model);
    assert_eq!(opened(&mut cmd), ["/x/pack.zip"]);
}

#[test]
fn a_file_inside_an_archive_is_brought_out_to_be_opened() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let inside = "archive:/x/pack.zip!/";
    open(&app, &mut model, tab, inside, &[entry("readme.txt", false)]);
    let materialize = |cmd: &mut Command<Effect, Event>| {
        cmd.effects()
            .find_map(|effect| match effect {
                Effect::Materialize(request) => Some(request),
                _ => None,
            })
            .expect("a copy is asked for")
    };

    // The selection opens the same way as a row does.
    click(&app, &mut model, tab, 0, false, false);
    for event in [Event::OpenRow { tab, row: 0 }, Event::OpenSelection { tab }] {
        let mut cmd = app.update(event, &mut model);
        let mut copy = materialize(&mut cmd);
        assert_eq!(copy.operation.location, "archive:/x/pack.zip!/readme.txt");
        copy.resolve(Materialized(Ok("/tmp/copies/readme.txt".into())))
            .expect("resolves");
        let event = cmd.events().next().expect("the copy comes back");
        let mut cmd = app.update(event, &mut model);
        assert_eq!(opened(&mut cmd), ["/tmp/copies/readme.txt"]);
    }

    // A copy that can't be made says why.
    let mut cmd = app.update(Event::OpenRow { tab, row: 0 }, &mut model);
    let mut copy = materialize(&mut cmd);
    copy.resolve(Materialized(Err(Unreadable::Failed(
        "The archive is damaged: bad header".into(),
    ))))
    .expect("resolves");
    let event = cmd.events().next().expect("the answer comes back");
    let _ = app.update(event, &mut model);
    let toast = app.view(&model).toasts.last().cloned().expect("a toast");
    assert_eq!(toast.text, "The archive is damaged: bad header");
    assert_eq!(toast.kind, ToastKind::Error);
}

#[test]
fn nothing_is_changed_or_handed_to_other_programs_inside_an_archive() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let inside = "archive:/x/pack.zip!/";
    open(&app, &mut model, tab, inside, &[entry("readme.txt", false)]);
    click(&app, &mut model, tab, 0, false, false);

    // Making, removing, renaming and copying beside: no operation runs.
    for event in [
        Event::NewFolder { tab },
        Event::NewTextFile { tab },
        Event::TrashSelection { tab },
        Event::DeleteSelection { tab },
        Event::Duplicate { tab },
        Event::StartRename { tab },
    ] {
        let mut cmd = app.update(event.clone(), &mut model);
        assert!(file_ops(&mut cmd).is_empty(), "{event:?}");
    }
    let view = app.view(&model);
    assert_eq!(view.operations, Vec::new());
    assert!(
        view.toasts
            .iter()
            .any(|toast| toast.text == "What is inside an archive can't be changed"),
        "making a folder says why not: {:?}",
        view.toasts
    );
    assert_eq!(shown(&app, &model).renaming, None);

    // Dropping files in asks nothing and copies nothing.
    let mut cmd = app.update(
        Event::DropItems {
            tab,
            paths: vec!["/y/new.txt".into()],
            target: None,
            copy: true,
            force_move: false,
        },
        &mut model,
    );
    assert!(
        cmd.effects()
            .all(|effect| !matches!(effect, Effect::FindConflicts(_) | Effect::RunFileOp(_)))
    );

    // The file manager, a terminal and the clipboard get no made-up path.
    for event in [
        Event::RevealSelection { tab },
        Event::OpenInTerminal { tab },
        Event::CopyPaths { tab },
        Event::GetInfo { tab },
    ] {
        let mut cmd = app.update(event.clone(), &mut model);
        assert!(
            cmd.effects()
                .all(|effect| !matches!(effect, Effect::System(_))),
            "{event:?}"
        );
    }

    // Searching stays in the folder shown, whatever the depth asked for.
    let _ = app.update(Event::SetSearchDepth { tab, depth: 3 }, &mut model);
    let mut cmd = search_text(&app, &mut model, tab, "read");
    assert!(walks(&mut cmd).is_empty());
}

/// The extraction `cmd` started: its archive, what it takes, where to, and
/// whether what comes out is kept together.
fn extraction(cmd: &mut Command<Effect, Event>) -> Option<(String, Vec<String>, String, bool)> {
    file_ops(cmd)
        .into_iter()
        .find_map(|run| match run.operation.op {
            crate::fileops::FileOp::Extract {
                archive,
                members,
                dst,
                own_folder,
                ..
            } => Some((
                archive.to_string_lossy().into_owned(),
                members,
                dst.to_string_lossy().into_owned(),
                own_folder,
            )),
            _ => None,
        })
}

#[test]
fn selected_archives_are_extracted_beside_them_or_where_asked() {
    let app = DirFleet;
    let mut model = Model::default();
    model.settings.queue_mode = QueueMode::Unlimited;
    let tab = window(&app, &mut model);
    let files = [entry("pack.zip", false), entry("notes.txt", false)];
    open(&app, &mut model, tab, "/x", &files);

    // Nothing to extract until an archive is selected.
    click(&app, &mut model, tab, 1, false, false);
    assert!(!shown(&app, &model).can_extract);
    let mut cmd = app.update(Event::ExtractHere { tab }, &mut model);
    assert_eq!(extraction(&mut cmd), None);

    let _ = app.update(Event::SelectAll { tab }, &mut model);
    assert!(shown(&app, &model).can_extract);
    let mut cmd = app.update(Event::ExtractHere { tab }, &mut model);
    assert_eq!(
        extraction(&mut cmd),
        Some(("/x/pack.zip".into(), Vec::new(), "/x".into(), true)),
        "the whole archive, kept together, beside it"
    );
    let operation = &app.view(&model).operations[0];
    assert_eq!(operation.description, "Extracting pack.zip to x");
    assert!(!operation.can_pause, "an extraction runs to its end");

    let mut cmd = app.update(
        Event::ExtractTo {
            tab,
            folder: "/out".into(),
        },
        &mut model,
    );
    assert_eq!(
        extraction(&mut cmd),
        Some(("/x/pack.zip".into(), Vec::new(), "/out".into(), true))
    );
}

#[test]
fn inside_an_archive_the_selection_or_the_folder_shown_is_extracted() {
    let app = DirFleet;
    let mut model = Model::default();
    model.settings.queue_mode = QueueMode::Unlimited;
    let tab = window(&app, &mut model);
    let docs = "archive:/x/pack.zip!/docs";
    open(
        &app,
        &mut model,
        tab,
        docs,
        &[entry("a.md", false), entry("sub", true)],
    );
    assert!(shown(&app, &model).can_extract);

    // Nothing selected: the folder shown, into the folder the archive is in.
    let mut cmd = app.update(Event::ExtractHere { tab }, &mut model);
    assert_eq!(
        extraction(&mut cmd),
        Some((
            "/x/pack.zip".into(),
            vec!["docs".into()],
            "/x".into(),
            false
        ))
    );
    click(&app, &mut model, tab, 0, false, false);
    let mut cmd = app.update(
        Event::ExtractTo {
            tab,
            folder: "/out".into(),
        },
        &mut model,
    );
    assert_eq!(
        extraction(&mut cmd),
        Some((
            "/x/pack.zip".into(),
            vec!["docs/a.md".into()],
            "/out".into(),
            false
        ))
    );

    // At the archive's top with nothing selected: all of it, kept together.
    open(
        &app,
        &mut model,
        tab,
        "archive:/x/pack.zip!/",
        &[entry("docs", true)],
    );
    let mut cmd = app.update(Event::ExtractHere { tab }, &mut model);
    assert_eq!(
        extraction(&mut cmd),
        Some(("/x/pack.zip".into(), Vec::new(), "/x".into(), true))
    );
}

#[test]
fn an_archive_that_lists_nothing_without_its_password_asks_for_it() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let inside = "archive:/x/pack.rar!/";
    let mut cmd = app.update(
        Event::Navigate {
            tab,
            path: inside.into(),
        },
        &mut model,
    );
    let answer = |cmd: &mut Command<Effect, Event>, model: &mut Model, wrong| {
        let mut list = cmd
            .effects()
            .find_map(|effect| match effect {
                Effect::ListDir(request) => Some(request),
                _ => None,
            })
            .expect("the archive is listed");
        list.resolve(ListDirOutput::NeedsPassword { wrong })
            .expect("resolves");
        let event = cmd.events().next().expect("the answer comes back");
        let _ = app.update(event, model);
    };
    answer(&mut cmd, &mut model, false);
    assert_eq!(
        app.view(&model).archive_password,
        Some(PasswordView {
            archive: "pack.rar".into(),
            wrong: false
        })
    );
    assert_eq!(
        shown(&app, &model).error.as_deref(),
        Some("This archive is password protected.")
    );

    // Given, it goes to where archives are read before the tab lists again.
    let submit = Event::SubmitArchivePassword {
        password: Secret::new("hunter2"),
    };
    assert!(
        !format!("{submit:?}").contains("hunter2"),
        "a password never shows in a log"
    );
    let mut cmd = app.update(submit, &mut model);
    let mut order = Vec::new();
    let mut list = None;
    for effect in cmd.effects() {
        match effect {
            Effect::SetArchivePassword(request) => {
                assert_eq!(request.operation.archive, "/x/pack.rar");
                assert_eq!(request.operation.password.reveal(), "hunter2");
                order.push("password");
            }
            Effect::ListDir(request) => {
                order.push("list");
                list = Some(request);
            }
            _ => {}
        }
    }
    assert_eq!(order, ["password", "list"]);
    assert_eq!(app.view(&model).archive_password, None);

    // It did not fit: asked again, saying so. Giving up asks no more.
    let mut list = list.expect("listed again");
    list.resolve(ListDirOutput::NeedsPassword { wrong: true })
        .expect("resolves");
    let event = cmd.events().next().expect("the answer comes back");
    let _ = app.update(event, &mut model);
    assert_eq!(
        app.view(&model).archive_password.map(|prompt| prompt.wrong),
        Some(true)
    );
    let _ = app.update(Event::CancelArchivePassword, &mut model);
    assert_eq!(app.view(&model).archive_password, None);
}

#[test]
fn opening_or_extracting_what_is_locked_asks_for_the_password_and_goes_on() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let inside = "archive:/x/pack.zip!/";
    open(&app, &mut model, tab, inside, &[entry("secret.txt", false)]);
    let submit = || Event::SubmitArchivePassword {
        password: Secret::new("hunter2"),
    };

    // A locked file is asked to be opened: the password, then the file again.
    let mut cmd = app.update(Event::OpenRow { tab, row: 0 }, &mut model);
    let mut copy = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::Materialize(request) => Some(request),
            _ => None,
        })
        .expect("a copy is asked for");
    copy.resolve(Materialized(Err(Unreadable::Password { wrong: false })))
        .expect("resolves");
    let event = cmd.events().next().expect("the answer comes back");
    let _ = app.update(event, &mut model);
    assert_eq!(
        app.view(&model).archive_password,
        Some(PasswordView {
            archive: "pack.zip".into(),
            wrong: false
        })
    );
    assert_eq!(app.view(&model).toasts, Vec::new(), "asking is no error");
    let mut cmd = app.update(submit(), &mut model);
    assert!(cmd.effects().any(|effect| matches!(
        effect,
        Effect::Materialize(request)
            if request.operation.location == "archive:/x/pack.zip!/secret.txt"
    )));

    // An extraction that stops for the password runs again once it is given.
    let mut cmd = app.update(Event::ExtractHere { tab }, &mut model);
    let mut run = file_ops(&mut cmd).pop().expect("the extraction runs");
    let op = run.operation.op.clone();
    let id = run.operation.id;
    run.resolve(FileOpUpdate(crate::fileops::FileOpEvent::Finished(
        crate::fileops::Outcome {
            errors: vec!["This archive is password protected.".into()],
            needs_password: Some(false),
            ..outcome(id, crate::fileops::Origin::User)
        },
    )))
    .expect("resolves");
    let event = cmd.events().next().expect("the outcome comes back");
    let _ = app.update(event, &mut model);
    let view = app.view(&model);
    assert!(view.archive_password.is_some());
    assert_eq!(view.toasts, Vec::new(), "the question says it all");
    let mut cmd = app.update(submit(), &mut model);
    let again = file_ops(&mut cmd).pop().expect("it runs again");
    assert_eq!(again.operation.op, op);
}

#[test]
fn the_selection_is_compressed_into_an_archive_named_in_a_dialog() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let files = [
        entry("report.pdf", false),
        entry("photos.2024", true),
        entry("taken.zip", false),
    ];
    open(&app, &mut model, tab, "/x", &files);
    let dialog = |model: &Model| shown(&app, model).compress;
    assert_eq!(dialog(&model), None);

    // One file: its name without its ending. A folder keeps its dots.
    click(&app, &mut model, tab, 0, false, false);
    let _ = app.update(Event::StartCompress { tab }, &mut model);
    let view = dialog(&model).expect("the dialog opens");
    assert_eq!((view.name.as_str(), view.count), ("report", 1));
    assert_eq!(view.format, ArchiveFormat::Zip);
    assert_eq!(view.file_name, "report.zip");
    assert_eq!(view.error, "");
    assert_eq!(view.formats.len(), 5);
    assert_eq!(view.formats[2].label, "Tar, gzip (.tar.gz)");
    let _ = app.update(Event::CancelCompress { tab }, &mut model);
    assert_eq!(dialog(&model), None);
    click(&app, &mut model, tab, 1, false, false);
    let _ = app.update(Event::StartCompress { tab }, &mut model);
    assert_eq!(dialog(&model).expect("open").name, "photos.2024");

    // Several items: the folder's name.
    let _ = app.update(Event::SelectAll { tab }, &mut model);
    let _ = app.update(Event::StartCompress { tab }, &mut model);
    let view = dialog(&model).expect("open");
    assert_eq!((view.name.as_str(), view.count), ("x", 3));

    // A name that is taken, or none, keeps the dialog open and says why.
    let set = |name: &str, format| Event::SetCompress {
        tab,
        name: name.into(),
        format,
    };
    let _ = app.update(set("taken", ArchiveFormat::Zip), &mut model);
    assert_eq!(
        dialog(&model).expect("open").error,
        "taken.zip already exists."
    );
    let mut cmd = app.update(Event::ApplyCompress { tab }, &mut model);
    assert!(file_ops(&mut cmd).is_empty());
    assert!(dialog(&model).is_some());
    let _ = app.update(set("  ", ArchiveFormat::Zip), &mut model);
    assert_eq!(
        dialog(&model).expect("open").error,
        "The archive needs a name."
    );

    let _ = app.update(set("My stuff", ArchiveFormat::TarGz), &mut model);
    let view = dialog(&model).expect("open");
    assert_eq!(view.file_name, "My stuff.tar.gz");
    assert_eq!(view.error, "");
    let mut cmd = app.update(Event::ApplyCompress { tab }, &mut model);
    let run = file_ops(&mut cmd).pop().expect("the archive is made");
    assert_eq!(
        run.operation.op,
        crate::fileops::FileOp::Compress {
            srcs: vec![
                PathBuf::from("/x/report.pdf"),
                PathBuf::from("/x/photos.2024"),
                PathBuf::from("/x/taken.zip"),
            ],
            dst: PathBuf::from("/x").join("My stuff.tar.gz"),
            format: ArchiveFormat::TarGz,
        }
    );
    assert_eq!(dialog(&model), None);
    let operation = &app.view(&model).operations[0];
    assert_eq!(
        operation.description,
        "Compressing 3 items to My stuff.tar.gz"
    );
    assert!(operation.can_pause);

    // Nothing is compressed from inside an archive.
    open(
        &app,
        &mut model,
        tab,
        "archive:/x/taken.zip!/",
        &[entry("a", false)],
    );
    click(&app, &mut model, tab, 0, false, false);
    let _ = app.update(Event::StartCompress { tab }, &mut model);
    assert_eq!(dialog(&model), None);
}

/// The connection `cmd` asks for.
fn connects(cmd: &mut Command<Effect, Event>) -> Vec<Request<Connect>> {
    cmd.effects()
        .filter_map(|effect| match effect {
            Effect::Connect(request) => Some(request),
            _ => None,
        })
        .collect()
}

/// Answers the connection `connect` asked for by `cmd` and feeds it back.
fn connected(
    app: &DirFleet,
    model: &mut Model,
    cmd: &mut Command<Effect, Event>,
    connect: &mut Request<Connect>,
    output: ConnectOutput,
) -> Command<Effect, Event> {
    connect.resolve(output).expect("resolves");
    let event = cmd.events().next().expect("the answer comes back");
    app.update(event, model)
}

fn sftp_fields(host: &str) -> ConnectFields {
    ConnectFields {
        name: "Work".into(),
        host: host.into(),
        user: "ann".into(),
        ..ConnectFields::default()
    }
}

#[test]
#[allow(clippy::too_many_lines, reason = "one story, told in order")]
fn a_server_is_connected_to_from_a_dialog_saved_and_shown_in_the_tab() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    assert_eq!(app.view(&model).connect, None);
    let _ = app.update(Event::OpenConnect { tab, edit: None }, &mut model);
    let dialog = app.view(&model).connect.expect("the dialog opens");
    assert_eq!(dialog.title, "Connect to Server");
    assert!(!dialog.editing && !dialog.busy);
    let labels: Vec<&str> = dialog
        .schemes
        .iter()
        .map(|choice| choice.label.as_str())
        .collect();
    assert_eq!(labels, ["SFTP", "FTP", "WebDAV", "S3"]);

    // What is missing keeps the dialog open and says so.
    let submit = |password: &str| Event::SubmitConnect {
        secret: Secret::new(password),
        remember: true,
    };
    let mut cmd = app.update(submit("hunter2"), &mut model);
    assert!(connects(&mut cmd).is_empty());
    assert_eq!(
        app.view(&model).connect.expect("open").error,
        "The server's name or address is needed."
    );
    let _ = app.update(
        Event::SetConnect {
            fields: ConnectFields {
                port: "ssh".into(),
                ..sftp_fields("example.org")
            },
        },
        &mut model,
    );
    let _ = app.update(submit("hunter2"), &mut model);
    assert_eq!(
        app.view(&model).connect.expect("open").error,
        "The port is a number up to 65535."
    );

    let _ = app.update(
        Event::SetConnect {
            fields: sftp_fields("example.org"),
        },
        &mut model,
    );
    let mut cmd = app.update(submit("hunter2"), &mut model);
    let mut connect = connects(&mut cmd).pop().expect("it connects");
    let asked = connect.operation.clone();
    assert_eq!(asked.connection.host, "example.org");
    assert_eq!(asked.connection.user, "ann");
    assert_eq!(asked.connection.host_key, None);
    assert_eq!(asked.secret.as_ref().map(Secret::reveal), Some("hunter2"));
    assert!(asked.remember);
    assert!(app.view(&model).connect.expect("open").busy);

    // A server never seen before shows its key: the user says it is right.
    let _ = connected(
        &app,
        &mut model,
        &mut cmd,
        &mut connect,
        ConnectOutput::UnknownHostKey {
            fingerprint: "SHA256:abc".into(),
        },
    );
    assert_eq!(
        app.view(&model).host_key,
        Some(HostKeyView {
            host: "example.org".into(),
            fingerprint: "SHA256:abc".into()
        })
    );
    let mut cmd = app.update(Event::AcceptHostKey { accept: true }, &mut model);
    let mut connect = connects(&mut cmd).pop().expect("tried again");
    assert_eq!(
        connect.operation.connection.host_key.as_deref(),
        Some("SHA256:abc")
    );
    assert_eq!(
        connect.operation.secret.as_ref().map(Secret::reveal),
        Some("hunter2"),
        "with what was typed"
    );
    assert_eq!(app.view(&model).host_key, None);

    // Connected: the dialog closes and the tab goes to the server's top.
    let id = connect.operation.connection.id.clone();
    let mut cmd = connected(
        &app,
        &mut model,
        &mut cmd,
        &mut connect,
        ConnectOutput::Connected,
    );
    let top = format!("sftp://{id}/");
    assert_eq!(listed_paths(&mut cmd), std::slice::from_ref(&top));
    let view = app.view(&model);
    assert_eq!(view.connect, None);
    let tab_view = shown(&app, &model);
    assert_eq!(
        tab_view.location.kind,
        crate::location::LocationKind::Remote
    );
    assert!(!tab_view.location.caps.real_paths);

    // It is saved without its password, and stands among the drives.
    let saved = DirFleet::persisted(&model);
    assert_eq!(saved.connections.len(), 1);
    assert_eq!(saved.connections[0].host_key.as_deref(), Some("SHA256:abc"));
    assert!(!format!("{saved:?}").contains("hunter2"));
    let server = view.windows[0]
        .sidebar
        .iter()
        .flat_map(|section| &section.items)
        .find(|item| item.path == top)
        .cloned()
        .expect("in the sidebar");
    assert_eq!(server.title, "Work");
    assert_eq!(server.eject.as_deref(), Some("Disconnect"));

    // Its "eject" closes the connection; it stays saved.
    let mut cmd = app.update(Event::EjectDrive { mount: top }, &mut model);
    assert!(cmd.effects().any(|effect| matches!(
        effect,
        Effect::Disconnect(request) if request.operation.id == id
    )));
    assert_eq!(DirFleet::persisted(&model).connections.len(), 1);
}

#[test]
#[allow(clippy::too_many_lines, reason = "one story, told in order")]
fn a_tab_whose_server_is_not_connected_connects_with_the_kept_password() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    model.remote.connections.push(Connection {
        id: "c7".into(),
        name: "NAS".into(),
        scheme: crate::location::Scheme::Ftp,
        host: "nas.local".into(),
        ..Connection::default()
    });
    let top = "ftp://c7/";
    let mut cmd = app.update(
        Event::Navigate {
            tab,
            path: top.into(),
        },
        &mut model,
    );
    let mut list = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::ListDir(request) => Some(request),
            _ => None,
        })
        .expect("listed");
    list.resolve(ListDirOutput::NeedsConnection {
        connection: "c7".into(),
    })
    .expect("resolves");
    let event = cmd.events().next().expect("the answer comes back");
    let mut cmd = app.update(event, &mut model);
    assert_eq!(shown(&app, &model).error.as_deref(), Some("Connecting…"));
    let mut connect = connects(&mut cmd).pop().expect("it connects by itself");
    assert_eq!(connect.operation.secret, None, "the keychain's password");
    assert_eq!(app.view(&model).connect, None, "no dialog yet");

    // The keychain has none that fits: the dialog asks, for that server.
    let _ = connected(
        &app,
        &mut model,
        &mut cmd,
        &mut connect,
        ConnectOutput::NeedsSecret,
    );
    let dialog = app.view(&model).connect.expect("the dialog opens");
    assert_eq!(dialog.title, "NAS");
    assert!(dialog.editing);
    assert_eq!(dialog.fields.host, "nas.local");
    assert_eq!(dialog.error, "The server wants the password.");

    // Typed: connected, and the tab that waited is read again.
    let mut cmd = app.update(
        Event::SubmitConnect {
            secret: Secret::new("pw"),
            remember: false,
        },
        &mut model,
    );
    let mut connect = connects(&mut cmd).pop().expect("it connects");
    assert_eq!(connect.operation.connection.id, "c7");
    let mut cmd = connected(
        &app,
        &mut model,
        &mut cmd,
        &mut connect,
        ConnectOutput::Connected,
    );
    assert_eq!(listed_paths(&mut cmd), [top]);
    assert_eq!(app.view(&model).connect, None);

    // A server that can't be reached says why on the tab; removing it
    // forgets its password too.
    let _ = app.update(
        Event::OpenConnect {
            tab,
            edit: Some("c7".into()),
        },
        &mut model,
    );
    let mut cmd = app.update(
        Event::SubmitConnect {
            secret: Secret::new(""),
            remember: false,
        },
        &mut model,
    );
    let mut connect = connects(&mut cmd).pop().expect("it connects");
    assert_eq!(
        connect.operation.secret, None,
        "nothing typed: the keychain's"
    );
    let _ = connected(
        &app,
        &mut model,
        &mut cmd,
        &mut connect,
        ConnectOutput::Failed("No route to nas.local".into()),
    );
    assert_eq!(
        app.view(&model).connect.expect("still open").error,
        "No route to nas.local"
    );
    let mut cmd = app.update(Event::RemoveConnection { id: "c7".into() }, &mut model);
    let mut forgot = false;
    for effect in cmd.effects() {
        forgot |= matches!(effect, Effect::ForgetSecret(request) if request.operation.id == "c7");
    }
    assert!(forgot);
    assert_eq!(app.view(&model).connect, None);
    assert_eq!(DirFleet::persisted(&model).connections.len(), 0);
}

#[test]
fn a_new_folder_is_made_selected_and_renamed_once_listed() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("a", false)]);
    let mut cmd = app.update(Event::NewFolder { tab }, &mut model);
    let mut run = file_ops(&mut cmd).pop().expect("a folder is made");
    assert_eq!(
        run.operation.op,
        crate::fileops::FileOp::NewFolder {
            parent: PathBuf::from("/x")
        }
    );
    run.resolve(FileOpUpdate(crate::fileops::FileOpEvent::Progress {
        id: run.operation.id,
        fraction: 0.5,
        current: "untitled folder".into(),
        bytes_done: 0,
        bytes_total: 0,
        bytes_per_second: 0,
    }))
    .expect("progress");
    let event = cmd.events().next().expect("progress comes back");
    let _ = app.update(event, &mut model);
    let operations = app.view(&model).operations;
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].progress, 500);

    let made = Path::new("/x").join("untitled folder");
    run.resolve(FileOpUpdate(crate::fileops::FileOpEvent::Finished(
        crate::fileops::Outcome {
            id: run.operation.id,
            created: vec![made.clone()],
            undo: Some(crate::fileops::Undo::Created(vec![made.clone()])),
            ..outcome(0, crate::fileops::Origin::User)
        },
    )))
    .expect("finished");
    let event = cmd.events().next().expect("the outcome comes back");
    let mut cmd = app.update(event, &mut model);
    assert_eq!(app.view(&model).operations, Vec::new());
    assert_eq!(app.view(&model).undo.as_deref(), Some("creation of 1 item"));
    let mut list = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::ListDir(request) => Some(request),
            _ => None,
        })
        .expect("the folder is read again");
    let view = list.operation.view.clone();
    let mut folder = DirEntry::test_new(&made.to_string_lossy());
    folder.meta.entry_type = EntryType::Directory;
    list.resolve(listing(&[folder, entry("a", false)], view))
        .expect("listing");
    let event = cmd.events().next().expect("the listing comes back");
    let _ = app.update(event, &mut model);
    let shown = shown(&app, &model);
    assert_eq!(shown.selection, vec![RowRange { start: 0, end: 1 }]);
    assert_eq!(
        shown.renaming,
        Some(RenameView {
            row: 0,
            name: "untitled folder".into(),
            error: None
        })
    );
}

#[test]
fn renaming_checks_the_name_and_undo_and_redo_run_the_inverse() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("a.txt", false)]);
    click(&app, &mut model, tab, 0, false, false);
    let _ = app.update(Event::StartRename { tab }, &mut model);
    assert_eq!(
        shown(&app, &model).renaming.map(|r| r.name),
        Some("a.txt".into())
    );

    let _ = app.update(
        Event::Rename {
            tab,
            name: "a/b".into(),
        },
        &mut model,
    );
    assert!(shown(&app, &model).renaming.and_then(|r| r.error).is_some());

    let mut cmd = app.update(
        Event::Rename {
            tab,
            name: "b.txt".into(),
        },
        &mut model,
    );
    let (from, to) = (Path::new("/x").join("a.txt"), Path::new("/x").join("b.txt"));
    assert_eq!(
        file_ops(&mut cmd)[0].operation.op,
        crate::fileops::FileOp::Rename {
            from: from.clone(),
            to: to.clone()
        }
    );
    let mut cmd = app.update(
        Event::Rename {
            tab,
            name: "b.txt".into(),
        },
        &mut model,
    );
    assert!(file_ops(&mut cmd).is_empty(), "the rename already ended");

    let _ = app.update(Event::StartRename { tab }, &mut model);
    let mut cmd = app.update(
        Event::Rename {
            tab,
            name: "b.txt".into(),
        },
        &mut model,
    );
    let undo = crate::fileops::Undo::Rename { from: to, to: from };
    let _ = finish(
        &app,
        &mut model,
        &mut cmd,
        crate::fileops::Outcome {
            undo: Some(undo.clone()),
            ..outcome(0, crate::fileops::Origin::User)
        },
    );
    assert_eq!(app.view(&model).undo.as_deref(), Some("rename"));
    assert_eq!(app.view(&model).redo, None);

    let mut cmd = app.update(Event::Undo, &mut model);
    let run = &file_ops(&mut cmd)[0].operation;
    assert_eq!(run.op, crate::fileops::FileOp::Undo(undo));
    assert_eq!(run.origin, crate::fileops::Origin::Undo);
    let mut cmd = app.update(Event::Undo, &mut model);
    assert!(file_ops(&mut cmd).is_empty(), "nothing left to undo");
}

#[test]
fn trash_delete_and_duplicate_act_on_the_selection_but_not_in_the_trash() {
    let app = DirFleet;
    let mut model = Model::default();
    // Each operation here starts at once, whatever still runs.
    model.settings.queue_mode = QueueMode::Unlimited;
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[entry("a", false), entry("b", false)],
    );
    let _ = app.update(Event::SelectAll { tab }, &mut model);
    let both = vec![Path::new("/x").join("a"), Path::new("/x").join("b")];
    for (event, op) in [
        (
            Event::TrashSelection { tab },
            crate::fileops::FileOp::Trash(both.clone()),
        ),
        (
            Event::DeleteSelection { tab },
            crate::fileops::FileOp::Delete(both.clone()),
        ),
        (
            Event::Duplicate { tab },
            crate::fileops::FileOp::Duplicate(both),
        ),
    ] {
        let mut cmd = app.update(event, &mut model);
        assert_eq!(file_ops(&mut cmd)[0].operation.op, op);
    }
    let id = app.view(&model).operations[0].id;
    let mut cmd = app.update(Event::CancelOperation { id }, &mut model);
    assert!(
        cmd.effects()
            .any(|e| matches!(e, Effect::CancelFileOp(r) if r.operation.id == id))
    );

    open_trash(&app, &mut model, tab, &[entry("gone", false)]);
    let _ = app.update(Event::SelectAll { tab }, &mut model);
    let mut cmd = app.update(Event::TrashSelection { tab }, &mut model);
    assert!(file_ops(&mut cmd).is_empty());
}

#[test]
fn copying_asks_how_clashing_names_go_and_copies_into_the_same_folder_duplicate() {
    let app = DirFleet;
    let mut model = Model::default();
    // Each operation here starts at once, whatever still runs.
    model.settings.queue_mode = QueueMode::Unlimited;
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("a", false)]);
    click(&app, &mut model, tab, 0, false, false);

    let mut cmd = app.update(
        Event::CopyTo {
            tab,
            folder: "/x".into(),
        },
        &mut model,
    );
    assert!(matches!(
        &file_ops(&mut cmd)[0].operation.op,
        crate::fileops::FileOp::Duplicate(_)
    ));

    let mut cmd = app.update(
        Event::CopyTo {
            tab,
            folder: "/y".into(),
        },
        &mut model,
    );
    let mut check = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::FindConflicts(request) => Some(request),
            _ => None,
        })
        .expect("names are checked");
    check
        .resolve(Conflicts(vec!["a".into()]))
        .expect("resolves");
    let event = cmd.events().next().expect("the clashes come back");
    let _ = app.update(event, &mut model);
    assert_eq!(
        app.view(&model).conflict,
        Some(ConflictView {
            count: 1,
            name: "a".into(),
            folder: "/y".into(),
            copy: true
        })
    );
    let mut cmd = app.update(
        Event::ResolveConflict {
            choice: ConflictChoice::Replace,
        },
        &mut model,
    );
    assert_eq!(
        file_ops(&mut cmd)[0].operation.op,
        crate::fileops::FileOp::Copy {
            srcs: vec![Path::new("/x").join("a")],
            dst: PathBuf::from("/y"),
            policy: crate::fileops::ConflictPolicy::Replace
        }
    );
    assert_eq!(app.view(&model).conflict, None);
}

/// The clipboard count a cut asks for, answered with `count`.
fn count_clipboard(app: &DirFleet, model: &mut Model, mut cmd: Command<Effect, Event>, count: u64) {
    let mut request = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::ClipboardChanges(request) => Some(request),
            _ => None,
        })
        .expect("the clipboard's changes are counted");
    request.resolve(count).expect("resolves");
    let event = cmd.events().next().expect("the count comes back");
    let _ = app.update(event, model);
}

#[test]
fn coming_back_to_a_window_reads_folders_sorted_by_opening_and_forgets_a_replaced_cut() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("a", false)]);
    let other = window(&app, &mut model);
    open(&app, &mut model, other, "/y", &[entry("b", false)]);
    let windows: Vec<u32> = app.view(&model).windows.iter().map(|w| w.id).collect();
    let _ = app.update(
        Event::SortBy {
            tab,
            sort: Sort::Accessed,
        },
        &mut model,
    );
    let focus = |model: &mut Model, window: u32| app.update(Event::WindowFocused { window }, model);
    assert_eq!(
        listed_paths(&mut focus(&mut model, windows[0])),
        ["/x"],
        "items may have been opened in other apps meanwhile"
    );
    assert!(
        listed_paths(&mut focus(&mut model, windows[1])).is_empty(),
        "a folder sorted by name stays as it is"
    );

    click(&app, &mut model, tab, 0, false, false);
    let mut cmd = app.update(Event::CutSelection { tab }, &mut model);
    let mut write = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::Clipboard(request) => Some(request),
            _ => None,
        })
        .expect("the clipboard is written");
    write.resolve(ClipboardDone(Ok(None))).expect("resolves");
    let event = cmd.events().next().expect("the write comes back");
    let cmd = app.update(event, &mut model);
    count_clipboard(&app, &mut model, cmd, 7);
    assert!(shown(&app, &model).rows[0].cut);

    let cmd = focus(&mut model, windows[1]);
    count_clipboard(&app, &mut model, cmd, 7);
    assert!(
        shown(&app, &model).rows[0].cut,
        "the clipboard still has them"
    );
    let cmd = focus(&mut model, windows[1]);
    count_clipboard(&app, &mut model, cmd, 8);
    assert!(
        !shown(&app, &model).rows[0].cut,
        "another app replaced them on the clipboard"
    );
    assert!(
        !focus(&mut model, windows[1])
            .effects()
            .any(|effect| matches!(effect, Effect::ClipboardChanges(_))),
        "nothing is cut, so nothing is checked"
    );
}

#[test]
fn cut_items_are_dimmed_and_pasting_moves_them() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[entry("a", false), entry("b", false)],
    );
    click(&app, &mut model, tab, 0, false, false);
    let mut cmd = app.update(Event::CutSelection { tab }, &mut model);
    let written = cmd.effects().find_map(|effect| match effect {
        Effect::Clipboard(request) => Some(request.operation),
        _ => None,
    });
    assert_eq!(
        written,
        Some(ClipboardOp::Write {
            paths: vec![Path::new("/x").join("a")],
            cut: true
        })
    );
    let rows = shown(&app, &model).rows;
    assert!(rows[0].cut && !rows[1].cut);

    let other = window(&app, &mut model);
    open(&app, &mut model, other, "/y", &[]);
    let mut paste = app.update(Event::Paste { tab: other }, &mut model);
    let mut read = paste
        .effects()
        .find_map(|effect| match effect {
            Effect::Clipboard(request) => Some(request),
            _ => None,
        })
        .expect("the clipboard is read");
    assert_eq!(read.operation, ClipboardOp::Read);
    read.resolve(ClipboardDone(Ok(Some((
        vec![Path::new("/x").join("a")],
        true,
    )))))
    .expect("resolves");
    let event = paste.events().next().expect("the files come back");
    let mut cmd = app.update(event, &mut model);
    let check = cmd.effects().find_map(|effect| match effect {
        Effect::FindConflicts(request) => Some(request.operation),
        _ => None,
    });
    assert_eq!(
        check,
        Some(FindConflicts {
            srcs: vec![Path::new("/x").join("a")],
            dst: PathBuf::from("/y")
        })
    );
    assert!(
        !shown(&app, &model).rows[0].cut,
        "pasted cut items are no longer dimmed"
    );
    assert_eq!(app.view(&model).conflict, None);
}

fn child_dirs(cmd: &mut Command<Effect, Event>) -> Option<Request<ChildDirs>> {
    cmd.effects().find_map(|effect| match effect {
        Effect::ChildDirs(request) => Some(request),
        _ => None,
    })
}

/// Answers the `ChildDirs` in `cmd` and feeds the result back.
#[cfg_attr(windows, allow(dead_code))]
fn answer_dirs(
    app: &DirFleet,
    model: &mut Model,
    cmd: &mut Command<Effect, Event>,
    names: Result<Vec<&str>, &str>,
) -> Command<Effect, Event> {
    let mut request = child_dirs(cmd).expect("subfolders are read");
    request
        .resolve(ChildDirsOutput(
            names
                .map(|names| names.into_iter().map(Into::into).collect())
                .map_err(Into::into),
        ))
        .expect("resolves");
    let event = cmd.events().next().expect("they come back");
    app.update(event, model)
}

#[test]
#[cfg(not(windows))]
fn the_path_bar_shows_crumbs_and_a_crumb_s_subfolders() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = start(
        &app,
        &mut model,
        Some("/home/docs/2026"),
        LoadedState::Missing,
    );
    let tab = shown(&app, &model).id;
    let view = shown(&app, &model);
    let titles: Vec<_> = view.crumbs.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(titles, ["Home", "docs", "2026"]);
    assert_eq!(view.crumbs[0].anchor, Some(Anchor::Home));
    assert_eq!(view.crumbs_above.len(), 1);

    let mut cmd = app.update(
        Event::ListChildren {
            tab,
            path: "/home/docs".into(),
        },
        &mut model,
    );
    let _ = answer_dirs(&app, &mut model, &mut cmd, Ok(vec!["2025", "2026"]));
    assert_eq!(
        shown(&app, &model).children,
        Some(ChildrenView {
            path: "/home/docs".into(),
            names: vec!["2025".into(), "2026".into()]
        })
    );
}

#[test]
#[cfg(not(windows))]
fn go_to_folder_completes_names_and_reports_missing_folders() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = start(&app, &mut model, Some("/home"), LoadedState::Missing);
    let tab = shown(&app, &model).id;
    let mut cmd = app.update(Event::OpenGoTo { tab }, &mut model);
    let _ = answer_dirs(
        &app,
        &mut model,
        &mut cmd,
        Ok(vec!["Desktop", "Documents", "Downloads"]),
    );
    assert_eq!(shown(&app, &model).goto.expect("open").matches.len(), 3);

    let mut cmd = app.update(
        Event::GoToTyped {
            tab,
            text: "~/do".into(),
        },
        &mut model,
    );
    assert!(child_dirs(&mut cmd).is_none(), "/home/ is listed already");
    let matches: Vec<_> = shown(&app, &model)
        .goto
        .expect("open")
        .matches
        .into_iter()
        .map(|found| found.title)
        .collect();
    assert_eq!(&matches[..2], ["Documents", "Downloads"]);

    let mut cmd = app.update(
        Event::GoToTyped {
            tab,
            text: "/nope".into(),
        },
        &mut model,
    );
    drop(child_dirs(&mut cmd));
    let mut cmd = app.update(Event::GoToSubmit { tab, path: None }, &mut model);
    let _ = answer_dirs(&app, &mut model, &mut cmd, Err("no such folder"));
    assert_eq!(
        shown(&app, &model).goto.and_then(|goto| goto.error),
        Some("/nope does not exist".into())
    );

    let mut cmd = app.update(
        Event::GoToSubmit {
            tab,
            path: Some("/home/Documents".into()),
        },
        &mut model,
    );
    let mut next = answer_dirs(&app, &mut model, &mut cmd, Ok(vec![]));
    assert_eq!(
        next.events().next(),
        Some(Event::Navigate {
            tab,
            path: "/home/Documents".into()
        })
    );
    assert_eq!(shown(&app, &model).goto, None);
}

#[test]
fn a_bare_name_goes_to_a_matching_favorite() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let _ = app.update(
        Event::AddFavorite {
            path: "/data/Work".into(),
        },
        &mut model,
    );
    let mut cmd = app.update(Event::OpenGoTo { tab }, &mut model);
    assert!(child_dirs(&mut cmd).is_none(), "an empty tab lists nothing");
    let _ = app.update(
        Event::GoToTyped {
            tab,
            text: "wo".into(),
        },
        &mut model,
    );
    let mut cmd = app.update(Event::GoToSubmit { tab, path: None }, &mut model);
    let check = child_dirs(&mut cmd).expect("the match is checked");
    assert_eq!(check.operation.path, "/data/Work");
}

#[test]
fn finished_operations_and_drives_coming_and_going_show_toasts_that_expire() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("a", false)]);
    click(&app, &mut model, tab, 0, false, false);
    let mut cmd = app.update(Event::Duplicate { tab }, &mut model);
    let mut done = finish(
        &app,
        &mut model,
        &mut cmd,
        outcome(0, crate::fileops::Origin::User),
    );
    let toasts = app.view(&model).toasts;
    assert_eq!(toasts[0].text, "Duplicated 1 item");
    assert_eq!(toasts[0].kind, ToastKind::Success);
    let mut delay = done
        .effects()
        .find_map(|effect| match effect {
            Effect::Delay(request) => Some(request),
            _ => None,
        })
        .expect("the toast expires");
    delay.resolve(()).expect("resolves");
    let event = done
        .events()
        .find(|e| matches!(e, Event::DismissToast { .. }))
        .expect("dismissed");
    let _ = app.update(event, &mut model);
    assert_eq!(app.view(&model).toasts, Vec::new());

    let usb = crate::drives::test_drive("/Volumes/USB", "USB", crate::drives::DriveKind::Removable);
    let disk = crate::drives::test_drive("/", "Macintosh HD", crate::drives::DriveKind::Fixed);
    let _ = app.update(
        Event::DrivesChanged {
            drives: DriveList(vec![disk.clone()]),
        },
        &mut model,
    );
    assert_eq!(
        app.view(&model).toasts,
        Vec::new(),
        "the first list is no news"
    );
    let _ = app.update(
        Event::DrivesChanged {
            drives: DriveList(vec![disk.clone(), usb]),
        },
        &mut model,
    );
    let _ = app.update(
        Event::DrivesChanged {
            drives: DriveList(vec![disk]),
        },
        &mut model,
    );
    let texts: Vec<_> = app
        .view(&model)
        .toasts
        .into_iter()
        .map(|toast| toast.text)
        .collect();
    assert_eq!(texts, ["USB connected", "USB was disconnected"]);
}

#[test]
fn the_inspector_describes_one_item_and_adds_up_a_folder_on_request() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[entry("sub", true), entry("a.txt", false)],
    );
    click(&app, &mut model, tab, 1, false, false);
    assert_eq!(shown(&app, &model).inspector, None, "hidden until shown");

    let _ = app.update(Event::SetInspector { shown: true }, &mut model);
    let item = shown(&app, &model)
        .inspector
        .and_then(|i| i.item)
        .expect("one item");
    assert_eq!(item.name, "a.txt");
    assert_eq!(item.size, SizeInfo::Known("10 B".into()));
    assert!(!item.is_dir);

    click(&app, &mut model, tab, 0, false, false);
    let item = shown(&app, &model)
        .inspector
        .and_then(|i| i.item)
        .expect("one item");
    assert_eq!(item.size, SizeInfo::NotCalculated);
    let mut cmd = app.update(Event::CalculateSize { path: item.path }, &mut model);
    let mut size = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::FolderSize(request) => Some(request),
            _ => None,
        })
        .expect("the folder is added up");
    assert_eq!(
        shown(&app, &model)
            .inspector
            .and_then(|i| i.item)
            .map(|i| i.size),
        Some(SizeInfo::Calculating)
    );
    size.resolve(Some(2_500_000)).expect("resolves");
    let event = cmd.events().next().expect("the size comes back");
    let _ = app.update(event, &mut model);
    assert_eq!(
        shown(&app, &model)
            .inspector
            .and_then(|i| i.item)
            .map(|i| i.size),
        Some(SizeInfo::Known("2.5 MB".into()))
    );
}

#[test]
fn the_inspector_sums_up_several_items_and_its_state_is_kept() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = start(&app, &mut model, Some("/x"), LoadedState::Missing);
    let tab = shown(&app, &model).id;
    let mut cmd = app.update(Event::SetInspector { shown: true }, &mut model);
    let saved = cmd.effects().find_map(|effect| match effect {
        Effect::SaveState(request) => Some(request.operation.state.inspector_shown),
        _ => None,
    });
    assert_eq!(saved, Some(true));
    let mut list = app.update(Event::Refresh { tab }, &mut model);
    let mut request = list
        .effects()
        .find_map(|effect| match effect {
            Effect::ListDir(request) => Some(request),
            _ => None,
        })
        .expect("listed");
    let view = request.operation.view.clone();
    request
        .resolve(listing(
            &[
                entry("photos", true),
                entry("a.jpg", false),
                entry("b.png", false),
                entry("c.txt", false),
            ],
            view,
        ))
        .expect("resolves");
    let event = list.events().next().expect("listing");
    let _ = app.update(event, &mut model);
    let _ = app.update(Event::SelectAll { tab }, &mut model);
    let summary = shown(&app, &model)
        .inspector
        .and_then(|i| i.summary)
        .expect("several");
    assert_eq!((summary.count, summary.folders, summary.files), (4, 1, 3));
    assert_eq!(summary.total, "30 B");
    let kinds: Vec<_> = summary
        .kinds
        .iter()
        .map(|k| (k.kind.as_str(), k.count))
        .collect();
    assert_eq!(kinds, [("Document", 1), ("Folder", 1), ("Image", 2)]);
}

#[test]
fn settings_change_the_sidebar_and_terminal_and_are_kept() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = start(&app, &mut model, Some("/x"), LoadedState::Missing);
    let tab = shown(&app, &model).id;
    open(&app, &mut model, tab, "/y", &[]);
    let usb = crate::drives::test_drive("/Volumes/USB", "USB", crate::drives::DriveKind::Removable);
    let _ = app.update(
        Event::DrivesChanged {
            drives: DriveList(vec![usb]),
        },
        &mut model,
    );
    assert!(
        section(&app, &model, SidebarSection::Drives)[0]
            .used_permille
            .is_some()
    );

    let settings = Settings {
        terminal: "iTerm".into(),
        show_recent: false,
        show_trash: false,
        show_drive_capacity: false,
        notify_drive_changes: false,
        ..app.view(&model).settings
    };
    let mut cmd = app.update(
        Event::ChangeSettings {
            settings: settings.clone(),
        },
        &mut model,
    );
    let saved = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::SaveState(request) => Some(request.operation.state),
            _ => None,
        })
        .expect("saved");
    assert_eq!(saved.settings, settings);
    assert_eq!(section(&app, &model, SidebarSection::Recent), Vec::new());
    assert!(
        section(&app, &model, SidebarSection::Locations)
            .iter()
            .all(|item| !item.trash)
    );
    assert_eq!(
        section(&app, &model, SidebarSection::Drives)[0].used_permille,
        None
    );
    let _ = app.update(
        Event::DrivesChanged {
            drives: DriveList(vec![]),
        },
        &mut model,
    );
    assert_eq!(app.view(&model).toasts, Vec::new(), "drive news is off");

    let mut cmd = app.update(Event::OpenInTerminal { tab }, &mut model);
    assert_eq!(
        system_ops(&mut cmd),
        vec![SystemOp::Terminal {
            folder: "/y".into(),
            app: "iTerm".into()
        }]
    );

    let mut restored = Model::default();
    let _ = start(&app, &mut restored, None, LoadedState::Found(saved));
    assert_eq!(app.view(&restored).settings, settings);
    let _ = app.update(Event::RestoreDefaultSettings, &mut restored);
    assert_eq!(app.view(&restored).settings, Settings::default());
}

#[test]
fn folder_defaults_apply_to_folders_without_a_view_of_their_own() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let view = FolderView {
        display: DisplayType::Icons,
        show_hidden: true,
        ..FolderView::default()
    };
    let _ = app.update(Event::SetFolderDefaults { view }, &mut model);
    assert_eq!(app.view(&model).folder_defaults, view);
    open(&app, &mut model, tab, "/new", &[]);
    assert_eq!(shown(&app, &model).view, view);
}

#[test]
fn a_search_is_saved_updated_reverted_and_run_from_the_sidebar() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("report.pdf", false)]);
    let _ = search_text(&app, &mut model, tab, "report");
    let _ = app.update(Event::CommitSearchText { tab }, &mut model);
    let _ = app.update(
        Event::SaveSearch {
            tab,
            name: "Reports".into(),
        },
        &mut model,
    );
    let saved = app.view(&model).saved_searches;
    assert_eq!(saved.len(), 1);
    assert_eq!(
        (saved[0].name.as_str(), saved[0].text.as_str()),
        ("Reports", "report")
    );
    let badge = shown(&app, &model).saved.expect("from a saved search");
    assert!(!badge.modified);
    let row = section(&app, &model, SidebarSection::SavedSearches);
    assert_eq!(
        (row[0].title.as_str(), row[0].saved_search, row[0].active),
        ("Reports", Some(saved[0].id), true)
    );

    let _ = search_text(&app, &mut model, tab, "pdf");
    assert!(shown(&app, &model).saved.expect("still from it").modified);
    let _ = app.update(Event::RevertSearch { tab }, &mut model);
    let search = shown(&app, &model).search.expect("open");
    assert_eq!(
        (search.text.as_str(), search.chips[0].token.as_str()),
        ("", "report")
    );
    assert!(!shown(&app, &model).saved.expect("from it").modified);

    let _ = app.update(
        Event::ToggleSearchFilter {
            tab,
            token: "ext:pdf".into(),
        },
        &mut model,
    );
    let _ = app.update(Event::UpdateSavedSearch { tab }, &mut model);
    assert_eq!(app.view(&model).saved_searches[0].text, "report ext:pdf");

    let _ = app.update(Event::CloseSearch { tab }, &mut model);
    let id = saved[0].id;
    let _ = app.update(Event::RunSavedSearch { tab, id }, &mut model);
    let search = shown(&app, &model).search.expect("run");
    let tokens: Vec<_> = search
        .chips
        .iter()
        .map(|chip| chip.token.as_str())
        .collect();
    assert_eq!(tokens, ["report", "ext:pdf"]);
    assert_eq!(shown(&app, &model).saved.map(|badge| badge.id), Some(id));
    assert!(shown(&app, &model).nav.back, "running it is a history step");
}

#[test]
fn saved_searches_are_edited_duplicated_moved_deleted_and_kept() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = start(&app, &mut model, Some("/x"), LoadedState::Missing);
    let tab = shown(&app, &model).id;
    let _ = search_text(&app, &mut model, tab, "a");
    let _ = app.update(
        Event::SaveSearch {
            tab,
            name: "A".into(),
        },
        &mut model,
    );
    let id = app.view(&model).saved_searches[0].id;
    let _ = app.update(Event::DuplicateSavedSearch { id }, &mut model);
    let names: Vec<_> = app
        .view(&model)
        .saved_searches
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["A", "A (2)"]);

    let _ = app.update(
        Event::EditSavedSearch {
            id,
            name: "Photos".into(),
            text: "kind:image".into(),
            case_sensitive: true,
            depth: 3,
            any: false,
            favorites: true,
        },
        &mut model,
    );
    let _ = app.update(Event::MoveSavedSearch { from: 0, to: 1 }, &mut model);
    let saved = app.view(&model).saved_searches;
    assert_eq!(saved[1].name, "Photos");
    assert_eq!(
        (saved[1].text.as_str(), saved[1].depth, saved[1].favorites),
        ("kind:image", 3, true)
    );

    let mut cmd = app.update(Event::DeleteSavedSearch { id: saved[0].id }, &mut model);
    let state = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::SaveState(request) => Some(request.operation.state),
            _ => None,
        })
        .expect("saved");
    let mut restored = Model::default();
    let _ = start(&app, &mut restored, None, LoadedState::Found(state));
    let names: Vec<_> = app
        .view(&restored)
        .saved_searches
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["Photos"]);
}

fn history_ops(cmd: &mut Command<Effect, Event>) -> Vec<OpenHistoryOp> {
    cmd.effects()
        .filter_map(|effect| match effect {
            Effect::OpenHistory(request) => Some(request.operation),
            _ => None,
        })
        .collect()
}

#[test]
fn opened_files_are_counted_shown_and_forgotten() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let mut cmd = app.update(
        Event::Navigate {
            tab,
            path: "/x".into(),
        },
        &mut model,
    );
    let mut list = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::ListDir(request) => Some(request),
            _ => None,
        })
        .expect("listed");
    let view = list.operation.view.clone();
    let entries = [entry("a.txt", false), entry("b.txt", false)];
    list.resolve(ListDirOutput::Listed {
        listing: Arc::new(Listing::from_entries(&entries)),
        order: vec![0, 1],
        view,
        opened: Arc::new(vec![(3, 1_762_610_580), (0, 0)]),
    })
    .expect("resolves");
    let event = cmd.events().next().expect("listing");
    let _ = app.update(event, &mut model);
    let _ = app.update(
        Event::ToggleColumn {
            column: Column::OpenCount,
        },
        &mut model,
    );
    let view = app.view(&model);
    let opens = view
        .columns
        .iter()
        .skip(1)
        .position(|c| c.column == Column::OpenCount)
        .expect("shown");
    let rows = shown(&app, &model).rows;
    assert_eq!(rows[0].cells[opens], "3");
    assert_eq!(rows[1].cells[opens], "");

    let _ = app.update(Event::SetInspector { shown: true }, &mut model);
    click(&app, &mut model, tab, 0, false, false);
    let item = shown(&app, &model)
        .inspector
        .and_then(|i| i.item)
        .expect("one item");
    assert_eq!((item.opens, item.opened.as_str()), (3, "2025-11-08 14:03"));

    let mut cmd = app.update(Event::OpenRow { tab, row: 1 }, &mut model);
    assert_eq!(
        history_ops(&mut cmd),
        vec![OpenHistoryOp::Record(vec![path("/x/b.txt")])]
    );
    assert!(
        cmd.events().any(|e| e == Event::Refresh { tab }),
        "the counts are read again"
    );

    let mut cmd = app.update(Event::ForgetOpened { tab }, &mut model);
    assert_eq!(
        history_ops(&mut cmd),
        vec![OpenHistoryOp::Forget(vec![path("/x/a.txt")])]
    );
    let mut cmd = app.update(Event::ClearOpenHistory, &mut model);
    assert_eq!(history_ops(&mut cmd), vec![OpenHistoryOp::Clear]);

    let settings = Settings {
        track_open_history: false,
        ..app.view(&model).settings
    };
    let _ = app.update(Event::ChangeSettings { settings }, &mut model);
    let mut cmd = app.update(Event::OpenRow { tab, row: 1 }, &mut model);
    assert_eq!(
        history_ops(&mut cmd),
        Vec::new(),
        "not counted when turned off"
    );
}

#[cfg_attr(windows, allow(dead_code))]
fn drop_items(
    app: &DirFleet,
    model: &mut Model,
    tab: u32,
    paths: &[&str],
    target: Option<&str>,
    copy: bool,
    force_move: bool,
) -> Command<Effect, Event> {
    app.update(
        Event::DropItems {
            tab,
            paths: paths.iter().map(|p| (*p).to_owned()).collect(),
            target: target.map(Into::into),
            copy,
            force_move,
        },
        model,
    )
}

#[cfg_attr(windows, allow(dead_code))]
fn conflict_check(cmd: &mut Command<Effect, Event>) -> Option<FindConflicts> {
    cmd.effects().find_map(|effect| match effect {
        Effect::FindConflicts(request) => Some(request.operation),
        _ => None,
    })
}

#[test]
#[cfg(not(windows))]
fn drops_move_within_a_drive_copy_across_and_keys_decide() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/Users/ann/Projects", &[]);
    let drives = DriveList(vec![
        crate::drives::test_drive("/", "Macintosh HD", crate::drives::DriveKind::Fixed),
        crate::drives::test_drive("/Volumes/USB", "USB", crate::drives::DriveKind::Removable),
    ]);
    let _ = app.update(Event::DrivesChanged { drives }, &mut model);
    let answer = |app: &DirFleet, model: &mut Model, mut cmd: Command<Effect, Event>| {
        let mut check = cmd
            .effects()
            .find_map(|effect| match effect {
                Effect::FindConflicts(request) => Some(request),
                _ => None,
            })
            .expect("names are checked");
        check.resolve(Conflicts(Vec::new())).expect("resolves");
        let event = cmd.events().next().expect("no clashes");
        let mut run = app.update(event, model);
        file_ops(&mut run).pop().expect("runs").operation.op
    };
    let dropped = drop_items(
        &app,
        &mut model,
        tab,
        &["/Users/ann/a.txt"],
        None,
        false,
        false,
    );
    let op = answer(&app, &mut model, dropped);
    assert!(
        matches!(op, crate::fileops::FileOp::Move { .. }),
        "same drive moves: {op:?}"
    );
    let dropped = drop_items(
        &app,
        &mut model,
        tab,
        &["/Volumes/USB/a.txt"],
        None,
        false,
        false,
    );
    let op = answer(&app, &mut model, dropped);
    assert!(
        matches!(op, crate::fileops::FileOp::Copy { .. }),
        "another drive copies: {op:?}"
    );
    let dropped = drop_items(
        &app,
        &mut model,
        tab,
        &["/Users/ann/a.txt"],
        None,
        true,
        false,
    );
    let op = answer(&app, &mut model, dropped);
    assert!(
        matches!(op, crate::fileops::FileOp::Copy { .. }),
        "the copy key copies: {op:?}"
    );
    let dropped = drop_items(
        &app,
        &mut model,
        tab,
        &["/Volumes/USB/a.txt"],
        Some("/Users/ann/Docs"),
        false,
        true,
    );
    let op = answer(&app, &mut model, dropped);
    assert!(
        matches!(&op, crate::fileops::FileOp::Move { dst, .. } if dst == Path::new("/Users/ann/Docs")),
        "the move key moves: {op:?}"
    );
}

#[test]
#[cfg(not(windows))]
fn pointless_drops_do_nothing_and_apps_open_what_is_dropped_on_them() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[]);
    let mut cmd = drop_items(&app, &mut model, tab, &["/x/a.txt"], None, false, false);
    assert_eq!(conflict_check(&mut cmd), None, "already in the folder");
    let mut cmd = drop_items(&app, &mut model, tab, &["/x"], Some("/x/sub"), false, false);
    assert_eq!(conflict_check(&mut cmd), None, "a folder into itself");

    let mut cmd = drop_items(
        &app,
        &mut model,
        tab,
        &["/y/a.txt"],
        Some("/Applications/Preview.app"),
        false,
        false,
    );
    assert_eq!(
        system_ops(&mut cmd),
        vec![SystemOp::OpenWith {
            app: "/Applications/Preview.app".into(),
            paths: vec!["/y/a.txt".into()]
        }]
    );

    let mut cmd = app.update(
        Event::DropOnTrash {
            tab,
            paths: vec!["/y/a.txt".into()],
        },
        &mut model,
    );
    assert_eq!(
        file_ops(&mut cmd)[0].operation.op,
        crate::fileops::FileOp::Trash(vec![PathBuf::from("/y/a.txt")])
    );
}

#[test]
fn quick_look_of_one_item_steps_through_the_folder() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[
            entry("a.png", false),
            entry("b.png", false),
            entry("c.png", false),
        ],
    );
    click(&app, &mut model, tab, 1, false, false);
    let mut cmd = app.update(Event::QuickLook { tab }, &mut model);
    let look = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::QuickLook(request) => Some(request.operation),
            _ => None,
        })
        .expect("previewed");
    assert_eq!(look.paths.len(), 3, "every row around the cursor");
    assert_eq!(look.index, 1, "starting at b.png");
}

#[test]
fn typing_jumps_to_matching_names_and_a_repeated_letter_cycles() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[
            entry("apple", false),
            entry("banana", false),
            entry("berry", false),
            entry("blue", false),
        ],
    );
    let typed = |model: &mut Model, text: &str| {
        let _ = app.update(
            Event::TypeSelect {
                tab,
                text: text.into(),
            },
            model,
        );
    };
    let selected = |model: &Model| rows_selected(&app, model);
    typed(&mut model, "b");
    assert_eq!(selected(&model), ["banana"]);
    typed(&mut model, "e");
    assert_eq!(selected(&model), ["berry"], "be… continues the name");

    let _ = app.update(Event::TypingPaused { tab, typed: 2 }, &mut model);
    typed(&mut model, "b");
    assert_eq!(selected(&model), ["blue"], "after a pause, the next b");
    typed(&mut model, "b");
    assert_eq!(selected(&model), ["banana"], "bb cycles through b names");
    typed(&mut model, "B");
    assert_eq!(selected(&model), ["berry"]);
}

#[test]
fn a_path_from_a_crumb_is_revealed_opened_or_copied() {
    let app = DirFleet;
    let mut model = Model::default();
    let mut cmd = app.update(Event::RevealPath { path: "/x".into() }, &mut model);
    assert_eq!(
        system_ops(&mut cmd),
        vec![SystemOp::Reveal(vec!["/x".into()])]
    );
    let mut cmd = app.update(Event::CopyPath { path: "/x".into() }, &mut model);
    assert_eq!(system_ops(&mut cmd), vec![SystemOp::CopyText("/x".into())]);
    let mut cmd = app.update(Event::OpenInFileManager { path: "/x".into() }, &mut model);
    assert_eq!(
        system_ops(&mut cmd),
        vec![SystemOp::OpenFolder("/x".into())]
    );
}

#[test]
fn the_palette_matches_commands_favorites_and_saved_searches() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("a", false)]);
    let _ = app.update(
        Event::AddFavorite {
            path: "/data/Reports".into(),
        },
        &mut model,
    );
    let _ = search_text(&app, &mut model, tab, "report");
    let _ = app.update(
        Event::SaveSearch {
            tab,
            name: "Report search".into(),
        },
        &mut model,
    );
    assert_eq!(app.view(&model).palette, None);

    let _ = app.update(
        Event::SetPaletteText {
            text: String::new(),
        },
        &mut model,
    );
    let all = app.view(&model).palette.expect("open").items;
    assert_eq!(all[0].target, PaletteTarget::Command(CommandId::Back));
    assert!(
        all.iter()
            .all(|item| item.target != PaletteTarget::Command(CommandId::Palette))
    );

    let _ = app.update(
        Event::SetPaletteText {
            text: "report".into(),
        },
        &mut model,
    );
    let found: Vec<_> = app
        .view(&model)
        .palette
        .expect("open")
        .items
        .into_iter()
        .map(|item| item.target)
        .collect();
    assert!(found.contains(&PaletteTarget::Folder("/data/Reports".into())));
    assert!(
        found
            .iter()
            .any(|target| matches!(target, PaletteTarget::SavedSearch(_)))
    );

    let _ = app.update(
        Event::SetPaletteText {
            text: "new fold".into(),
        },
        &mut model,
    );
    let first = app
        .view(&model)
        .palette
        .expect("open")
        .items
        .into_iter()
        .next()
        .expect("a match");
    assert_eq!(first.target, PaletteTarget::Command(CommandId::NewFolder));
    let _ = app.update(Event::ClosePalette, &mut model);
    assert_eq!(app.view(&model).palette, None);

    let commands = app.view(&model).commands;
    let undo = commands
        .iter()
        .find(|c| c.id == CommandId::Undo)
        .expect("undo");
    assert!(
        !undo.shortcut.contains("Ctrl") || !cfg!(target_os = "macos"),
        "no Ctrl on the Mac"
    );
}

fn media(name: &str, revision: u128) -> DirEntry {
    let mut entry = entry(name, false);
    entry.meta.source_revision = revision;
    entry
}

/// The thumbnails `cmd` asks for, and `cmd` to answer them through.
struct Asked {
    cmd: Command<Effect, Event>,
    requests: Vec<Request<Thumbnail>>,
}

impl Asked {
    fn new(mut cmd: Command<Effect, Event>) -> Self {
        let requests = cmd
            .effects()
            .filter_map(|effect| match effect {
                Effect::Thumbnail(request) => Some(request),
                _ => None,
            })
            .collect();
        Self { cmd, requests }
    }

    fn paths(&self) -> Vec<(String, u32)> {
        self.requests
            .iter()
            .map(|request| (request.operation.path.clone(), request.operation.edge))
            .collect()
    }

    /// Answers the request for `path` with `file`, returning what the core
    /// asks for next.
    fn answer(
        &mut self,
        app: &DirFleet,
        model: &mut Model,
        path: &str,
        file: Option<&str>,
    ) -> Self {
        let at = self
            .requests
            .iter()
            .position(|request| request.operation.path == path)
            .expect("the thumbnail was asked for");
        let mut request = self.requests.remove(at);
        request
            .resolve(file.map(Into::into))
            .expect("the thumbnail resolves");
        let event = self.cmd.events().next().expect("the thumbnail comes back");
        Self::new(app.update(event, model))
    }
}

/// Navigates `tab` to `path`, answers its listing, and returns the
/// thumbnails asked for once it is shown.
fn open_media(
    app: &DirFleet,
    model: &mut Model,
    tab: u32,
    path: &str,
    entries: &[DirEntry],
) -> Asked {
    let mut cmd = app.update(
        Event::Navigate {
            tab,
            path: path.into(),
        },
        model,
    );
    let mut list = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::ListDir(request) => Some(request),
            _ => None,
        })
        .expect("a listing is requested");
    let view = list.operation.view.clone();
    list.resolve(listing(entries, view))
        .expect("listing resolves");
    let event = cmd.events().next().expect("the listing comes back");
    Asked::new(app.update(event, model))
}

fn thumbnail_of(app: &DirFleet, model: &Model, name: &str) -> String {
    shown(app, model)
        .rows
        .iter()
        .find(|row| row.name == name)
        .map(|row| row.thumbnail.clone())
        .expect("the row is shown")
}

#[test]
fn shown_images_and_videos_get_thumbnails_once() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let entries = [
        entry("folder", true),
        media("photo.jpg", 1),
        entry("notes.txt", false),
        media("clip.mp4", 1),
    ];
    let mut asked = open_media(&app, &mut model, tab, "/x", &entries);
    assert_eq!(
        asked.paths(),
        [
            ("/x/photo.jpg".to_owned(), thumbnails::LIST_EDGE),
            ("/x/clip.mp4".to_owned(), thumbnails::LIST_EDGE)
        ]
    );
    assert_eq!(thumbnail_of(&app, &model, "photo.jpg"), "");

    let next = asked.answer(&app, &mut model, "/x/photo.jpg", Some("/cache/photo.jpg"));
    assert!(next.requests.is_empty(), "nothing is asked twice");
    let _ = asked.answer(&app, &mut model, "/x/clip.mp4", None);
    assert_eq!(thumbnail_of(&app, &model, "photo.jpg"), "/cache/photo.jpg");
    assert_eq!(thumbnail_of(&app, &model, "clip.mp4"), "");
    assert_eq!(thumbnail_of(&app, &model, "notes.txt"), "");

    // Nothing more is asked for while nothing changes.
    let again = Asked::new(app.update(Event::Refresh { tab }, &mut model));
    assert!(again.requests.is_empty());
}

#[test]
fn icons_ask_at_the_icon_size_and_changed_files_ask_again() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let mut asked = open_media(&app, &mut model, tab, "/x", &[media("photo.jpg", 1)]);
    let _ = asked.answer(&app, &mut model, "/x/photo.jpg", Some("/cache/small.jpg"));

    let grid = Asked::new(app.update(
        Event::SetDisplay {
            tab,
            display: DisplayType::Icons,
        },
        &mut model,
    ));
    assert_eq!(
        grid.paths(),
        [("/x/photo.jpg".to_owned(), IconSize::Medium.points() * 2)]
    );
    assert_eq!(
        thumbnail_of(&app, &model, "photo.jpg"),
        "",
        "not yet at this size"
    );

    // The file changed: its old thumbnail is dropped and a new one asked for.
    let mut changed = open_media(&app, &mut model, tab, "/x", &[media("photo.jpg", 2)]);
    assert_eq!(changed.paths().len(), 1);
    assert_eq!(changed.requests[0].operation.revision, 2);
    let _ = changed.answer(&app, &mut model, "/x/photo.jpg", Some("/cache/new.jpg"));
    assert_eq!(thumbnail_of(&app, &model, "photo.jpg"), "/cache/new.jpg");
}

#[test]
fn a_late_thumbnail_for_an_older_file_is_dropped() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let mut old = open_media(&app, &mut model, tab, "/x", &[media("photo.jpg", 1)]);
    let mut new = open_media(&app, &mut model, tab, "/x", &[media("photo.jpg", 2)]);
    assert_eq!(new.requests.len(), 1);
    assert_eq!(new.requests[0].operation.revision, 2);
    let _ = old.answer(&app, &mut model, "/x/photo.jpg", Some("/cache/old.jpg"));
    assert_eq!(thumbnail_of(&app, &model, "photo.jpg"), "");
    let _ = new.answer(&app, &mut model, "/x/photo.jpg", Some("/cache/new.jpg"));
    assert_eq!(thumbnail_of(&app, &model, "photo.jpg"), "/cache/new.jpg");
}

#[test]
fn only_a_few_thumbnails_are_made_at_once() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let entries: Vec<DirEntry> = (0..20).map(|i| media(&format!("{i:02}.png"), 1)).collect();
    let mut asked = open_media(&app, &mut model, tab, "/x", &entries);
    assert_eq!(asked.requests.len(), 8);
    assert_eq!(
        asked.requests[0].operation.path, "/x/00.png",
        "from the top"
    );
    let next = asked.answer(&app, &mut model, "/x/00.png", Some("/cache/00.png"));
    assert_eq!(
        next.paths(),
        [("/x/08.png".to_owned(), thumbnails::LIST_EDGE)]
    );
}

#[test]
fn the_inspector_previews_its_item_and_retries_when_asked() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let mut list = open_media(
        &app,
        &mut model,
        tab,
        "/x",
        &[entry("notes.txt", false), media("photo.jpg", 1)],
    );
    let _ = list.answer(&app, &mut model, "/x/photo.jpg", Some("/cache/row.jpg"));
    let _ = app.update(Event::SetInspector { shown: true }, &mut model);
    let preview = |model: &Model| {
        shown(&app, model)
            .inspector
            .and_then(|inspector| inspector.item)
            .map(|item| item.preview)
    };

    click(&app, &mut model, tab, 0, false, false);
    assert_eq!(preview(&model), Some(PreviewState::None));

    let mut asked = Asked::new(app.update(
        Event::Click {
            tab,
            row: 1,
            command: false,
            shift: false,
        },
        &mut model,
    ));
    assert_eq!(
        asked.paths(),
        [("/x/photo.jpg".to_owned(), thumbnails::PREVIEW_EDGE)]
    );
    assert_eq!(preview(&model), Some(PreviewState::Loading));
    let _ = asked.answer(&app, &mut model, "/x/photo.jpg", None);
    assert_eq!(preview(&model), Some(PreviewState::Unavailable));

    let mut retry = Asked::new(app.update(
        Event::RetryPreview {
            path: "/x/photo.jpg".into(),
        },
        &mut model,
    ));
    assert_eq!(retry.paths().len(), 1);
    let _ = retry.answer(&app, &mut model, "/x/photo.jpg", Some("/cache/big.jpg"));
    assert_eq!(
        preview(&model),
        Some(PreviewState::Ready("/cache/big.jpg".into()))
    );
}

/// The pointer enters (`inside`) or leaves tile `row`; returns the dwell
/// the core waits for, if any, and the command it comes back through.
fn hover(
    app: &DirFleet,
    model: &mut Model,
    tab: u32,
    row: u32,
    inside: bool,
) -> (Command<Effect, Event>, Option<Request<Delay>>) {
    let mut cmd = app.update(Event::HoverRow { tab, row, inside }, model);
    let delay = cmd.effects().find_map(|effect| match effect {
        Effect::Delay(request) => Some(request),
        _ => None,
    });
    (cmd, delay)
}

/// Lets the dwell pass and returns what the core asks for then.
fn rest(
    app: &DirFleet,
    model: &mut Model,
    (mut cmd, delay): (Command<Effect, Event>, Option<Request<Delay>>),
) -> Asked {
    let mut delay = delay.expect("the core waits for the pointer to rest");
    assert_eq!(delay.operation.millis, thumbnails::HOVER_MILLIS);
    delay.resolve(()).expect("the delay resolves");
    let event = cmd.events().next().expect("the dwell comes back");
    Asked::new(app.update(event, model))
}

fn animation_of(app: &DirFleet, model: &Model, name: &str) -> String {
    shown(app, model)
        .rows
        .iter()
        .find(|row| row.name == name)
        .map(|row| row.animation.clone())
        .expect("the row is shown")
}

/// A grid of a video, a GIF and a photo, with their thumbnails made.
fn media_grid(app: &DirFleet, model: &mut Model) -> u32 {
    let tab = window(app, model);
    let entries = [
        media("clip.mp4", 1),
        media("loop.gif", 1),
        media("photo.jpg", 1),
    ];
    let mut asked = open_media(app, model, tab, "/x", &entries);
    for name in ["clip.mp4", "loop.gif", "photo.jpg"] {
        let _ = asked.answer(app, model, &format!("/x/{name}"), Some("/cache/still.jpg"));
    }
    let mut grid = Asked::new(app.update(
        Event::SetDisplay {
            tab,
            display: DisplayType::Icons,
        },
        model,
    ));
    for name in ["clip.mp4", "loop.gif", "photo.jpg"] {
        let _ = grid.answer(app, model, &format!("/x/{name}"), Some("/cache/tile.jpg"));
    }
    tab
}

#[test]
fn a_video_tile_plays_once_the_pointer_rests_on_it() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = media_grid(&app, &mut model);
    let edge = IconSize::Medium.points() * 2;

    let entered = hover(&app, &mut model, tab, 0, true);
    let mut asked = rest(&app, &mut model, entered);
    assert_eq!(asked.requests.len(), 1);
    let request = &asked.requests[0].operation;
    assert_eq!(
        (request.path.as_str(), request.edge, request.animated),
        ("/x/clip.mp4", edge, true)
    );
    assert_eq!(animation_of(&app, &model, "clip.mp4"), "");
    let _ = asked.answer(&app, &mut model, "/x/clip.mp4", Some("/cache/clip.gif"));
    assert_eq!(animation_of(&app, &model, "clip.mp4"), "/cache/clip.gif");
    assert_eq!(
        thumbnail_of(&app, &model, "clip.mp4"),
        "/cache/tile.jpg",
        "the still stays"
    );

    let _ = hover(&app, &mut model, tab, 0, false);
    assert_eq!(
        animation_of(&app, &model, "clip.mp4"),
        "",
        "it stops when the pointer leaves"
    );

    // A photo has nothing to play, so the core doesn't wait on it.
    let (_, delay) = hover(&app, &mut model, tab, 2, true);
    assert!(delay.is_none());
}

#[test]
fn moving_on_before_the_pointer_rests_plays_nothing() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = media_grid(&app, &mut model);
    let on_clip = hover(&app, &mut model, tab, 0, true);
    // Entering the GIF before leaving the clip, as the shell may report it.
    let on_gif = hover(&app, &mut model, tab, 1, true);
    let _ = hover(&app, &mut model, tab, 0, false);
    assert!(
        rest(&app, &mut model, on_clip).requests.is_empty(),
        "a late dwell is dropped"
    );
    let asked = rest(&app, &mut model, on_gif);
    assert_eq!(asked.paths().len(), 1);
    assert_eq!(asked.requests[0].operation.path, "/x/loop.gif");
}

#[test]
fn lists_play_nothing_on_hover() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let _ = open_media(&app, &mut model, tab, "/x", &[media("clip.mp4", 1)]);
    let (_, delay) = hover(&app, &mut model, tab, 0, true);
    assert!(delay.is_none());
}

#[test]
fn the_selected_tile_plays_when_the_setting_is_on() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = media_grid(&app, &mut model);
    let select = |model: &mut Model| {
        Asked::new(app.update(
            Event::Click {
                tab,
                row: 1,
                command: false,
                shift: false,
            },
            model,
        ))
    };
    assert!(select(&mut model).requests.is_empty(), "off by default");

    let settings = Settings {
        animate_selected_previews: true,
        ..Settings::default()
    };
    let mut asked = Asked::new(app.update(Event::ChangeSettings { settings }, &mut model));
    assert_eq!(asked.paths().len(), 1);
    assert!(asked.requests[0].operation.animated);
    let _ = asked.answer(&app, &mut model, "/x/loop.gif", Some("/cache/loop.gif"));
    assert_eq!(animation_of(&app, &model, "loop.gif"), "/cache/loop.gif");
}

#[test]
fn the_inspector_plays_a_selected_video() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let mut list = open_media(&app, &mut model, tab, "/x", &[media("clip.mp4", 1)]);
    let _ = list.answer(&app, &mut model, "/x/clip.mp4", Some("/cache/row.jpg"));
    let _ = app.update(Event::SetInspector { shown: true }, &mut model);
    let mut asked = Asked::new(app.update(
        Event::Click {
            tab,
            row: 0,
            command: false,
            shift: false,
        },
        &mut model,
    ));
    let wanted: Vec<(bool, u32)> = asked
        .requests
        .iter()
        .map(|request| (request.operation.animated, request.operation.edge))
        .collect();
    assert_eq!(
        wanted,
        [
            (true, thumbnails::PREVIEW_EDGE),
            (false, thumbnails::PREVIEW_EDGE)
        ],
        "the frames first, then the still"
    );
    let at = asked
        .requests
        .iter()
        .position(|request| request.operation.animated)
        .expect("the frames are asked for");
    let mut frames = asked.requests.remove(at);
    frames
        .resolve(Some("/cache/clip.gif".into()))
        .expect("resolves");
    let event = asked.cmd.events().next().expect("the frames come back");
    let _ = app.update(event, &mut model);
    let item = shown(&app, &model)
        .inspector
        .and_then(|inspector| inspector.item)
        .expect("the item");
    assert_eq!(item.animation, "/cache/clip.gif");
}

#[test]
fn media_notices_become_toasts() {
    let app = DirFleet;
    let mut model = Model::default();
    let mut cmd = app.update(
        Event::Started {
            open: None,
            home: "/home".into(),
        },
        &mut model,
    );
    let mut notices = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::MediaNotices(request) => Some(request),
            _ => None,
        })
        .expect("the core listens to the media service");
    for text in [
        "Downloading FFmpeg for video previews…",
        "Video previews are ready",
    ] {
        notices
            .resolve(MediaNotice {
                kind: ToastKind::Info,
                text: text.into(),
            })
            .expect("a notice resolves");
    }
    let events: Vec<Event> = cmd.events().collect();
    for event in events {
        let _ = app.update(event, &mut model);
    }
    let toasts: Vec<String> = app
        .view(&model)
        .toasts
        .into_iter()
        .map(|t| t.text)
        .collect();
    assert_eq!(
        toasts,
        [
            "Downloading FFmpeg for video previews…",
            "Video previews are ready"
        ]
    );
}

#[test]
fn dates_read_as_time_ago_once_the_clock_ticks() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let mut file = entry("a.txt", false);
    file.meta.modified_at = TimestampSeconds::from_unix(1_762_610_580);
    file.meta.created_at = TimestampSeconds::from_unix(1_762_000_000);
    open(&app, &mut model, tab, "/x", &[file]);
    let modified = |model: &Model| {
        let view = app.view(model);
        let at = view
            .columns
            .iter()
            .skip(1)
            .position(|column| column.column == Column::Modified)
            .expect("a Modified column");
        view.windows[0].active.rows[0].cells[at].clone()
    };
    assert_eq!(
        modified(&model),
        "2025-11-08 14:03",
        "exact until the clock ticks"
    );
    assert_eq!(shown(&app, &model).rows[0].tooltip, "");

    let _ = app.update(
        Event::ClockTicked {
            now: 1_762_610_580 + 2 * 3600 + 10,
        },
        &mut model,
    );
    assert_eq!(modified(&model), "2 hours ago");
    assert_eq!(
        shown(&app, &model).rows[0].tooltip,
        "Modified: 2025-11-08 14:03\nCreated: 2025-11-01 12:26",
        "the tooltip keeps the exact dates"
    );
}

#[test]
fn a_folder_the_app_may_not_read_offers_the_privacy_settings() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let mut cmd = app.update(
        Event::Navigate {
            tab,
            path: "/private".into(),
        },
        &mut model,
    );
    let mut list = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::ListDir(request) => Some(request),
            _ => None,
        })
        .expect("a listing is requested");
    list.resolve(ListDirOutput::Denied(
        "/private: Operation not permitted (os error 1)".into(),
    ))
    .expect("resolves");
    let event = cmd.events().next().expect("the refusal comes back");
    let _ = app.update(event, &mut model);
    let view = shown(&app, &model);
    assert!(view.denied);
    assert!(view.error.is_some());

    let mut cmd = app.update(Event::OpenPrivacySettings, &mut model);
    assert!(cmd.effects().any(|effect| matches!(
        effect,
        Effect::System(request) if request.operation == SystemOp::OpenPrivacySettings
    )));

    open(&app, &mut model, tab, "/x", &[entry("a", false)]);
    assert!(!shown(&app, &model).denied, "a readable folder clears it");
}

#[test]
fn large_tiles_show_a_files_size() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[entry("sub", true), entry("a.txt", false)],
    );
    let details: Vec<String> = shown(&app, &model)
        .rows
        .iter()
        .map(|row| row.detail.clone())
        .collect();
    assert_eq!(details, ["", "10 B"]);
}

fn batch(app: &DirFleet, model: &Model) -> BatchRenameView {
    shown(app, model)
        .batch_rename
        .expect("batch rename is open")
}

#[test]
fn renaming_several_items_previews_names_and_flags_clashes() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let entries = [
        entry("pics", true),
        entry("a.jpg", false),
        entry("b.jpg", false),
        entry("c.jpg", false),
    ];
    open(&app, &mut model, tab, "/x", &entries);
    click(&app, &mut model, tab, 0, false, false);
    click(&app, &mut model, tab, 1, true, false);
    click(&app, &mut model, tab, 2, true, false);
    let _ = app.update(Event::StartRename { tab }, &mut model);
    assert!(
        shown(&app, &model).renaming.is_none(),
        "several: no inline rename"
    );
    assert_eq!(batch(&app, &model).changes, 0, "nothing to find yet");

    let set = |model: &mut Model, fields: BatchFields| {
        let _ = app.update(Event::SetBatchRename { tab, fields }, model);
    };
    set(
        &mut model,
        BatchFields {
            template: "Trip {n}{ext}".into(),
            ..BatchFields::default()
        },
    );
    let names: Vec<String> = batch(&app, &model)
        .items
        .into_iter()
        .map(|i| i.new)
        .collect();
    assert_eq!(
        names,
        ["Trip 1", "Trip 2.jpg", "Trip 3.jpg"],
        "a folder has no extension"
    );

    set(
        &mut model,
        BatchFields {
            template: "same".into(),
            ..BatchFields::default()
        },
    );
    assert_eq!(batch(&app, &model).problems, 3);

    set(
        &mut model,
        BatchFields {
            find: "a.jpg".into(),
            replace: "c.jpg".into(),
            ..BatchFields::default()
        },
    );
    let view = batch(&app, &model);
    assert_eq!(
        view.items[1].problem,
        "An item with this name already exists."
    );
    let mut cmd = app.update(Event::ApplyBatchRename { tab }, &mut model);
    assert!(
        file_ops(&mut cmd).is_empty(),
        "not while a name can't be used"
    );

    set(
        &mut model,
        BatchFields {
            find: ".jpg".into(),
            replace: ".jpeg".into(),
            ..BatchFields::default()
        },
    );
    assert_eq!(
        (batch(&app, &model).changes, batch(&app, &model).problems),
        (2, 0)
    );
    let mut cmd = app.update(Event::ApplyBatchRename { tab }, &mut model);
    let run = file_ops(&mut cmd).pop().expect("the renames run");
    let x = Path::new("/x");
    assert_eq!(
        run.operation.op,
        crate::fileops::FileOp::RenameMany(vec![
            (x.join("a.jpg"), x.join("a.jpeg")),
            (x.join("b.jpg"), x.join("b.jpeg")),
        ])
    );
    assert!(
        shown(&app, &model).batch_rename.is_none(),
        "the dialog closes"
    );
}

#[test]
fn move_and_copy_to_offer_other_tabs_favorites_and_drives_once() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("a", false)]);
    let _ = app.update(
        Event::NewTab {
            window: 1,
            path: Some("/y".into()),
        },
        &mut model,
    );
    for path in ["/y", "/x", "/z"] {
        let _ = app.update(Event::AddFavorite { path: path.into() }, &mut model);
    }
    let _ = app.update(Event::ActivateTab { tab }, &mut model);
    let targets: Vec<(String, String)> = shown(&app, &model)
        .transfer_targets
        .into_iter()
        .map(|target| (target.path, target.detail))
        .collect();
    assert_eq!(
        targets,
        [
            ("/y".to_owned(), "Tab".to_owned()),
            ("/z".to_owned(), "Favorite".to_owned())
        ],
        "never the tab's own folder, and each folder once"
    );
}

#[test]
fn pasting_into_a_folder_sends_the_clipboard_there() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("sub", true)]);
    let mut paste = app.update(
        Event::PasteInto {
            tab,
            folder: "/x/sub".into(),
        },
        &mut model,
    );
    let mut read = paste
        .effects()
        .find_map(|effect| match effect {
            Effect::Clipboard(request) => Some(request),
            _ => None,
        })
        .expect("the clipboard is read");
    read.resolve(ClipboardDone(Ok(Some((
        vec![PathBuf::from("/y/a")],
        false,
    )))))
    .expect("resolves");
    let event = paste.events().next().expect("the files come back");
    let mut cmd = app.update(event, &mut model);
    let check = cmd.effects().find_map(|effect| match effect {
        Effect::FindConflicts(request) => Some(request.operation),
        _ => None,
    });
    assert_eq!(
        check,
        Some(FindConflicts {
            srcs: vec![PathBuf::from("/y/a")],
            dst: PathBuf::from("/x/sub")
        })
    );
}

#[test]
fn finished_operations_are_listed_newest_first_and_cut_items_counted() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[entry("a", false), entry("b", false)],
    );
    click(&app, &mut model, tab, 0, false, false);
    let _ = app.update(Event::ClockTicked { now: 1_000_000 }, &mut model);
    let mut cmd = app.update(Event::Duplicate { tab }, &mut model);
    let _ = finish(
        &app,
        &mut model,
        &mut cmd,
        outcome(0, crate::fileops::Origin::User),
    );
    let mut cmd = app.update(Event::DeleteSelection { tab }, &mut model);
    let _ = finish(
        &app,
        &mut model,
        &mut cmd,
        crate::fileops::Outcome {
            description: "Deleting 1 item".into(),
            errors: vec!["a: in use".into()],
            ..outcome(0, crate::fileops::Origin::User)
        },
    );
    let activity = app.view(&model).activity;
    let states: Vec<ActivityState> = activity.iter().map(|item| item.state).collect();
    assert_eq!(states, [ActivityState::Failed, ActivityState::Done]);
    assert_eq!(activity[0].errors, ["a: in use"]);
    assert_eq!(activity[1].when, "Just now");

    let _ = app.update(Event::CutSelection { tab }, &mut model);
    assert_eq!(app.view(&model).cut_count, 1);
}

#[test]
fn the_trash_blocks_rename_copy_and_sending_items() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open_trash(&app, &mut model, tab, &[entry("old.txt", false)]);
    click(&app, &mut model, tab, 0, false, false);
    let _ = app.update(Event::StartRename { tab }, &mut model);
    assert!(shown(&app, &model).renaming.is_none());
    for event in [
        Event::CopySelection { tab },
        Event::CutSelection { tab },
        Event::CopyTo {
            tab,
            folder: "/y".into(),
        },
        Event::NewFolder { tab },
    ] {
        let mut cmd = app.update(event, &mut model);
        assert!(
            cmd.effects().all(|effect| !matches!(
                effect,
                Effect::Clipboard(_) | Effect::RunFileOp(_) | Effect::FindConflicts(_)
            )),
            "nothing happens in the Trash"
        );
    }
    assert_eq!(shown(&app, &model).transfer_targets, Vec::new());
}

/// Starts the app with nothing saved and returns the command the saved
/// state's arrival made, which checks access.
fn started(app: &DirFleet, model: &mut Model) -> Command<Effect, Event> {
    let mut cmd = app.update(
        Event::Started {
            open: Some("/x".into()),
            home: "/home".into(),
        },
        model,
    );
    let mut load = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::LoadState(request) => Some(request),
            _ => None,
        })
        .expect("the saved state is read");
    load.resolve(LoadedState::Missing).expect("resolves");
    let event = cmd.events().next().expect("the saved state comes back");
    app.update(event, model)
}

/// Answers the access check in `cmd` and feeds the result back.
fn answer_access(
    app: &DirFleet,
    model: &mut Model,
    cmd: &mut Command<Effect, Event>,
    found: DiskAccess,
) -> Command<Effect, Event> {
    let mut check = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::DiskAccessCheck(request) => Some(request),
            _ => None,
        })
        .expect("access is checked");
    check.resolve(found).expect("resolves");
    let event = cmd
        .events()
        .find(|event| matches!(event, Event::DiskAccessChecked { .. }))
        .expect("the answer comes back");
    app.update(event, model)
}

const MISSING: DiskAccess = DiskAccess {
    applies: true,
    granted: false,
};

#[test]
fn missing_full_disk_access_is_offered_watched_for_and_ends_in_a_restart() {
    let app = DirFleet;
    let mut model = Model::default();
    let mut cmd = started(&app, &mut model);
    let _ = answer_access(&app, &mut model, &mut cmd, MISSING);
    assert_eq!(app.view(&model).access.prompt, DiskAccessPrompt::Ask);

    let mut open = app.update(Event::OpenDiskAccessSettings, &mut model);
    assert_eq!(app.view(&model).access.prompt, DiskAccessPrompt::Waiting);
    let mut wait = answer_access(&app, &mut model, &mut open, MISSING);
    let mut delay = wait
        .effects()
        .find_map(|effect| match effect {
            Effect::Delay(request) => Some(request),
            _ => None,
        })
        .expect("checked again in a second");
    assert_eq!(delay.operation.millis, 1000);
    delay.resolve(()).expect("resolves");
    let event = wait.events().next().expect("the recheck");
    let mut recheck = app.update(event, &mut model);
    let _ = answer_access(
        &app,
        &mut model,
        &mut recheck,
        DiskAccess {
            applies: true,
            granted: true,
        },
    );
    assert_eq!(app.view(&model).access.prompt, DiskAccessPrompt::Restart);

    let mut restart = app.update(Event::RestartApp, &mut model);
    let mut relaunch = restart
        .effects()
        .find_map(|effect| match effect {
            Effect::System(request) if request.operation == SystemOp::Relaunch => Some(request),
            _ => None,
        })
        .expect("a relaunch is set up");
    relaunch.resolve(SystemDone(Ok(()))).expect("resolves");
    let event = restart.events().next().expect("the relaunch is ready");
    let mut quit = app.update(event, &mut model);
    assert!(
        quit.effects()
            .any(|effect| matches!(effect, Effect::QuitApp(_)))
    );
}

#[test]
fn opening_the_settings_from_the_banner_asks_the_system() {
    let app = DirFleet;
    let mut model = Model::default();
    let mut cmd = started(&app, &mut model);
    let _ = answer_access(&app, &mut model, &mut cmd, MISSING);
    let mut open = app.update(Event::OpenDiskAccessSettings, &mut model);
    assert!(open.effects().any(|effect| matches!(
        effect,
        Effect::System(request) if request.operation == SystemOp::OpenPrivacySettings
    )));
}

#[test]
fn the_banner_hides_for_tests_when_dismissed_and_after_dont_ask_again() {
    let app = DirFleet;
    let mut model = Model::default();
    let mut cmd = started(&app, &mut model);
    let _ = answer_access(
        &app,
        &mut model,
        &mut cmd,
        DiskAccess {
            applies: false,
            granted: true,
        },
    );
    assert_eq!(
        app.view(&model).access.prompt,
        DiskAccessPrompt::Hidden,
        "not a built app"
    );

    let mut model = Model::default();
    let mut cmd = started(&app, &mut model);
    let _ = answer_access(&app, &mut model, &mut cmd, MISSING);
    let _ = app.update(Event::DismissDiskAccess { forever: true }, &mut model);
    assert_eq!(app.view(&model).access.prompt, DiskAccessPrompt::Hidden);
    assert!(
        !app.view(&model).settings.ask_full_disk_access,
        "kept in the settings"
    );
    assert!(!app.view(&model).access.disk_access_granted);
}

#[test]
fn the_terminal_is_checked_at_start_and_when_it_changes() {
    let app = DirFleet;
    let mut model = Model::default();
    let mut cmd = started(&app, &mut model);
    let check = cmd.effects().find_map(|effect| match effect {
        Effect::TerminalCheck(request) => Some(request.operation),
        _ => None,
    });
    assert_eq!(check.map(|c| c.app), Some(Settings::default().terminal));

    let settings = Settings {
        terminal: "iTerm".into(),
        ..Settings::default()
    };
    let mut cmd = app.update(Event::ChangeSettings { settings }, &mut model);
    let mut check = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::TerminalCheck(request) => Some(request),
            _ => None,
        })
        .expect("the new terminal is checked");
    assert_eq!(check.operation.app, "iTerm");
    check.resolve(false).expect("resolves");
    let event = cmd.events().next().expect("the answer comes back");
    let _ = app.update(event, &mut model);
    let access = app.view(&model).access;
    assert!(!access.terminal_found);
    assert!(access.version.starts_with("Version "), "{}", access.version);
    assert!(access.website.starts_with("https://"));
}

/// Types `text` into the tab's search and lets the pause pass, so it is
/// recorded.
fn type_and_pause(app: &DirFleet, model: &mut Model, tab: u32, text: &str) {
    let mut cmd = search_text(app, model, tab, text);
    let mut delay = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::Delay(request) => Some(request),
            _ => None,
        })
        .expect("typing waits for a pause");
    delay.resolve(()).expect("resolves");
    let event = cmd
        .events()
        .find(|event| matches!(event, Event::SearchPaused { .. }))
        .expect("the pause comes back");
    let _ = app.update(event, model);
}

fn search_of(app: &DirFleet, model: &Model) -> Option<(String, Vec<String>)> {
    shown(app, model).search.map(|search| {
        (
            search.text,
            search.chips.into_iter().map(|chip| chip.token).collect(),
        )
    })
}

#[test]
fn search_edits_are_undone_and_redone_with_file_operations() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[entry("report.pdf", false), entry("a", false)],
    );
    type_and_pause(&app, &mut model, tab, "rep");
    assert_eq!(
        app.view(&model).undo.as_deref(),
        Some("type \u{201c}rep\u{201d}")
    );
    let _ = app.update(
        Event::ToggleSearchFilter {
            tab,
            token: "ext:pdf".into(),
        },
        &mut model,
    );
    assert_eq!(app.view(&model).undo.as_deref(), Some("add filter ext:pdf"));

    let _ = app.update(Event::Undo, &mut model);
    assert_eq!(search_of(&app, &model), Some(("rep".into(), Vec::new())));
    assert_eq!(app.view(&model).redo.as_deref(), Some("add filter ext:pdf"));
    assert!(
        app.view(&model)
            .toasts
            .iter()
            .any(|toast| toast.text == "Undid add filter ext:pdf")
    );
    let _ = app.update(Event::Redo, &mut model);
    assert_eq!(
        search_of(&app, &model),
        Some(("rep".into(), vec!["ext:pdf".to_owned()]))
    );
    let _ = app.update(Event::Undo, &mut model);
    let _ = app.update(Event::Undo, &mut model);
    assert_eq!(search_of(&app, &model), None, "back to no search at all");
}

#[test]
fn saved_search_changes_are_undoable() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("report.pdf", false)]);
    type_and_pause(&app, &mut model, tab, "rep");
    let _ = app.update(
        Event::SaveSearch {
            tab,
            name: "Reports".into(),
        },
        &mut model,
    );
    assert_eq!(app.view(&model).saved_searches.len(), 1);
    assert_eq!(
        app.view(&model).undo.as_deref(),
        Some("save search \u{201c}Reports\u{201d}")
    );
    let id = app.view(&model).saved_searches[0].id;
    let _ = app.update(Event::DeleteSavedSearch { id }, &mut model);
    assert_eq!(app.view(&model).saved_searches.len(), 0);
    let _ = app.update(Event::Undo, &mut model);
    assert_eq!(
        app.view(&model).saved_searches.len(),
        1,
        "the deleted one is back"
    );
    let _ = app.update(Event::Undo, &mut model);
    assert!(
        app.view(&model).saved_searches.is_empty(),
        "and saving it is undone"
    );
    let _ = app.update(Event::Redo, &mut model);
    assert_eq!(app.view(&model).saved_searches[0].name, "Reports");
}

#[test]
fn nothing_found_in_the_folder_offers_subfolders_and_favorites() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("a.txt", false)]);
    let mut cmd = search_text(&app, &mut model, tab, "zzz");
    let mut order = reorders(&mut cmd).pop().expect("the folder is filtered");
    let (listing, view) = (
        Arc::clone(&order.operation.listing),
        order.operation.view.clone(),
    );
    order
        .resolve(Ordered {
            listing,
            order: Vec::new(),
            view,
        })
        .expect("resolves");
    let event = cmd
        .events()
        .find(|event| matches!(event, Event::Ordered { .. }))
        .expect("the filtered order comes back");
    let _ = app.update(event, &mut model);
    let search = shown(&app, &model).search.expect("open");
    assert!(search.offer_subfolders);
    assert!(!search.offer_favorites, "no favorites yet");

    let _ = app.update(Event::AddFavorite { path: "/y".into() }, &mut model);
    assert!(shown(&app, &model).search.expect("open").offer_favorites);

    let _ = app.update(
        Event::SetSearchDepth {
            tab,
            depth: WIDER_DEPTH,
        },
        &mut model,
    );
    let search = shown(&app, &model).search.expect("open");
    assert!(
        !search.offer_subfolders && !search.offer_favorites,
        "already wider"
    );
    assert_ne!(app.view(&model).search_help, Vec::new());
}

#[test]
fn escape_clears_the_text_then_the_filters_then_closes() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("a.txt", false)]);
    let _ = app.update(
        Event::ToggleSearchFilter {
            tab,
            token: "ext:txt".into(),
        },
        &mut model,
    );
    let _ = search_text(&app, &mut model, tab, "a");
    let _ = app.update(Event::SearchEscape { tab }, &mut model);
    assert_eq!(
        search_of(&app, &model),
        Some((String::new(), vec!["ext:txt".to_owned()]))
    );
    let _ = app.update(Event::SearchEscape { tab }, &mut model);
    assert_eq!(search_of(&app, &model), Some((String::new(), Vec::new())));
    let _ = app.update(Event::SearchEscape { tab }, &mut model);
    assert_eq!(search_of(&app, &model), None);
}

#[test]
fn an_edit_while_typing_waits_records_the_typing_first() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("a.txt", false)]);
    let _ = search_text(&app, &mut model, tab, "gem");
    let _ = app.update(
        Event::SetSearchDepth {
            tab,
            depth: WIDER_DEPTH,
        },
        &mut model,
    );
    assert_eq!(app.view(&model).undo.as_deref(), Some("search subfolders"));
    let _ = app.update(Event::Undo, &mut model);
    let search = shown(&app, &model).search.expect("the search stays");
    assert_eq!((search.text.as_str(), search.depth), ("gem", 1));
    assert_eq!(
        app.view(&model).undo.as_deref(),
        Some("type \u{201c}gem\u{201d}")
    );
}

fn saved_state(cmd: &mut Command<Effect, Event>) -> Option<PersistedState> {
    cmd.effects().find_map(|effect| match effect {
        Effect::SaveState(request) => Some(request.operation.state),
        _ => None,
    })
}

#[test]
fn window_frames_are_kept_and_come_back() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = start(&app, &mut model, Some("/a"), LoadedState::Missing);
    let window = app.view(&model).windows[0].id;
    let frame = WindowFrame {
        x: 100,
        y: 200,
        width: 900,
        height: 600,
    };
    let mut cmd = app.update(Event::WindowFramed { window, frame }, &mut model);
    let saved = saved_state(&mut cmd).expect("the new frame is saved");
    assert_eq!(saved.windows[0].frame, Some(frame));

    let mut again = Model::default();
    let _ = start(&app, &mut again, None, LoadedState::Found(saved));
    assert_eq!(
        app.view(&again).windows[0].frame,
        Some(frame),
        "it comes back"
    );
}

#[test]
fn sidebar_sections_move_and_their_order_is_kept() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = start(&app, &mut model, Some("/a"), LoadedState::Missing);
    let _ = app.update(Event::AddFavorite { path: "/a".into() }, &mut model);
    // The tags, which every sidebar lists last, are left aside here.
    let order = |model: &Model| -> Vec<SidebarSection> {
        app.view(model).windows[0]
            .sidebar
            .iter()
            .map(|section| section.section)
            .filter(|section| *section != SidebarSection::Tags)
            .collect()
    };
    assert_eq!(
        order(&model),
        [SidebarSection::Favorites, SidebarSection::Locations]
    );
    let mut cmd = app.update(
        Event::MoveSidebarSection {
            section: SidebarSection::Locations,
            to: SidebarSection::Favorites,
        },
        &mut model,
    );
    assert_eq!(
        order(&model),
        [SidebarSection::Locations, SidebarSection::Favorites]
    );
    let saved = saved_state(&mut cmd).expect("the order is saved");
    assert_eq!(saved.section_order[0], SidebarSection::Locations);

    let mut again = Model::default();
    let _ = start(&app, &mut again, None, LoadedState::Found(saved));
    assert_eq!(
        order(&again),
        [SidebarSection::Locations, SidebarSection::Favorites]
    );
}

#[test]
fn a_saved_search_runs_in_a_new_window_at_its_folder() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("report.pdf", false)]);
    type_and_pause(&app, &mut model, tab, "rep");
    let _ = app.update(
        Event::SaveSearch {
            tab,
            name: "Reports".into(),
        },
        &mut model,
    );
    let id = app.view(&model).saved_searches[0].id;
    let _ = app.update(Event::CloseSearch { tab }, &mut model);
    let mut cmd = app.update(Event::RunSavedSearchInNewWindow { tab, id }, &mut model);
    assert!(
        cmd.effects()
            .any(|effect| matches!(effect, Effect::OpenWindow(_)))
    );
    let view = app.view(&model);
    assert_eq!(view.windows.len(), 2);
    let new = &view.windows[1].active;
    assert_eq!(new.path, "/x");
    assert_eq!(new.search.as_ref().map(|s| s.text.as_str()), Some("rep"));
    assert_eq!(new.saved.as_ref().map(|s| s.name.as_str()), Some("Reports"));
    assert_eq!(
        shown(&app, &model).search,
        None,
        "the first tab is left as it was"
    );
}

#[test]
fn a_right_click_outside_the_selection_selects_that_row() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[entry("a", false), entry("b", false), entry("c", false)],
    );
    click(&app, &mut model, tab, 0, false, false);
    let _ = app.update(Event::ContextClick { tab, row: 1 }, &mut model);
    assert_eq!(rows_selected(&app, &model), ["b"], "the menu acts on b");
    click(&app, &mut model, tab, 2, true, false);
    let _ = app.update(Event::ContextClick { tab, row: 1 }, &mut model);
    assert_eq!(
        rows_selected(&app, &model),
        ["b", "c"],
        "inside the selection, it stays"
    );
}

/// A window on the drive `/Volumes/USB`, with the drive listed.
fn on_usb(app: &DirFleet, model: &mut Model) -> u32 {
    let tab = window(app, model);
    let usb = crate::drives::test_drive("/Volumes/USB", "USB", crate::drives::DriveKind::Removable);
    let _ = app.update(
        Event::DrivesChanged {
            drives: DriveList(vec![usb]),
        },
        model,
    );
    open(app, model, tab, "/Volumes/USB", &[entry("a.txt", false)]);
    tab
}

#[test]
fn the_status_line_says_how_much_room_the_drive_has() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = on_usb(&app, &mut model);
    let status = shown(&app, &model)
        .drive_status
        .expect("the folder is on a drive");
    assert_eq!(status.free, "16.0 GB available");
    assert!(
        status.detail.contains("16.0 GB free of 64.0 GB"),
        "{}",
        status.detail
    );
    assert_eq!(status.eject.as_deref(), Some("Eject USB"));
}

#[test]
fn ejecting_a_busy_drive_asks_first_and_cancels_its_operations() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = on_usb(&app, &mut model);
    let mount = "/Volumes/USB".to_owned();
    let mut idle = app.update(
        Event::EjectDrive {
            mount: mount.clone(),
        },
        &mut model,
    );
    assert!(
        idle.effects()
            .any(|effect| matches!(effect, Effect::Eject(_))),
        "idle: ejected at once"
    );

    click(&app, &mut model, tab, 0, false, false);
    let mut copying = app.update(Event::Duplicate { tab }, &mut model);
    assert_eq!(file_ops(&mut copying).len(), 1);
    let mut asked = app.update(Event::EjectDrive { mount }, &mut model);
    assert!(
        asked
            .effects()
            .all(|effect| !matches!(effect, Effect::Eject(_)))
    );
    let confirm = app.view(&model).eject_confirm.expect("the user is asked");
    assert_eq!(confirm.title, "Eject USB?");
    assert_eq!(
        confirm.message,
        "A file operation is still running on this drive. Ejecting cancels it."
    );

    let mut go = app.update(Event::ConfirmEject, &mut model);
    let effects: Vec<Effect> = go.effects().collect();
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::CancelFileOp(_)))
    );
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Eject(_)))
    );
    assert_eq!(app.view(&model).eject_confirm, None);
}

/// The one request of this kind `cmd` makes.
fn request_of<T: crux_core::capability::Operation>(
    cmd: &mut Command<Effect, Event>,
    pick: impl Fn(Effect) -> Option<Request<T>>,
) -> Request<T> {
    cmd.effects().find_map(pick).expect("asked for")
}

fn picker_frame(cmd: &mut Command<Effect, Event>) -> Request<PickerFrame> {
    request_of(cmd, |effect| match effect {
        Effect::PickerFrame(request) => Some(request),
        _ => None,
    })
}

fn choice_op(cmd: &mut Command<Effect, Event>) -> Request<ThumbnailChoiceOp> {
    request_of(cmd, |effect| match effect {
        Effect::ThumbnailChoice(request) => Some(request),
        _ => None,
    })
}

fn choices_of(app: &DirFleet, model: &Model) -> Vec<ThumbnailChoice> {
    shown(app, model)
        .rows
        .iter()
        .map(|row| row.thumbnail_choice)
        .collect()
}

/// A window with clip.mp4 and notes.txt, and the picker open on the clip
/// with its first two frames extracted (the default one at 1 s first).
/// Returns the tab and where the second frame is.
fn picker_with_two_frames(app: &DirFleet, model: &mut Model) -> (u32, u64) {
    let tab = window(app, model);
    let entries = [media("clip.mp4", 1), entry("notes.txt", false)];
    let mut asked = open_media(app, model, tab, "/x", &entries);
    let _ = asked.answer(app, model, "/x/clip.mp4", Some("/cache/default.jpg"));
    assert_eq!(
        choices_of(app, model),
        [ThumbnailChoice::Automatic, ThumbnailChoice::None]
    );
    let mut cmd = app.update(Event::SelectThumbnail { tab, row: 1 }, model);
    assert!(app.view(model).thumbnail_picker.is_none(), "not a video");
    assert!(cmd.effects().next().is_none());

    let mut cmd = app.update(Event::SelectThumbnail { tab, row: 0 }, model);
    let mut timing = request_of(&mut cmd, |effect| match effect {
        Effect::PickerTiming(request) => Some(request),
        _ => None,
    });
    assert_eq!(timing.operation.path, "/x/clip.mp4");
    let picker = app
        .view(model)
        .thumbnail_picker
        .expect("the dialog is open");
    assert_eq!(
        (picker.tab, picker.name.as_str(), picker.status.as_str()),
        (tab, "clip.mp4", "Reading the video…")
    );
    timing
        .resolve(VideoTiming(Ok(VideoLength {
            duration_ms: Some(60_000),
            frame_rate_milli: Some(30_000),
            default_ms: 1_000,
        })))
        .expect("resolves");
    let event = cmd.events().next().expect("the length comes back");
    let mut cmd = app.update(event, model);

    // One frame at a time, the default one first.
    let mut first = picker_frame(&mut cmd);
    assert_eq!(first.operation.at_ms, 1_000);
    let picker = app.view(model).thumbnail_picker.expect("open");
    assert_eq!(picker.duration, "1:00");
    assert!(picker.frames.len() > 10, "{}", picker.frames.len());
    assert!(
        picker.status.starts_with("Extracting 0 of "),
        "{}",
        picker.status
    );
    assert_eq!(
        (picker.preview_badge.as_str(), picker.preview_label.as_str()),
        ("Default", "0:01")
    );
    assert!(!picker.can_apply && !picker.can_reset);
    first
        .resolve(Some("/cache/f1.jpg".into()))
        .expect("resolves");
    let event = cmd.events().next().expect("the frame comes back");
    let mut cmd = app.update(event, model);
    let mut second = picker_frame(&mut cmd);
    let at = second.operation.at_ms;
    assert_ne!(at, 1_000);
    second
        .resolve(Some("/cache/f2.jpg".into()))
        .expect("resolves");
    let event = cmd.events().next().expect("the frame comes back");
    let _ = app.update(event, model);
    assert_eq!(
        app.view(model)
            .thumbnail_picker
            .expect("open")
            .preview_image,
        "/cache/f1.jpg"
    );
    (tab, at)
}

#[test]
fn a_video_s_thumbnail_is_chosen_from_its_frames_and_reset() {
    let app = DirFleet;
    let mut model = Model::default();
    let (_, at) = picker_with_two_frames(&app, &mut model);

    // The arrow keys move over finished frames.
    let forward = at > 1_000;
    let _ = app.update(Event::StepPickerFrame { forward, steps: 1 }, &mut model);
    let picker = app.view(&model).thumbnail_picker.expect("open");
    assert_eq!(picker.preview_image, "/cache/f2.jpg");
    assert!(picker.can_apply);
    assert_eq!(
        picker.frames.iter().filter(|frame| frame.selected).count(),
        1
    );

    let mut cmd = app.update(Event::UsePickerFrame, &mut model);
    assert!(
        app.view(&model).thumbnail_picker.is_none(),
        "the dialog closes"
    );
    let mut set = choice_op(&mut cmd);
    let chosen = ChosenFrame {
        at_ms: at,
        revision: 1,
        size: 10,
    };
    assert_eq!(
        set.operation,
        ThumbnailChoiceOp::Set {
            path: "/x/clip.mp4".into(),
            frame: Some(chosen)
        }
    );
    set.resolve(ThumbnailChoices(vec![("/x/clip.mp4".into(), chosen)]))
        .expect("resolves");
    let event = cmd.events().next().expect("the choices come back");
    let mut asked = Asked::new(app.update(event, &mut model));
    assert_eq!(
        asked.paths(),
        [("/x/clip.mp4".to_owned(), thumbnails::LIST_EDGE)],
        "the poster is made again, from the chosen frame"
    );
    assert_eq!(
        choices_of(&app, &model),
        [ThumbnailChoice::Chosen, ThumbnailChoice::None]
    );
    let _ = asked.answer(&app, &mut model, "/x/clip.mp4", Some("/cache/chosen.jpg"));
    assert_eq!(thumbnail_of(&app, &model, "clip.mp4"), "/cache/chosen.jpg");
}

#[test]
fn a_chosen_thumbnail_is_reset_from_its_row_or_all_at_once() {
    let app = DirFleet;
    let mut model = Model::default();
    let (tab, at) = picker_with_two_frames(&app, &mut model);
    let _ = app.update(Event::SelectPickerFrame { at_ms: at }, &mut model);
    let mut cmd = app.update(Event::UsePickerFrame, &mut model);
    let chosen = ChosenFrame {
        at_ms: at,
        revision: 1,
        size: 10,
    };
    choice_op(&mut cmd)
        .resolve(ThumbnailChoices(vec![("/x/clip.mp4".into(), chosen)]))
        .expect("resolves");
    let event = cmd.events().next().expect("the choices come back");
    let _ = app.update(event, &mut model);

    // Opened again, the dialog starts from the choice.
    let mut cmd = app.update(Event::SelectThumbnail { tab, row: 0 }, &mut model);
    let _ = request_of(&mut cmd, |effect| match effect {
        Effect::PickerTiming(request) => Some(request),
        _ => None,
    });
    assert!(app.view(&model).thumbnail_picker.expect("open").can_reset);
    let _ = app.update(Event::ClosePicker, &mut model);
    assert!(app.view(&model).thumbnail_picker.is_none());

    let mut cmd = app.update(Event::ResetThumbnail { tab, row: 0 }, &mut model);
    let mut reset = choice_op(&mut cmd);
    assert_eq!(
        reset.operation,
        ThumbnailChoiceOp::Set {
            path: "/x/clip.mp4".into(),
            frame: None
        }
    );
    reset
        .resolve(ThumbnailChoices(Vec::new()))
        .expect("resolves");
    let event = cmd.events().next().expect("the choices come back");
    let asked = Asked::new(app.update(event, &mut model));
    assert_eq!(asked.paths().len(), 1, "the default poster again");
    assert_eq!(choices_of(&app, &model)[0], ThumbnailChoice::Automatic);

    let mut cmd = app.update(Event::ResetAllThumbnails, &mut model);
    assert_eq!(choice_op(&mut cmd).operation, ThumbnailChoiceOp::Clear);
}

#[test]
fn a_choice_made_for_an_earlier_version_of_the_file_is_not_used() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let _ = open_media(&app, &mut model, tab, "/x", &[media("clip.mp4", 2)]);
    let chosen = ChosenFrame {
        at_ms: 4_000,
        revision: 1,
        size: 10,
    };
    let _ = app.update(
        Event::ThumbnailChoicesChanged {
            choices: ThumbnailChoices(vec![("/x/clip.mp4".into(), chosen)]),
        },
        &mut model,
    );
    assert_eq!(
        choices_of(&app, &model),
        [ThumbnailChoice::Automatic],
        "the file changed since"
    );
    let _ = app.update(
        Event::ThumbnailChoicesChanged {
            choices: ThumbnailChoices(vec![(
                "/x/clip.mp4".into(),
                ChosenFrame {
                    revision: 2,
                    ..chosen
                },
            )]),
        },
        &mut model,
    );
    assert_eq!(choices_of(&app, &model), [ThumbnailChoice::Chosen]);
}

#[test]
fn a_failed_video_says_why_and_a_late_frame_for_a_closed_dialog_is_dropped() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let _ = open_media(&app, &mut model, tab, "/x", &[media("clip.mp4", 1)]);
    let mut cmd = app.update(Event::SelectThumbnail { tab, row: 0 }, &mut model);
    let mut timing = request_of(&mut cmd, |effect| match effect {
        Effect::PickerTiming(request) => Some(request),
        _ => None,
    });
    timing
        .resolve(VideoTiming(Err("no video stream".into())))
        .expect("resolves");
    let event = cmd.events().next().expect("comes back");
    let mut next = app.update(event, &mut model);
    assert!(
        !next
            .effects()
            .any(|effect| matches!(effect, Effect::PickerFrame(_)))
    );
    let picker = app.view(&model).thumbnail_picker.expect("open");
    assert_eq!(
        picker.error,
        "The video's frames could not be extracted: no video stream"
    );
    assert_eq!(picker.status, "");

    // A frame of a dialog closed since changes nothing.
    let _ = app.update(Event::ClosePicker, &mut model);
    let _ = app.update(
        Event::PickerFrameMade {
            session: 1,
            at_ms: 0,
            file: Some("/cache/late.jpg".into()),
        },
        &mut model,
    );
    assert!(app.view(&model).thumbnail_picker.is_none());
}

#[test]
fn renamed_and_moved_items_keep_their_open_counts_and_chosen_thumbnails() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("a.mp4", false)]);
    click(&app, &mut model, tab, 0, false, false);
    let _ = app.update(Event::StartRename { tab }, &mut model);
    let mut cmd = app.update(
        Event::Rename {
            tab,
            name: "b.mp4".into(),
        },
        &mut model,
    );
    let mut done = finish(
        &app,
        &mut model,
        &mut cmd,
        crate::fileops::Outcome {
            undo: Some(crate::fileops::Undo::Rename {
                from: PathBuf::from("/x/a.mp4"),
                to: PathBuf::from("/x/b.mp4"),
            }),
            ..outcome(0, crate::fileops::Origin::User)
        },
    );
    let pairs = vec![("/x/a.mp4".to_owned(), "/x/b.mp4".to_owned())];
    let effects: Vec<Effect> = done.effects().collect();
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::OpenHistory(request) if request.operation == OpenHistoryOp::Rename(pairs.clone())
    )));
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::ThumbnailChoice(request) if request.operation == ThumbnailChoiceOp::Rename(pairs.clone())
    )));
}

/// A plugin as loading finds it, from its manifest.
fn plugin_code(manifest: &str, source: crate::plugins::Source) -> Arc<crate::plugins::PluginCode> {
    Arc::new(crate::plugins::PluginCode {
        manifest: crate::plugins::manifest::parse(manifest).expect("a valid manifest"),
        files: std::collections::BTreeMap::new(),
        source,
    })
}

fn plugin_controls(cmd: &mut Command<Effect, Event>) -> Vec<PluginControl> {
    cmd.effects()
        .filter_map(|effect| match effect {
            Effect::PluginControl(request) => Some(request.operation),
            _ => None,
        })
        .collect()
}

fn plugin_says(
    app: &DirFleet,
    model: &mut Model,
    message: crate::plugins::PluginMsg,
) -> Command<Effect, Event> {
    app.update(
        Event::PluginNoticed {
            notice: PluginNotice(message),
        },
        model,
    )
}

const BUNDLED_PLUGIN: &str = r#"
api_version = 1
id = "io.example.names"
name = "Names"
version = "1.0.0"
author = "Me"

[[action]]
id = "copy"
title = "Copy Names"
context_menu = "entry"
targets = { kind = "files" }

[[action]]
id = "ask"
title = "Ask First"
context_menu = "entry"
form = "static"

[[action.field]]
id = "name"
type = "text"
"#;

const USER_PLUGIN: &str = r#"
api_version = 1
id = "io.example.tidy"
name = "Tidy"
version = "0.1.0"
author = "Someone"
permissions = ["fs.write", "process:git"]
"#;

#[test]
fn plugins_start_when_enabled_and_approved_and_ask_again_when_they_want_more() {
    let app = DirFleet;
    let mut model = Model::default();
    let effects = start(&app, &mut model, None, LoadedState::Missing);
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::LoadPlugins(_))),
        "plugins load once the settings are known"
    );
    let found = PluginsFound {
        plugins: vec![
            plugin_code(BUNDLED_PLUGIN, crate::plugins::Source::Bundled),
            plugin_code(
                USER_PLUGIN,
                crate::plugins::Source::User("/plugins/tidy".into()),
            ),
        ],
        broken: vec![("/plugins/bad".into(), "plugin.toml:3: unknown field".into())],
        ffmpeg: None,
    };
    let mut cmd = app.update(Event::PluginsLoaded { found }, &mut model);
    assert_eq!(
        plugin_controls(&mut cmd),
        [PluginControl::Start {
            id: "io.example.names".into(),
            generation: 1
        }],
        "bundled plugins are on; others wait to be turned on"
    );
    let view = app.view(&model).plugins;
    assert_eq!(
        view.plugins
            .iter()
            .map(|p| (p.name.as_str(), p.status.as_str()))
            .collect::<Vec<_>>(),
        [("Names", "Starting…"), ("Tidy", "Off")]
    );
    assert_eq!(
        view.plugins[1].permissions,
        ["Change, move and delete files", "Run git"]
    );
    assert_eq!(view.broken[0].message, "plugin.toml:3: unknown field");

    // Turned on, a plugin asking for permissions waits for them to be allowed.
    let mut cmd = app.update(
        Event::SetPluginEnabled {
            id: "io.example.tidy".into(),
            enabled: true,
        },
        &mut model,
    );
    assert_eq!(plugin_controls(&mut cmd), Vec::new());
    assert!(app.view(&model).plugins.plugins[1].needs_approval);
    let mut cmd = app.update(
        Event::ApprovePlugin {
            id: "io.example.tidy".into(),
        },
        &mut model,
    );
    assert_eq!(
        plugin_controls(&mut cmd),
        [PluginControl::Start {
            id: "io.example.tidy".into(),
            generation: 2
        }]
    );
    let saved = DirFleet::persisted(&model);
    assert_eq!(saved.plugins["io.example.tidy"].enabled, Some(true));
    assert!(saved.plugins["io.example.tidy"].approved.is_some());

    // An update asking for more needs approval again.
    let grown = USER_PLUGIN.replace(r#""process:git"]"#, r#""process:git", "clipboard"]"#);
    let reloaded = |model: &mut Model, manifest: &str| {
        let found = PluginsFound {
            plugins: vec![
                plugin_code(BUNDLED_PLUGIN, crate::plugins::Source::Bundled),
                plugin_code(
                    manifest,
                    crate::plugins::Source::User("/plugins/tidy".into()),
                ),
            ],
            broken: Vec::new(),
            ffmpeg: None,
        };
        app.update(Event::PluginsLoaded { found }, model)
    };
    let mut cmd = reloaded(&mut model, &grown);
    assert!(
        !plugin_controls(&mut cmd).iter().any(|control| matches!(
            control,
            PluginControl::Start { id, .. } if id == "io.example.tidy"
        )),
        "the grown plugin doesn't start"
    );
    assert!(app.view(&model).plugins.plugins[1].needs_approval);
    let _ = reloaded(&mut model, USER_PLUGIN);
    assert_eq!(
        app.view(&model).plugins.plugins[1].status,
        "Starting…",
        "as approved before"
    );
}

#[test]
fn running_plugins_offer_their_actions_for_the_selection_and_run_them_on_their_worker() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[entry("a.txt", false), entry("sub", true)],
    );
    let found = PluginsFound {
        plugins: vec![plugin_code(BUNDLED_PLUGIN, crate::plugins::Source::Bundled)],
        broken: Vec::new(),
        ffmpeg: None,
    };
    let _ = app.update(Event::PluginsLoaded { found }, &mut model);

    // A late load of an earlier worker changes nothing; this one runs.
    let loaded = |generation| crate::plugins::PluginMsg::Loaded {
        plugin: "io.example.names".into(),
        generation,
        result: Ok(()),
    };
    let _ = plugin_says(&app, &mut model, loaded(7));
    assert_eq!(app.view(&model).plugins.plugins[0].status, "Starting…");
    let _ = plugin_says(&app, &mut model, loaded(1));
    assert_eq!(app.view(&model).plugins.plugins[0].status, "Running");

    // Its file action is offered for a file, not for a folder; the one for
    // any item, for both.
    assert!(
        shown(&app, &model).plugin_actions.is_empty(),
        "nothing selected"
    );
    click(&app, &mut model, tab, 0, false, false);
    let names = |model: &Model| -> Vec<String> {
        shown(&app, model)
            .plugin_actions
            .iter()
            .map(|action| action.title.clone())
            .collect()
    };
    assert_eq!(names(&model), ["Copy Names", "Ask First"]);
    click(&app, &mut model, tab, 1, false, false);
    assert_eq!(names(&model), ["Ask First"], "a folder isn't a file");
    click(&app, &mut model, tab, 0, false, false);

    let mut cmd = app.update(
        Event::RunPluginAction {
            tab,
            action: "io.example.names.copy".into(),
        },
        &mut model,
    );
    match plugin_controls(&mut cmd).as_slice() {
        [
            PluginControl::Send {
                id,
                request: crate::plugins::Request::Run { action, context },
            },
        ] => {
            assert_eq!((id.as_str(), action.as_str()), ("io.example.names", "copy"));
            assert_eq!(context.dir.as_deref(), Some(Path::new("/x")));
            assert_eq!(context.targets.len(), 1);
            assert_eq!(context.targets[0].path, Path::new("/x").join("a.txt"));
            assert_eq!(context.targets[0].size, 10);
        }
        other => panic!("the action runs on its worker: {other:?}"),
    }
}

#[test]
fn plugin_messages_become_toasts_navigation_and_file_operations() {
    use crate::plugins::{AppRequest, NotifyLevel, PluginMsg};
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("a.txt", false)]);
    let found = PluginsFound {
        plugins: vec![plugin_code(BUNDLED_PLUGIN, crate::plugins::Source::Bundled)],
        broken: Vec::new(),
        ffmpeg: None,
    };
    let _ = app.update(Event::PluginsLoaded { found }, &mut model);
    let _ = plugin_says(
        &app,
        &mut model,
        PluginMsg::Loaded {
            plugin: "io.example.names".into(),
            generation: 1,
            result: Ok(()),
        },
    );
    click(&app, &mut model, tab, 0, false, false);
    let _ = app.update(
        Event::RunPluginAction {
            tab,
            action: "io.example.names.copy".into(),
        },
        &mut model,
    );
    let texts = |model: &Model| -> Vec<String> {
        app.view(model)
            .toasts
            .iter()
            .map(|toast| toast.text.clone())
            .collect()
    };

    let _ = plugin_says(
        &app,
        &mut model,
        PluginMsg::App(AppRequest::Notify {
            level: NotifyLevel::Success,
            text: "Copied 1 name".into(),
        }),
    );
    assert_eq!(texts(&model), ["Copied 1 name"]);

    let failed = |message: &str| PluginMsg::ActionDone {
        plugin: "io.example.names".into(),
        result: Err(message.into()),
    };
    let _ = plugin_says(&app, &mut model, failed("main.lua:3: boom"));
    assert_eq!(
        texts(&model).last().map(String::as_str),
        Some("Names: main.lua:3: boom")
    );
    let _ = plugin_says(&app, &mut model, failed("again"));
    assert_eq!(texts(&model).len(), 2, "errors show once a minute at most");
    assert_eq!(
        app.view(&model).plugins.plugins[0].errors,
        ["main.lua:3: boom", "again"],
        "Settings keeps them all"
    );

    let mut cmd = plugin_says(
        &app,
        &mut model,
        PluginMsg::App(AppRequest::Open("/y".into())),
    );
    assert_eq!(
        cmd.events().next(),
        Some(Event::Navigate {
            tab,
            path: "/y".into()
        }),
        "in the tab the action ran in"
    );
    let mut cmd = plugin_says(
        &app,
        &mut model,
        PluginMsg::App(AppRequest::FileOp(crate::fileops::FileOp::Trash(vec![
            "/x/a.txt".into(),
        ]))),
    );
    assert!(
        cmd.effects()
            .any(|effect| matches!(effect, Effect::RunFileOp(_)))
    );
    let mut cmd = plugin_says(
        &app,
        &mut model,
        PluginMsg::App(AppRequest::Clipboard("a.txt".into())),
    );
    assert_eq!(
        system_ops(&mut cmd),
        vec![SystemOp::CopyText("a.txt".into())]
    );
}

const CONVERT_PLUGIN: &str = r#"
api_version = 1
id = "io.example.convert"
name = "Convert"
version = "1.0.0"
author = "Me"
max_jobs = 2

[[action]]
id = "rename"
title = "Rename…"
context_menu = "entry"
form = "static"

[[action.field]]
id = "name"
type = "text"
required = true

[[action.field]]
id = "start"
type = "time"

[[action]]
id = "ask"
title = "Ask Lua…"
context_menu = "entry"
form = "dynamic"

[[action]]
id = "encode"
title = "Encode…"
context_menu = "entry"
mode = "job"
per_item = true
"#;

/// A window on /x with a.txt selected and the convert plugin running.
fn convert_running(app: &DirFleet, model: &mut Model) -> u32 {
    let tab = window(app, model);
    open(
        app,
        model,
        tab,
        "/x",
        &[entry("a.txt", false), entry("b.txt", false)],
    );
    let found = PluginsFound {
        plugins: vec![plugin_code(CONVERT_PLUGIN, crate::plugins::Source::Bundled)],
        broken: Vec::new(),
        ffmpeg: None,
    };
    let _ = app.update(Event::PluginsLoaded { found }, model);
    let _ = plugin_says(
        app,
        model,
        crate::plugins::PluginMsg::Loaded {
            plugin: "io.example.convert".into(),
            generation: 1,
            result: Ok(()),
        },
    );
    click(app, model, tab, 0, false, false);
    tab
}

fn run_action(app: &DirFleet, model: &mut Model, tab: u32, action: &str) -> Command<Effect, Event> {
    app.update(
        Event::RunPluginAction {
            tab,
            action: format!("io.example.convert.{action}"),
        },
        model,
    )
}

fn field<'a>(form: &'a PluginFormView, id: &str) -> &'a FormFieldView {
    form.fields
        .iter()
        .find(|field| field.id == id)
        .expect("the field")
}

#[test]
fn a_plugin_form_is_checked_then_validated_by_the_plugin_and_remembered() {
    use crate::plugins::{PluginMsg, Request};
    let app = DirFleet;
    let mut model = Model::default();
    let tab = convert_running(&app, &mut model);
    assert_eq!(
        shown(&app, &model)
            .plugin_actions
            .iter()
            .map(|action| action.title.as_str())
            .collect::<Vec<_>>(),
        ["Rename…", "Ask Lua…", "Encode…"],
        "forms and jobs are offered now"
    );

    let mut cmd = run_action(&app, &mut model, tab, "rename");
    assert_eq!(
        plugin_controls(&mut cmd),
        Vec::new(),
        "the manifest's form opens at once"
    );
    let form = app.view(&model).plugin_form.expect("the form");
    assert_eq!(
        (form.title.as_str(), form.submit.as_str(), form.tab),
        ("Rename…", "Run", tab)
    );
    assert_eq!(field(&form, "name").kind, FormFieldKind::Text);
    assert_eq!(field(&form, "start").value, "00:00");

    // The form's own rules first: a required field.
    let mut cmd = app.update(Event::SubmitPluginForm, &mut model);
    assert_eq!(plugin_controls(&mut cmd), Vec::new());
    assert_eq!(
        field(&app.view(&model).plugin_form.expect("open"), "name").error,
        "Required"
    );
    for (id, text) in [("name", "clip"), ("start", "1:30")] {
        let _ = app.update(
            Event::SetPluginFormField {
                id: id.into(),
                text: text.into(),
            },
            &mut model,
        );
    }
    let form = app.view(&model).plugin_form.expect("open");
    assert_eq!(
        (
            field(&form, "name").error.as_str(),
            field(&form, "start").value.as_str()
        ),
        ("", "1:30")
    );

    // Then the plugin's.
    let mut cmd = app.update(Event::SubmitPluginForm, &mut model);
    let request = match plugin_controls(&mut cmd).as_slice() {
        [
            PluginControl::Send {
                request:
                    Request::Validate {
                        request, context, ..
                    },
                ..
            },
        ] => {
            let values = context.values.as_ref().expect("values");
            assert_eq!(values["name"], "clip");
            assert_eq!(values["start"], 90.0);
            *request
        }
        other => panic!("the plugin validates: {other:?}"),
    };
    assert!(app.view(&model).plugin_form.expect("open").busy);
    let _ = plugin_says(
        &app,
        &mut model,
        PluginMsg::Validated {
            request,
            result: Ok([
                ("name".to_owned(), "Taken".to_owned()),
                (String::new(), "Not now".to_owned()),
            ]
            .into()),
        },
    );
    let form = app.view(&model).plugin_form.expect("still open");
    assert_eq!(
        (field(&form, "name").error.as_str(), form.message.as_str()),
        ("Taken", "Not now")
    );
    assert!(!form.busy);
}

#[test]
fn a_form_the_plugin_accepts_runs_its_action_with_the_values_and_remembers_them() {
    use crate::plugins::{PluginMsg, Request};
    let app = DirFleet;
    let mut model = Model::default();
    let tab = convert_running(&app, &mut model);
    let _ = run_action(&app, &mut model, tab, "rename");
    let _ = app.update(
        Event::SetPluginFormField {
            id: "name".into(),
            text: "clip".into(),
        },
        &mut model,
    );

    let mut cmd = app.update(Event::SubmitPluginForm, &mut model);
    let request = match plugin_controls(&mut cmd).as_slice() {
        [
            PluginControl::Send {
                request: Request::Validate { request, .. },
                ..
            },
        ] => *request,
        other => panic!("validated again: {other:?}"),
    };
    let mut cmd = plugin_says(
        &app,
        &mut model,
        PluginMsg::Validated {
            request,
            result: Ok(std::collections::BTreeMap::new()),
        },
    );
    assert!(app.view(&model).plugin_form.is_none(), "it closes");
    match plugin_controls(&mut cmd).as_slice() {
        [
            PluginControl::Send {
                request: Request::Run { action, context },
                ..
            },
        ] => {
            assert_eq!(action, "rename");
            assert_eq!(context.values.as_ref().expect("values")["name"], "clip");
        }
        other => panic!("the action runs with the values: {other:?}"),
    }

    // Next time it starts from them.
    let _ = run_action(&app, &mut model, tab, "rename");
    let form = app.view(&model).plugin_form.expect("open again");
    assert_eq!(field(&form, "name").value, "clip");
    let _ = app.update(Event::CancelPluginForm, &mut model);
    assert!(app.view(&model).plugin_form.is_none());
    assert!(
        DirFleet::persisted(&model).plugins["io.example.convert"]
            .remembered
            .contains_key("rename")
    );
}

#[test]
fn a_form_lua_makes_shows_once_it_arrives_or_says_why_not() {
    use crate::plugins::{Form, PluginMsg, Request};
    let app = DirFleet;
    let mut model = Model::default();
    let tab = convert_running(&app, &mut model);
    let ask = |model: &mut Model| {
        let mut cmd = run_action(&app, model, tab, "ask");
        match plugin_controls(&mut cmd).as_slice() {
            [
                PluginControl::Send {
                    request:
                        Request::Form {
                            request, action, ..
                        },
                    ..
                },
            ] if action == "ask" => *request,
            other => panic!("Lua is asked for the form: {other:?}"),
        }
    };
    let request = ask(&mut model);
    assert!(app.view(&model).plugin_form.expect("open").loading);
    let form: Form = serde_json::from_value(serde_json::json!({
        "title": "Pick one",
        "submit": "Go",
        "fields": [
            { "id": "quality", "type": "choice", "options": [["low", "Low"], ["high", "High"]], "default": "high" },
            { "id": "fast", "type": "bool", "visible_if": { "field": "quality", "equals": "low" } }
        ]
    }))
    .expect("a form");
    let _ = plugin_says(
        &app,
        &mut model,
        PluginMsg::FormReady {
            request: request + 1,
            result: Ok(form.clone()),
        },
    );
    assert!(
        app.view(&model).plugin_form.expect("open").loading,
        "an answer to another request"
    );
    let _ = plugin_says(
        &app,
        &mut model,
        PluginMsg::FormReady {
            request,
            result: Ok(form),
        },
    );
    let view = app.view(&model).plugin_form.expect("open");
    assert_eq!(
        (view.title.as_str(), view.submit.as_str()),
        ("Pick one", "Go")
    );
    assert_eq!(
        view.fields
            .iter()
            .map(|f| f.id.as_str())
            .collect::<Vec<_>>(),
        ["quality"],
        "fast is hidden"
    );
    assert_eq!(field(&view, "quality").options.len(), 2);
    let _ = app.update(
        Event::SetPluginFormField {
            id: "quality".into(),
            text: "low".into(),
        },
        &mut model,
    );
    let _ = app.update(
        Event::SetPluginFormField {
            id: "fast".into(),
            text: "true".into(),
        },
        &mut model,
    );
    let view = app.view(&model).plugin_form.expect("open");
    assert!(field(&view, "fast").on, "shown once quality is low");

    let _ = app.update(Event::CancelPluginForm, &mut model);
    let request = ask(&mut model);
    let _ = plugin_says(
        &app,
        &mut model,
        PluginMsg::FormReady {
            request,
            result: Err("main.lua:9: oops".into()),
        },
    );
    assert_eq!(
        app.view(&model).plugin_form.expect("open").error,
        "main.lua:9: oops"
    );
}

#[test]
fn plugin_jobs_wait_their_turn_report_progress_and_say_how_they_went() {
    use crate::plugins::{JobEvent, PluginMsg};
    let app = DirFleet;
    let mut model = Model::default();
    let tab = convert_running(&app, &mut model);
    let started = |cmd: &mut Command<Effect, Event>| -> Vec<u64> {
        plugin_controls(cmd)
            .into_iter()
            .filter_map(|control| match control {
                PluginControl::StartJob {
                    job,
                    action,
                    per_item,
                    ..
                } => {
                    assert_eq!(action, "encode");
                    assert!(per_item);
                    Some(job)
                }
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        started(&mut run_action(&app, &mut model, tab, "encode")),
        [1]
    );
    assert_eq!(
        started(&mut run_action(&app, &mut model, tab, "encode")),
        [2]
    );
    assert_eq!(
        started(&mut run_action(&app, &mut model, tab, "encode")),
        Vec::<u64>::new(),
        "two at once"
    );
    let jobs = app.view(&model).plugin_jobs;
    assert_eq!(
        jobs.iter().map(|job| job.waiting).collect::<Vec<_>>(),
        [false, false, true]
    );
    assert_eq!(jobs[0].title, "Encode: a.txt");

    let _ = plugin_says(
        &app,
        &mut model,
        PluginMsg::Job {
            id: 1,
            event: JobEvent::Progress {
                fraction: 0.25,
                label: "a.txt".into(),
            },
        },
    );
    let job = &app.view(&model).plugin_jobs[0];
    assert_eq!((job.progress, job.label.as_str()), (250, "a.txt"));

    let mut cmd = plugin_says(
        &app,
        &mut model,
        PluginMsg::Job {
            id: 1,
            event: JobEvent::Finished {
                done: 3,
                skipped: 1,
                failures: vec!["b: bad".into()],
                cancelled: false,
            },
        },
    );
    assert_eq!(started(&mut cmd), [3], "the waiting one starts");
    let toast = app.view(&model).toasts.last().cloned().expect("a toast");
    assert_eq!(
        (toast.kind, toast.text.as_str()),
        (
            ToastKind::Error,
            "Encode: a.txt: 3 done, 1 skipped, 1 failed"
        )
    );

    let mut cmd = app.update(Event::CancelPluginJob { id: 2 }, &mut model);
    assert_eq!(
        plugin_controls(&mut cmd),
        [PluginControl::CancelJob { job: 2 }]
    );
    let _ = run_action(&app, &mut model, tab, "encode");
    let _ = app.update(Event::CancelPluginJob { id: 4 }, &mut model);
    assert_eq!(
        app.view(&model).plugin_jobs.len(),
        2,
        "a waiting job just goes"
    );
}

const GIT_PLUGIN: &str = r#"
api_version = 1
id = "io.example.git"
name = "Git"
version = "1.0.0"
author = "Me"

[activation]
ancestor_contains = [".git"]

[[action]]
id = "fetch"
title = "Git: Fetch"
targets = { kind = "current_dir" }
context_menu = "background"
mode = "job"
"#;

const USAGE_PLUGIN: &str = r#"
api_version = 1
id = "io.example.usage"
name = "Usage"
version = "1.0.0"
author = "Me"

[activation]
always = true

[[action]]
id = "scan"
title = "Folder Usage…"
targets = { kind = "current_dir" }
context_menu = "background"
mode = "job"
"#;

fn activation_checks(cmd: &mut Command<Effect, Event>) -> Vec<Request<CheckActivation>> {
    cmd.effects()
        .filter_map(|effect| match effect {
            Effect::CheckActivation(request) => Some(request),
            _ => None,
        })
        .collect()
}

#[test]
fn folder_actions_show_where_their_activation_holds_and_act_on_the_folder() {
    use crate::plugins::PluginMsg;
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/repo", &[entry("a.txt", false)]);
    let found = PluginsFound {
        plugins: vec![
            plugin_code(GIT_PLUGIN, crate::plugins::Source::Bundled),
            plugin_code(USAGE_PLUGIN, crate::plugins::Source::Bundled),
        ],
        broken: Vec::new(),
        ffmpeg: None,
    };
    let _ = app.update(Event::PluginsLoaded { found }, &mut model);
    let loaded = |plugin: &str, generation| PluginMsg::Loaded {
        plugin: plugin.into(),
        generation,
        result: Ok(()),
    };
    let _ = plugin_says(&app, &mut model, loaded("io.example.usage", 2));
    let mut cmd = plugin_says(&app, &mut model, loaded("io.example.git", 1));
    let folder_actions = |model: &Model| -> Vec<String> {
        shown(&app, model)
            .folder_plugin_actions
            .iter()
            .map(|action| action.title.clone())
            .collect()
    };
    assert_eq!(
        folder_actions(&model),
        ["Folder Usage…"],
        "always applies, without looking"
    );

    let mut checks = activation_checks(&mut cmd);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].operation.dir, "/repo");
    assert_eq!(
        checks[0]
            .operation
            .plugins
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        ["io.example.git"]
    );
    checks[0]
        .resolve(ActivatedPlugins(vec!["io.example.git".into()]))
        .expect("resolves");
    let event = cmd.events().next().expect("the check comes back");
    let mut cmd = app.update(event, &mut model);
    assert_eq!(folder_actions(&model), ["Git: Fetch", "Folder Usage…"]);
    assert!(activation_checks(&mut cmd).is_empty(), "checked once");

    // It acts on the folder, whatever is selected.
    click(&app, &mut model, tab, 0, false, false);
    let mut cmd = app.update(
        Event::RunPluginAction {
            tab,
            action: "io.example.git.fetch".into(),
        },
        &mut model,
    );
    match plugin_controls(&mut cmd).as_slice() {
        [PluginControl::StartJob { context, .. }] => {
            assert_eq!(context.dir.as_deref(), Some(Path::new("/repo")));
            assert_eq!(context.targets, Vec::new());
        }
        other => panic!("a job on the folder: {other:?}"),
    }
    let job = &app.view(&model).plugin_jobs[0];
    assert_eq!(job.title, "Git: Fetch");

    // Read again, the folder is looked at again.
    let mut cmd = app.update(Event::Refresh { tab }, &mut model);
    let mut list = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::ListDir(request) => Some(request),
            _ => None,
        })
        .expect("listed again");
    let view = list.operation.view.clone();
    list.resolve(listing(&[entry("a.txt", false)], view))
        .expect("resolves");
    let event = cmd.events().next().expect("comes back");
    let mut cmd = app.update(event, &mut model);
    assert_eq!(activation_checks(&mut cmd).len(), 1);
}

const SCAN_PLUGIN: &str = r#"
api_version = 1
id = "io.example.scan"
name = "Scan"
version = "1.0.0"
author = "Me"

[activation]
always = true

[[action]]
id = "scan"
title = "Folder Usage…"
targets = { kind = "current_dir" }
context_menu = "background"
form = "static"
mode = "job"
results = true

[[action.field]]
id = "depth"
label = "Depth"
type = "int"
default = 2

[[action]]
id = "remove"
title = "Remove folder"
targets = { kind = "current_dir" }
mode = "job"

[[action]]
id = "measure"
title = "Measuring folder"
targets = { kind = "current_dir" }
mode = "job"
"#;

fn started_jobs(cmd: &mut Command<Effect, Event>) -> Vec<(u64, String, Option<PathBuf>)> {
    plugin_controls(cmd)
        .into_iter()
        .filter_map(|control| match control {
            PluginControl::StartJob {
                job,
                action,
                context,
                ..
            } => Some((job, action, context.dir)),
            _ => None,
        })
        .collect()
}

fn job_ends(
    app: &DirFleet,
    model: &mut Model,
    id: u64,
    failures: Vec<String>,
) -> Command<Effect, Event> {
    plugin_says(
        app,
        model,
        crate::plugins::PluginMsg::Job {
            id,
            event: crate::plugins::JobEvent::Finished {
                done: 1,
                skipped: 0,
                failures,
                cancelled: false,
            },
        },
    )
}

fn results(app: &DirFleet, model: &Model) -> PluginResultsView {
    app.view(model).plugin_results.expect("the results dialog")
}

/// The scan plugin running, with its results dialog open on /data.
fn scan_dialog(app: &DirFleet, model: &mut Model) -> u32 {
    let tab = window(app, model);
    open(app, model, tab, "/data", &[]);
    let found = PluginsFound {
        plugins: vec![plugin_code(SCAN_PLUGIN, crate::plugins::Source::Bundled)],
        broken: Vec::new(),
        ffmpeg: None,
    };
    let _ = app.update(Event::PluginsLoaded { found }, model);
    let _ = plugin_says(
        app,
        model,
        crate::plugins::PluginMsg::Loaded {
            plugin: "io.example.scan".into(),
            generation: 1,
            result: Ok(()),
        },
    );
    let _ = app.update(
        Event::RunPluginAction {
            tab,
            action: "io.example.scan.scan".into(),
        },
        model,
    );
    tab
}

/// Run, validated by the plugin: what the core asks for next.
fn run_scan(app: &DirFleet, model: &mut Model) -> Command<Effect, Event> {
    use crate::plugins::{PluginMsg, Request};
    let mut cmd = app.update(Event::RunPluginResults, model);
    let request = match plugin_controls(&mut cmd).as_slice() {
        [
            PluginControl::Send {
                request: Request::Validate { request, .. },
                ..
            },
        ] => *request,
        other => panic!("validated first: {other:?}"),
    };
    plugin_says(
        app,
        model,
        PluginMsg::Validated {
            request,
            result: Ok(std::collections::BTreeMap::new()),
        },
    )
}

/// The dialog's first run sent its table: big (3 GB, with a menu) and
/// small (1 MB), sorted by size.
fn scan_with_table(app: &DirFleet, model: &mut Model) -> u32 {
    use crate::plugins::{AppRequest, PluginMsg, TableSpec};
    let tab = scan_dialog(app, model);
    let _ = run_scan(app, model);
    let spec: TableSpec = serde_json::from_value(serde_json::json!({
        "title": "Folder Usage",
        "subtitle": "{count} folders · {total}",
        "progress": "Scanning 2 of 2",
        "refresh_action": "measure",
        "columns": [
            { "id": "name", "label": "Folder" },
            { "id": "size", "label": "Size", "kind": "size" }
        ],
        "rows": [
            { "path": "/data/small", "cells": { "name": "small", "size": 1_048_576 } },
            { "path": "/data/big", "cells": { "name": "big", "size": 3_221_225_472_u64 },
              "menu": [{ "label": "Move to Trash", "action": "remove", "danger": true, "busy": "Removing…" }] }
        ],
        "sort": { "column": "size", "descending": true }
    }))
    .expect("a table");
    let _ = plugin_says(
        app,
        model,
        PluginMsg::App(AppRequest::ShowTable {
            plugin: "io.example.scan".into(),
            job: Some(1),
            spec,
        }),
    );
    tab
}

#[test]
fn a_results_dialog_opens_with_its_options_and_runs_once_validated() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = scan_dialog(&app, &mut model);
    let view = results(&app, &model);
    assert_eq!(
        (view.title.as_str(), view.tab, view.running),
        ("Folder Usage: data", tab, false)
    );
    assert_eq!(view.fields[0].value, "2");
    assert_eq!(
        view.empty_text,
        "Press Run to see what takes the most space."
    );

    let mut cmd = run_scan(&app, &mut model);
    assert_eq!(
        started_jobs(&mut cmd),
        [(1, "scan".to_owned(), Some(PathBuf::from("/data")))]
    );
    let view = results(&app, &model);
    assert!(view.running);
    assert_eq!(view.empty_text, "Results appear here as they are found.");
    assert_eq!(
        app.view(&model).plugin_jobs[0].results,
        "io.example.scan.scan"
    );
}

#[test]
fn a_results_table_sorts_filters_and_waits_for_its_job_before_row_actions() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = scan_with_table(&app, &mut model);
    let view = results(&app, &model);
    assert_eq!(view.subtitle, "2 folders · 3.00 GB");
    assert_eq!(view.progress_text, "Scanning 2 of 2");
    assert_eq!(
        view.columns
            .iter()
            .map(|c| (c.title.as_str(), c.sorted))
            .collect::<Vec<_>>(),
        [("Folder", false), ("Size", true)]
    );
    assert_eq!(
        view.rows
            .iter()
            .map(|r| r.cells[0].as_str())
            .collect::<Vec<_>>(),
        ["big", "small"]
    );
    assert!(!view.rows[0].actions_enabled, "rows wait for the scan");
    assert_eq!(view.refresh_label, "Measure again");

    let _ = job_ends(&app, &mut model, 1, Vec::new());
    let view = results(&app, &model);
    assert!(!view.running && view.rows[0].actions_enabled);
    assert_eq!(
        app.view(&model).toasts.last().map(|t| t.text.clone()),
        Some("Folder Usage: done".into())
    );

    let _ = app.update(
        Event::FilterPluginResults {
            amount: "1".into(),
            gigabytes: true,
        },
        &mut model,
    );
    let view = results(&app, &model);
    assert_eq!(
        (view.rows.len(), view.filter_text.as_str()),
        (1, "Showing 1 of 2")
    );
    let _ = app.update(
        Event::FilterPluginResults {
            amount: String::new(),
            gigabytes: true,
        },
        &mut model,
    );
    let _ = app.update(Event::SortPluginResults { column: 0 }, &mut model);
    assert_eq!(
        results(&app, &model).rows[0].cells[0],
        "big",
        "names start ascending"
    );
}

#[test]
fn a_result_row_runs_its_action_is_measured_again_and_opens() {
    use crate::plugins::{AppRequest, PluginMsg, RowUpdate};
    let app = DirFleet;
    let mut model = Model::default();
    let _ = scan_with_table(&app, &mut model);
    let _ = job_ends(&app, &mut model, 1, Vec::new());
    // A row's action runs on its folder, and the row says so; then it is
    // measured again, quietly.
    let mut cmd = app.update(
        Event::PluginResultsRowAction {
            path: "/data/big".into(),
            action: "remove".into(),
        },
        &mut model,
    );
    assert_eq!(
        started_jobs(&mut cmd),
        [(2, "remove".to_owned(), Some(PathBuf::from("/data/big")))]
    );
    let row = |model: &Model| {
        results(&app, model)
            .rows
            .into_iter()
            .find(|r| r.path == "/data/big")
    };
    assert_eq!(row(&model).map(|r| r.status), Some("Removing…".into()));
    let mut cmd = job_ends(&app, &mut model, 2, Vec::new());
    assert_eq!(
        started_jobs(&mut cmd),
        [(3, "measure".to_owned(), Some(PathBuf::from("/data/big")))]
    );
    assert_eq!(row(&model).map(|r| r.status), Some("Measuring…".into()));
    let _ = plugin_says(
        &app,
        &mut model,
        PluginMsg::App(AppRequest::UpdateTableRow {
            plugin: "io.example.scan".into(),
            update: RowUpdate {
                payload: "/data/big".into(),
                cells: std::collections::BTreeMap::new(),
                clear: Vec::new(),
                remove: true,
            },
        }),
    );
    let toasts = app.view(&model).toasts.len();
    let _ = job_ends(&app, &mut model, 3, Vec::new());
    assert!(row(&model).is_none(), "gone");
    assert_eq!(
        app.view(&model).toasts.len(),
        toasts,
        "measuring says nothing"
    );

    // Hidden, it comes back as it was; a row opens in a new tab.
    let _ = app.update(Event::HidePluginResults, &mut model);
    assert!(app.view(&model).plugin_results.is_none());
    let _ = app.update(
        Event::ShowPluginResults {
            key: "io.example.scan.scan".into(),
        },
        &mut model,
    );
    assert_eq!(results(&app, &model).rows.len(), 1);
    let mut cmd = app.update(
        Event::OpenPluginResultsRow {
            path: "/data/small".into(),
        },
        &mut model,
    );
    assert!(
        cmd.effects()
            .any(|effect| matches!(effect, Effect::OpenWindow(_)))
    );
    assert!(app.view(&model).plugin_results.is_none());
}

const BAR_PLUGIN: &str = r#"
api_version = 1
id = "io.example.status"
name = "Status"
version = "1.0.0"
author = "Me"

[activation]
always = true

[bar]
refresh_secs = 60

[inspector]
title = "Status"
targets = { kind = "files", max = 1 }

[events]
subscribe = ["app_started", "folder_opened", "selection_changed", "fs_changed", "job_finished", "file_op_finished"]

[[action]]
id = "sync"
title = "Sync"
targets = { kind = "current_dir" }
mode = "job"
"#;

fn worker_requests(cmd: &mut Command<Effect, Event>) -> Vec<crate::plugins::Request> {
    plugin_controls(cmd)
        .into_iter()
        .filter_map(|control| match control {
            PluginControl::Send { request, .. } => Some(request),
            _ => None,
        })
        .collect()
}

fn events_sent(cmd: &mut Command<Effect, Event>) -> Vec<String> {
    worker_requests(cmd)
        .into_iter()
        .filter_map(|request| match request {
            crate::plugins::Request::Event { name, .. } => Some(name),
            _ => None,
        })
        .collect()
}

/// A window on /x and the status plugin started: its `app_started` and the
/// first bar asked for.
fn status_running(app: &DirFleet, model: &mut Model) -> (u32, Command<Effect, Event>) {
    let tab = window(app, model);
    open(
        app,
        model,
        tab,
        "/x",
        &[entry("a.txt", false), entry("b.txt", false)],
    );
    let found = PluginsFound {
        plugins: vec![plugin_code(BAR_PLUGIN, crate::plugins::Source::Bundled)],
        broken: Vec::new(),
        ffmpeg: None,
    };
    let _ = app.update(Event::PluginsLoaded { found }, model);
    let cmd = plugin_says(
        app,
        model,
        crate::plugins::PluginMsg::Loaded {
            plugin: "io.example.status".into(),
            generation: 1,
            result: Ok(()),
        },
    );
    (tab, cmd)
}

#[test]
fn a_plugin_bar_is_asked_for_shown_and_asked_again_when_stale() {
    use crate::plugins::{BarSegment, PluginMsg, Request};
    let app = DirFleet;
    let mut model = Model::default();
    let (_, mut cmd) = status_running(&app, &mut model);
    let requests = worker_requests(&mut cmd);
    assert!(
        requests
            .iter()
            .any(|r| matches!(r, Request::Event { name, .. } if name == "app_started"))
    );
    assert!(
        requests
            .iter()
            .any(|r| matches!(r, Request::Bar { dir, .. } if dir == Path::new("/x"))),
        "{requests:?}"
    );
    assert!(
        !requests
            .iter()
            .any(|r| matches!(r, Request::Inspector { .. })),
        "the inspector is hidden"
    );
    assert_eq!(shown(&app, &model).plugin_bar[0].plugin, "Status");
    assert!(shown(&app, &model).plugin_bar[0].loading);

    let segments: Vec<BarSegment> = serde_json::from_value(serde_json::json!([
        { "text": "main", "icon": "git-branch" },
        { "badge": "3", "tone": "warn" },
        { "button": "Sync", "action": "sync" }
    ]))
    .expect("segments");
    let mut cmd = plugin_says(
        &app,
        &mut model,
        PluginMsg::Bar {
            plugin: "io.example.status".into(),
            dir: "/x".into(),
            result: Ok(segments),
        },
    );
    assert!(worker_requests(&mut cmd).is_empty(), "nothing more to ask");
    let bar = &shown(&app, &model).plugin_bar[0];
    assert!(!bar.loading);
    assert_eq!(
        bar.segments
            .iter()
            .map(|s| (s.text.as_str(), s.badge.as_str(), s.action.as_str()))
            .collect::<Vec<_>>(),
        [
            ("main", "", ""),
            ("", "3", ""),
            ("", "", "io.example.status.sync")
        ]
    );

    // It acted: asked again. The clock passing its refresh: again too.
    let mut cmd = plugin_says(
        &app,
        &mut model,
        PluginMsg::ActionDone {
            plugin: "io.example.status".into(),
            result: Ok(()),
        },
    );
    assert!(
        worker_requests(&mut cmd)
            .iter()
            .any(|r| matches!(r, Request::Bar { .. }))
    );
    let _ = plugin_says(
        &app,
        &mut model,
        PluginMsg::Bar {
            plugin: "io.example.status".into(),
            dir: "/x".into(),
            result: Ok(Vec::new()),
        },
    );
    let mut cmd = app.update(Event::ClockTicked { now: 1_000_000 }, &mut model);
    assert!(
        worker_requests(&mut cmd).is_empty(),
        "made before the clock ticked: its time starts now"
    );
    let mut cmd = app.update(Event::ClockTicked { now: 1_000_030 }, &mut model);
    assert_eq!(worker_requests(&mut cmd), Vec::new());
    let mut cmd = app.update(Event::ClockTicked { now: 1_000_060 }, &mut model);
    assert!(
        worker_requests(&mut cmd)
            .iter()
            .any(|r| matches!(r, Request::Bar { .. })),
        "its refresh_secs passed"
    );
}

#[test]
fn a_plugin_inspector_section_follows_the_selection() {
    use crate::plugins::{InspectorRow, PluginMsg, Request};
    let app = DirFleet;
    let mut model = Model::default();
    let (tab, _) = status_running(&app, &mut model);
    let _ = app.update(Event::SetInspector { shown: true }, &mut model);
    let mut cmd = app.update(
        Event::Click {
            tab,
            row: 0,
            command: false,
            shift: false,
        },
        &mut model,
    );
    let key = worker_requests(&mut cmd)
        .into_iter()
        .find_map(|r| match r {
            Request::Inspector { key, context } => {
                assert_eq!(context.targets[0].path, Path::new("/x").join("a.txt"));
                Some(key)
            }
            _ => None,
        })
        .expect("the section is asked for");
    let _ = plugin_says(
        &app,
        &mut model,
        PluginMsg::Inspector {
            plugin: "io.example.status".into(),
            key,
            result: Ok(vec![InspectorRow::Pair("Branch".into(), "main".into())]),
        },
    );
    let sections = shown(&app, &model).plugin_sections;
    assert_eq!(sections[0].title, "Status");
    assert_eq!(
        (
            sections[0].rows[0].label.as_str(),
            sections[0].rows[0].value.as_str()
        ),
        ("Branch", "main")
    );

    // Two files: the section is for one at most.
    let _ = app.update(
        Event::Click {
            tab,
            row: 1,
            command: true,
            shift: false,
        },
        &mut model,
    );
    assert_eq!(shown(&app, &model).plugin_sections, Vec::new());
}

#[test]
fn plugins_hear_the_events_they_subscribe_to() {
    use crate::plugins::{JobEvent, PluginMsg};
    let app = DirFleet;
    let mut model = Model::default();
    let (tab, _) = status_running(&app, &mut model);

    let mut cmd = app.update(
        Event::Navigate {
            tab,
            path: "/y".into(),
        },
        &mut model,
    );
    assert!(events_sent(&mut cmd).contains(&"folder_opened".to_owned()));

    let mut cmd = app.update(
        Event::Click {
            tab,
            row: 0,
            command: false,
            shift: false,
        },
        &mut model,
    );
    let mut delay = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::Delay(request) if request.operation.millis == 250 => Some(request),
            _ => None,
        })
        .expect("the selection rests first");
    delay.resolve(()).expect("resolves");
    let settled = cmd
        .events()
        .find(|event| matches!(event, Event::SelectionSettled { .. }))
        .expect("settled");
    let _ = app.update(Event::SelectAll { tab }, &mut model);
    assert!(
        events_sent(&mut app.update(settled, &mut model)).is_empty(),
        "it moved again meanwhile"
    );

    let generation = model.generation;
    let mut cmd = app.update(
        Event::Watched {
            tab,
            generation,
            event: WatchEvent::Changed,
        },
        &mut model,
    );
    assert!(events_sent(&mut cmd).contains(&"fs_changed".to_owned()));

    let _ = app.update(
        Event::RunPluginAction {
            tab,
            action: "io.example.status.sync".into(),
        },
        &mut model,
    );
    let mut cmd = plugin_says(
        &app,
        &mut model,
        PluginMsg::Job {
            id: 1,
            event: JobEvent::Finished {
                done: 1,
                skipped: 0,
                failures: Vec::new(),
                cancelled: false,
            },
        },
    );
    assert!(events_sent(&mut cmd).contains(&"job_finished".to_owned()));

    // Told once that the app started, not again when it restarts.
    let mut cmd = plugin_says(
        &app,
        &mut model,
        PluginMsg::Loaded {
            plugin: "io.example.status".into(),
            generation: 1,
            result: Ok(()),
        },
    );
    assert!(!events_sent(&mut cmd).contains(&"app_started".to_owned()));
}

#[test]
fn the_palette_offers_plugin_actions_that_apply() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = status_running(&app, &mut model);
    let _ = app.update(
        Event::SetPaletteText {
            text: "sync".into(),
        },
        &mut model,
    );
    let palette = app.view(&model).palette.expect("open");
    assert_eq!(
        palette.items.first().map(|item| (
            item.title.as_str(),
            item.detail.as_str(),
            item.target.clone()
        )),
        Some((
            "Sync",
            "Status",
            PaletteTarget::PluginAction("io.example.status.sync".into())
        ))
    );
}

const SHORTCUT_PLUGIN: &str = r#"
api_version = 1
id = "io.example.keys"
name = "Keys"
version = "1.0.0"
author = "Me"

[[action]]
id = "tidy"
title = "Tidy"
shortcut = "Ctrl+Alt+M"
context_menu = "entry"

[[action]]
id = "refresh"
title = "Refresh Twice"
shortcut = "Ctrl+R"

[[action]]
id = "plain"
title = "Plain"
"#;

/// A window on `/x` with the shortcut plugin running; its tab.
fn keys_running(app: &DirFleet, model: &mut Model) -> u32 {
    let tab = window(app, model);
    open(app, model, tab, "/x", &[entry("a.txt", false)]);
    let found = PluginsFound {
        plugins: vec![plugin_code(
            SHORTCUT_PLUGIN,
            crate::plugins::Source::Bundled,
        )],
        broken: Vec::new(),
        ffmpeg: None,
    };
    let _ = app.update(Event::PluginsLoaded { found }, model);
    let _ = plugin_says(
        app,
        model,
        crate::plugins::PluginMsg::Loaded {
            plugin: "io.example.keys".into(),
            generation: 1,
            result: Ok(()),
        },
    );
    tab
}

#[test]
fn plugin_shortcuts_bind_unless_they_clash() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = keys_running(&app, &mut model);
    let (tidy, refresh) = if cfg!(target_os = "macos") {
        ("⌥⌘M", "⌘R")
    } else {
        ("Ctrl+Alt+M", "F5")
    };
    let view = app.view(&model);
    let bound: Vec<(&str, &str)> = view
        .plugin_shortcuts
        .iter()
        .map(|binding| (binding.action.as_str(), binding.text.as_str()))
        .collect();
    assert_eq!(bound, [("io.example.keys.tidy", tidy)]);
    let binding = &view.plugin_shortcuts[0];
    assert_eq!(
        (
            binding.key.as_str(),
            binding.command,
            binding.alt,
            binding.shift
        ),
        ("M", true, true, false)
    );
    let settings = &view.plugins.plugins[0].shortcuts;
    assert_eq!(
        settings
            .iter()
            .map(|s| (s.title.as_str(), s.text.as_str()))
            .collect::<Vec<_>>(),
        [
            ("Tidy", "Ctrl+Alt+M"),
            ("Refresh Twice", "Ctrl+R"),
            ("Plain", "")
        ]
    );
    if cfg!(target_os = "macos") {
        assert_eq!(
            settings[1].note,
            format!("{refresh} is used by \u{201c}Refresh\u{201d}")
        );
    }
    click(&app, &mut model, tab, 0, false, false);
    assert_eq!(
        shown(&app, &model).plugin_actions[0].shortcut,
        tidy,
        "menus show it"
    );
}

#[test]
fn plugin_shortcuts_can_be_changed() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = keys_running(&app, &mut model);
    let tidy = if cfg!(target_os = "macos") {
        "⌥⌘M"
    } else {
        "Ctrl+Alt+M"
    };
    // The user's own replaces the manifest's; the manifest's again removes it.
    let set = |model: &mut Model, action: &str, text: &str| {
        let _ = app.update(
            Event::SetPluginShortcut {
                plugin: "io.example.keys".into(),
                action: action.into(),
                text: text.into(),
            },
            model,
        );
    };
    set(&mut model, "plain", "Ctrl+Alt+M");
    let view = app.view(&model);
    assert_eq!(
        view.plugins.plugins[0].shortcuts[2].note,
        format!("{tidy} is used by another plugin action")
    );
    set(&mut model, "tidy", "");
    let view = app.view(&model);
    assert_eq!(
        view.plugin_shortcuts
            .iter()
            .map(|b| b.action.as_str())
            .collect::<Vec<_>>(),
        ["io.example.keys.plain"],
        "an empty one takes the default away"
    );
    set(&mut model, "tidy", "Ctrl+Alt+M");
    assert!(
        !DirFleet::persisted(&model).plugins["io.example.keys"]
            .shortcuts
            .contains_key("tidy")
    );
    set(&mut model, "plain", "Ctrl+Nope");
    assert_eq!(
        app.view(&model).plugins.plugins[0].shortcuts[2].note,
        "unknown key \"Nope\" in \"Ctrl+Nope\""
    );
}

#[test]
fn an_edited_plugin_folder_loads_the_plugins_again() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = window(&app, &mut model);
    let mut cmd = app.update(
        Event::PluginFolderChanged {
            folder: "/plugins/io.example.keys".into(),
        },
        &mut model,
    );
    assert!(
        cmd.effects()
            .any(|effect| matches!(effect, Effect::LoadPlugins(_)))
    );
    assert_eq!(
        app.view(&model)
            .toasts
            .last()
            .map(|toast| toast.text.clone()),
        Some("Plugin reloaded: /plugins/io.example.keys".into())
    );
}

#[test]
fn plugins_that_use_ffmpeg_say_whether_it_is_there() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = window(&app, &mut model);
    let video = SHORTCUT_PLUGIN.replace(
        "author = \"Me\"",
        "author = \"Me\"\npermissions = [\"ffmpeg\"]",
    );
    let found = |ffmpeg: Option<&str>| PluginsFound {
        plugins: vec![
            plugin_code(&video, crate::plugins::Source::Bundled),
            plugin_code(BAR_PLUGIN, crate::plugins::Source::Bundled),
        ],
        broken: Vec::new(),
        ffmpeg: ffmpeg.map(str::to_owned),
    };
    let _ = app.update(Event::PluginsLoaded { found: found(None) }, &mut model);
    let lines = |model: &Model| -> Vec<(String, String)> {
        app.view(model)
            .plugins
            .plugins
            .iter()
            .map(|p| (p.ffmpeg.clone(), p.ffmpeg_path.clone()))
            .collect()
    };
    assert!(lines(&model).contains(&(
        "Not installed yet: it downloads the first time it's needed".to_owned(),
        String::new()
    )));
    assert!(
        lines(&model).contains(&(String::new(), String::new())),
        "one without the permission says nothing"
    );
    let _ = app.update(
        Event::PluginsLoaded {
            found: found(Some("/bin/ffmpeg")),
        },
        &mut model,
    );
    assert!(lines(&model).contains(&("Found".to_owned(), "/bin/ffmpeg".to_owned())));
}

#[test]
fn thumbnails_that_failed_are_asked_again_once_ffmpeg_is_ready() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let mut asked = open_media(&app, &mut model, tab, "/x", &[media("clip.mp4", 1)]);
    let _ = asked.answer(&app, &mut model, "/x/clip.mp4", None);
    let again = Asked::new(app.update(Event::Refresh { tab }, &mut model));
    assert!(
        again.requests.is_empty(),
        "a failure isn't retried on its own"
    );

    let notice = |kind| Event::MediaNoticed {
        notice: MediaNotice {
            kind,
            text: "FFmpeg is ready".into(),
        },
    };
    let info = Asked::new(app.update(notice(ToastKind::Info), &mut model));
    assert!(info.requests.is_empty());
    let ready = Asked::new(app.update(notice(ToastKind::Success), &mut model));
    assert_eq!(
        ready.paths(),
        [("/x/clip.mp4".to_owned(), thumbnails::LIST_EDGE)]
    );
}

#[test]
#[allow(clippy::too_many_lines, reason = "one story, told in order")]
fn a_server_s_items_are_made_renamed_copied_beside_and_removed_there() {
    use crate::fileops::{ConflictPolicy, FileOp, Origin, Outcome};
    let app = DirFleet;
    let mut model = Model::default();
    // Each is looked at alone here: none waits behind the one before.
    model.settings.queue_mode = QueueMode::Unlimited;
    let tab = window(&app, &mut model);
    let docs = "sftp://c1/docs";
    open(
        &app,
        &mut model,
        tab,
        docs,
        &[entry("a.md", false), entry("b.md", false)],
    );
    let on = |name: &str| PathBuf::from(format!("{docs}/{name}"));
    let ran =
        |cmd: &mut Command<Effect, Event>| file_ops(cmd).pop().map(|request| request.operation.op);

    let mut cmd = app.update(Event::NewFolder { tab }, &mut model);
    assert_eq!(
        ran(&mut cmd),
        Some(FileOp::NewFolder {
            parent: PathBuf::from(docs)
        })
    );

    // A new name stays on the server, joined as the server joins.
    click(&app, &mut model, tab, 0, false, false);
    let _ = app.update(Event::StartRename { tab }, &mut model);
    assert_eq!(
        shown(&app, &model).renaming.map(|renaming| renaming.name),
        Some("a.md".into())
    );
    let mut cmd = app.update(
        Event::Rename {
            tab,
            name: "z.md".into(),
        },
        &mut model,
    );
    assert_eq!(
        ran(&mut cmd),
        Some(FileOp::Rename {
            from: on("a.md"),
            to: on("z.md")
        })
    );

    // A duplicate is a copy beside the original, under a free name.
    let mut cmd = app.update(Event::Duplicate { tab }, &mut model);
    assert_eq!(
        ran(&mut cmd),
        Some(FileOp::Copy {
            srcs: vec![on("a.md")],
            dst: PathBuf::from(docs),
            policy: ConflictPolicy::KeepBoth
        })
    );

    // There is no bin to move things to: asking for it removes nothing.
    let mut cmd = app.update(Event::TrashSelection { tab }, &mut model);
    assert!(file_ops(&mut cmd).is_empty());
    assert!(
        app.view(&model)
            .toasts
            .iter()
            .any(|toast| toast.text.contains("no Recycle Bin"))
    );

    // Deleting does, and the folder is read again once it is done.
    let mut cmd = app.update(Event::DeleteSelection { tab }, &mut model);
    let asked = file_ops(&mut cmd);
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].operation.op, FileOp::Delete(vec![on("a.md")]));
    let mut cmd = app.update(Event::DeleteSelection { tab }, &mut model);
    let mut after = finish(
        &app,
        &mut model,
        &mut cmd,
        Outcome {
            affected_dirs: vec![PathBuf::from(docs)],
            ..outcome(0, Origin::User)
        },
    );
    assert_eq!(listed_paths(&mut after), [docs]);
    assert_eq!(app.view(&model).undo, None, "nothing to take back");

    // What is inside an archive stays as it is.
    let inside = "archive:/x/a.zip!/";
    open(&app, &mut model, tab, inside, &[entry("m.txt", false)]);
    click(&app, &mut model, tab, 0, false, false);
    let mut cmd = app.update(Event::DeleteSelection { tab }, &mut model);
    assert!(file_ops(&mut cmd).is_empty());
}

/// Answers the clipboard count `cmd` asks for and returns what follows.
fn counted(
    app: &DirFleet,
    model: &mut Model,
    cmd: &mut Command<Effect, Event>,
    count: u64,
) -> Command<Effect, Event> {
    let effects: Vec<Effect> = cmd.effects().collect();
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::Clipboard(_))),
        "the system's clipboard is left alone"
    );
    let mut request = effects
        .into_iter()
        .find_map(|effect| match effect {
            Effect::ClipboardChanges(request) => Some(request),
            _ => None,
        })
        .expect("the clipboard's changes are counted");
    request.resolve(count).expect("resolves");
    let event = cmd.events().next().expect("the count comes back");
    app.update(event, model)
}

#[test]
#[allow(clippy::too_many_lines, reason = "one story, told in order")]
fn what_is_copied_on_a_server_is_kept_and_pasted_unless_the_clipboard_took_something_newer() {
    use crate::fileops::{ConflictPolicy, FileOp};
    let app = DirFleet;
    let mut model = Model::default();
    model.settings.queue_mode = QueueMode::Unlimited;
    let tab = window(&app, &mut model);
    let docs = "sftp://c1/docs";
    open(&app, &mut model, tab, docs, &[entry("a.md", false)]);
    let other = window(&app, &mut model);
    open(&app, &mut model, other, "/y", &[]);
    let file = PathBuf::from(format!("{docs}/a.md"));
    // Pastes into `/y` with the clipboard's count at `count`, and returns
    // what is asked of the shell next.
    let paste = |model: &mut Model, count: u64| {
        let mut cmd = app.update(Event::Paste { tab: other }, model);
        counted(&app, model, &mut cmd, count)
    };
    // The names checked and found free, the operation that runs.
    let runs = |model: &mut Model, mut cmd: Command<Effect, Event>| {
        let mut check = cmd
            .effects()
            .find_map(|effect| match effect {
                Effect::FindConflicts(request) => Some(request),
                _ => None,
            })
            .expect("names are checked");
        assert_eq!(check.operation.srcs, std::slice::from_ref(&file));
        check.resolve(Conflicts(Vec::new())).expect("resolves");
        let event = cmd.events().next().expect("the answer comes back");
        let mut cmd = app.update(event, model);
        file_ops(&mut cmd).pop().map(|request| request.operation.op)
    };

    // Copied: kept here, since the system's clipboard takes no server's files.
    click(&app, &mut model, tab, 0, false, false);
    let mut cmd = app.update(Event::CopySelection { tab }, &mut model);
    let _ = counted(&app, &mut model, &mut cmd, 5);
    let pasted = paste(&mut model, 5);
    assert_eq!(
        runs(&mut model, pasted),
        Some(FileOp::Copy {
            srcs: vec![file.clone()],
            dst: PathBuf::from("/y"),
            policy: ConflictPolicy::KeepBoth
        })
    );
    // A cut is dimmed, and moves when it is pasted.
    let mut cmd = app.update(Event::CutSelection { tab }, &mut model);
    let _ = counted(&app, &mut model, &mut cmd, 5);
    assert!(shown(&app, &model).rows[0].cut);
    let pasted = paste(&mut model, 5);
    assert_eq!(
        runs(&mut model, pasted),
        Some(FileOp::Move {
            srcs: vec![file.clone()],
            dst: PathBuf::from("/y"),
            policy: ConflictPolicy::KeepBoth
        })
    );
    assert!(!shown(&app, &model).rows[0].cut);

    // Another program put something on the clipboard since: that is pasted.
    let mut cmd = app.update(Event::CopySelection { tab }, &mut model);
    let _ = counted(&app, &mut model, &mut cmd, 5);
    let mut newer = paste(&mut model, 6);
    assert!(
        newer
            .effects()
            .any(|effect| matches!(effect, Effect::Clipboard(_))),
        "the system's clipboard is read"
    );
    // And what was kept is forgotten: the next paste asks the system at once.
    let mut cmd = app.update(Event::Paste { tab: other }, &mut model);
    assert!(
        cmd.effects()
            .any(|effect| matches!(effect, Effect::Clipboard(_)))
    );
}

#[test]
#[allow(clippy::too_many_lines, reason = "one story, told in order")]
fn file_contents_are_searched_once_the_term_is_committed_and_rows_show_the_matching_line() {
    use crate::search::{Chip, SearchTermType};
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[entry("a.txt", false), entry("b.md", false)],
    );
    let reads = |cmd: &mut Command<Effect, Event>| {
        cmd.effects()
            .filter(|effect| matches!(effect, Effect::ListDir(_) | Effect::SearchWalk(_)))
            .count()
    };

    // Typing reads nothing: the term waits for Enter, and the bar says so.
    let mut cmd = search_text(&app, &mut model, tab, "content:todo");
    assert_eq!(reads(&mut cmd), 0);
    let view = shown(&app, &model);
    assert!(
        view.search
            .as_ref()
            .is_some_and(|search| search.content_pending)
    );
    assert!(!view.show_match);
    assert_eq!(view.total_rows, 2, "nothing is filtered yet");

    // Committed: the folder's own files are read, by the committed search.
    let mut cmd = app.update(Event::CommitSearchText { tab }, &mut model);
    let mut walk = walks(&mut cmd).pop().expect("contents are read");
    let term = Chip::Content {
        pattern: "todo".into(),
        kind: SearchTermType::Plain,
    };
    let read = walk
        .operation
        .content
        .clone()
        .expect("the search to read by");
    assert_eq!(read.chips, std::slice::from_ref(&term));
    assert_eq!(
        (walk.operation.depth, walk.operation.skip_ignored),
        (0, false)
    );
    let view = shown(&app, &model);
    assert!(view.show_match);
    let bar = view.search.expect("the bar");
    assert!(!bar.content_pending);
    assert_eq!(bar.chips[0].token, "content:todo");

    // What matched comes back with its line.
    let found = [DirEntry::test_new("/x/a.txt")];
    walk.resolve(WalkUpdate {
        output: listing(&found, walk.operation.view.clone()),
        done: true,
        root: "/x".into(),
        matched: Arc::new(vec![MatchLine {
            line: 3,
            text: "a TODO b".into(),
            start: 2,
            end: 6,
        }]),
        scanned: 2,
    })
    .expect("done");
    let event = cmd.events().next().expect("the results");
    let _ = app.update(event, &mut model);
    let view = shown(&app, &model);
    assert_eq!(view.total_rows, 1);
    let row = &view.rows[0];
    assert_eq!(
        (row.name.as_str(), row.matched.as_str()),
        ("a.txt", "a TODO b")
    );
    assert_eq!(row.matched_line, 3);
    assert_eq!(row.matched_highlights, [TextRange { start: 2, end: 6 }]);
    assert_eq!(view.search.map(|search| search.scanned), Some(2));

    // More typing only narrows what was found; nothing is read again.
    let mut cmd = search_text(&app, &mut model, tab, "a");
    assert_eq!(reads(&mut cmd), 0);

    // Another committed filter, or another choice about ignored files,
    // changes which files are opened: they are read again.
    let _ = search_text(&app, &mut model, tab, "ext:txt");
    let mut cmd = app.update(Event::CommitSearchText { tab }, &mut model);
    let walk = walks(&mut cmd).pop().expect("contents are read again");
    let read = walk.operation.content.expect("the search to read by");
    assert_eq!(read.chips, [term, Chip::Ext(vec!["txt".into()])]);
    let mut cmd = app.update(Event::SetSearchIgnored { tab, on: true }, &mut model);
    let walk = walks(&mut cmd).pop().expect("contents are read again");
    assert!(walk.operation.skip_ignored);
    assert!(
        shown(&app, &model)
            .search
            .is_some_and(|search| search.skip_ignored)
    );

    // Without the term the folder is listed as it is, and no line is shown.
    let mut cmd = app.update(Event::RemoveSearchChip { tab, index: 0 }, &mut model);
    assert!(walks(&mut cmd).is_empty());
    let view = shown(&app, &model);
    assert!(!view.show_match);
    assert!(view.rows.iter().all(|row| row.matched.is_empty()));
}

#[test]
fn a_content_term_is_typed_shown_and_typed_again_the_same() {
    use crate::search::query::{chip_label, chip_token, parse};
    use crate::search::{Chip, Search, SearchTermType};
    // An expression with a space in it is quoted, like any such text.
    let chips = parse("content:TODO text:\"two words\" content:\"/fn \\w+/\" report").chips;
    assert_eq!(
        chips,
        [
            Chip::Content {
                pattern: "TODO".into(),
                kind: SearchTermType::Plain
            },
            Chip::Content {
                pattern: "two words".into(),
                kind: SearchTermType::Plain
            },
            Chip::Content {
                pattern: "fn \\w+".into(),
                kind: SearchTermType::Regex
            },
            Chip::name("report"),
        ]
    );
    let tokens: Vec<String> = chips.iter().map(chip_token).collect();
    assert_eq!(
        tokens,
        [
            "content:TODO",
            "content:\"two words\"",
            "content:\"/fn \\w+/\"",
            "report"
        ]
    );
    assert_eq!(parse(&tokens.join(" ")).chips, chips);
    assert_eq!(chip_label(&chips[0]), "Contains: TODO");

    // While it is typed a content term filters nothing; committed, it is
    // what file contents are read by, without the text typed after it.
    let mut search = Search {
        value: "report content:todo".into(),
        ..Search::default()
    };
    assert!(search.content_pending());
    assert_eq!(search.effective_chips(), [Chip::name("report")]);
    assert!(!search.has_content());
    search.commit_text();
    search.value = "more".into();
    assert!(search.has_content() && !search.content_pending());
    assert_eq!(search.committed().value, "");
    assert_eq!(search.committed().chips, search.chips);
}

/// The folder sizes asked for in `cmd`, and what was told to stop.
fn sizes_asked(cmd: &mut Command<Effect, Event>) -> (Vec<Request<FolderSize>>, Vec<String>) {
    let (mut asked, mut stopped) = (Vec::new(), Vec::new());
    for effect in cmd.effects() {
        match effect {
            Effect::FolderSize(request) => asked.push(request),
            Effect::StopFolderSize(request) => stopped.push(request.operation.path.clone()),
            _ => {}
        }
    }
    (asked, stopped)
}

/// Each shown row's Size cell, with the default columns.
fn size_cells(app: &DirFleet, model: &Model) -> Vec<String> {
    shown(app, model)
        .rows
        .iter()
        .map(|row| row.cells[1].clone())
        .collect()
}

fn folder_sizes_on(app: &DirFleet, model: &mut Model) -> Command<Effect, Event> {
    app.update(
        Event::ChangeSettings {
            settings: Settings {
                folder_sizes: true,
                ..Settings::default()
            },
        },
        model,
    )
}

#[test]
fn with_the_setting_on_folders_in_view_are_added_up_two_at_a_time_for_the_size_column() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let entries = [
        entry("a", true),
        entry("b", true),
        entry("c", true),
        entry("f.txt", false),
    ];
    open(&app, &mut model, tab, "/x", &entries);
    // Off unless asked for: a folder's row says nothing of its size.
    let mut idle = app.update(
        Event::ViewportChanged {
            tab,
            first_row: 0,
            rows: 10,
        },
        &mut model,
    );
    assert!(sizes_asked(&mut idle).0.is_empty());
    assert_eq!(size_cells(&app, &model), ["", "", "", "10 B"]);

    let mut cmd = folder_sizes_on(&app, &mut model);
    let (mut asked, _) = sizes_asked(&mut cmd);
    let names: Vec<_> = asked
        .iter()
        .map(|request| request.operation.path.clone())
        .collect();
    assert_eq!(names.len(), 2, "two at a time: {names:?}");
    assert!(
        names[0].ends_with('a') && names[1].ends_with('b'),
        "{names:?}"
    );
    assert_eq!(
        size_cells(&app, &model),
        ["\u{2026}", "\u{2026}", "", "10 B"]
    );

    // One answered: its size shows, and the next folder is asked for.
    asked[0].resolve(Some(2_500_000)).expect("resolves");
    let event = cmd.events().next().expect("the size comes back");
    let mut next = app.update(event, &mut model);
    let (mut third, _) = sizes_asked(&mut next);
    assert_eq!(third.len(), 1);
    assert!(third[0].operation.path.ends_with('c'));
    assert_eq!(
        size_cells(&app, &model),
        ["2.5 MB", "\u{2026}", "\u{2026}", "10 B"]
    );
    // A folder that can't be read shows nothing, and is not asked again.
    third[0].resolve(None).expect("resolves");
    let event = next.events().next().expect("the answer comes back");
    let mut after = app.update(event, &mut model);
    assert!(sizes_asked(&mut after).0.is_empty());
    assert_eq!(size_cells(&app, &model)[2], "");

    // The folder changed: it is added up again, its old size still shown.
    let mut changed = entries.clone();
    changed[0].meta.source_revision = 7;
    let mut cmd = app.update(Event::Refresh { tab }, &mut model);
    let mut list = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::ListDir(request) => Some(request),
            _ => None,
        })
        .expect("listed again");
    let view = list.operation.view.clone();
    list.resolve(listing(&changed, view)).expect("resolves");
    let event = cmd.events().next().expect("the listing comes back");
    let mut again = app.update(event, &mut model);
    let (asked, _) = sizes_asked(&mut again);
    assert_eq!(asked.len(), 1, "b is still being added up");
    assert!(asked[0].operation.path.ends_with('a'));
    assert_eq!(size_cells(&app, &model)[0], "2.5 MB");
}

#[test]
fn folders_scrolled_out_of_view_are_no_longer_added_up() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let entries: Vec<_> = (0..300)
        .map(|index| entry(&format!("d{index:03}"), true))
        .collect();
    open(&app, &mut model, tab, "/x", &entries);
    let _ = app.update(
        Event::ViewportChanged {
            tab,
            first_row: 0,
            rows: 10,
        },
        &mut model,
    );
    let mut cmd = folder_sizes_on(&app, &mut model);
    let (mut first, _) = sizes_asked(&mut cmd);
    assert_eq!(first.len(), 2);
    let early: Vec<_> = first
        .iter()
        .map(|request| request.operation.path.clone())
        .collect();

    let mut moved = app.update(
        Event::ViewportChanged {
            tab,
            first_row: 200,
            rows: 10,
        },
        &mut model,
    );
    let (asked, mut stopped) = sizes_asked(&mut moved);
    stopped.sort();
    assert_eq!(stopped, early, "what left the view is stopped");
    assert_eq!(asked.len(), 2);
    assert!(asked[0].operation.path.ends_with("d200"));

    // A stopped walk's answer says nothing of the folder.
    first[0].resolve(None).expect("resolves");
    let event = cmd.events().next().expect("the answer comes back");
    let mut late = app.update(event, &mut model);
    assert!(sizes_asked(&mut late).0.is_empty(), "two are still asked");
    let _ = app.update(
        Event::ViewportChanged {
            tab,
            first_row: 0,
            rows: 10,
        },
        &mut model,
    );
    assert_eq!(size_cells(&app, &model)[0], "\u{2026}", "asked for anew");
}

#[test]
fn a_size_asked_for_in_the_inspector_is_not_stopped_and_file_operations_make_sizes_stale() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("sub", true)]);
    let _ = app.update(Event::SetInspector { shown: true }, &mut model);
    click(&app, &mut model, tab, 0, false, false);
    let path = shown(&app, &model)
        .inspector
        .and_then(|inspector| inspector.item)
        .expect("one item")
        .path;
    let mut cmd = app.update(Event::CalculateSize { path: path.clone() }, &mut model);
    let (mut asked, _) = sizes_asked(&mut cmd);
    assert_eq!(asked.len(), 1);
    // Asked again while it is added up: nothing more.
    let mut twice = app.update(Event::CalculateSize { path: path.clone() }, &mut model);
    assert!(sizes_asked(&mut twice).0.is_empty());
    // Leaving the folder stops nothing: the inspector asked, not a list.
    let mut away = app.update(
        Event::Navigate {
            tab,
            path: "/elsewhere".into(),
        },
        &mut model,
    );
    assert_eq!(sizes_asked(&mut away).1, Vec::<String>::new());
    asked[0].resolve(Some(1_000)).expect("resolves");
    let event = cmd.events().next().expect("the size comes back");
    let _ = app.update(event, &mut model);
    assert_eq!(model.sizes.info(&path), SizeInfo::Known("1.0 KB".into()));

    // A copy into the folder, or into one inside it: the size is stale, and
    // added up again once a list shows the folder with the setting on.
    let inside = Path::new(&path).join("deeper");
    model
        .sizes
        .touched(&[inside.to_string_lossy().into_owned()]);
    open(&app, &mut model, tab, "/x", &[entry("sub", true)]);
    let mut cmd = folder_sizes_on(&app, &mut model);
    let (asked, _) = sizes_asked(&mut cmd);
    assert_eq!(asked.len(), 1, "added up again");
    assert_eq!(
        size_cells(&app, &model),
        ["1.0 KB"],
        "the old size meanwhile"
    );
}

/// The media files asked about in `cmd`.
fn probes(cmd: &mut Command<Effect, Event>) -> Vec<Request<ProbeMedia>> {
    cmd.effects()
        .filter_map(|effect| match effect {
            Effect::ProbeMedia(request) => Some(request),
            _ => None,
        })
        .collect()
}

fn probed_names(asked: &[Request<ProbeMedia>]) -> Vec<String> {
    asked
        .iter()
        .map(|request| {
            Path::new(&request.operation.path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .collect()
}

#[test]
fn the_dimensions_and_duration_columns_ask_about_the_media_in_view() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let entries = [
        entry("p.png", false),
        entry("clip.mp4", false),
        entry("song.mp3", false),
        entry("notes.txt", false),
    ];
    open(&app, &mut model, tab, "/x", &entries);
    let mut cmd = app.update(
        Event::ToggleColumn {
            column: Column::Dimensions,
        },
        &mut model,
    );
    let mut sized = probes(&mut cmd);
    // A sound has no picture, and text has neither.
    assert_eq!(probed_names(&sized), ["p.png", "clip.mp4"]);
    sized[0]
        .resolve(MediaInfo {
            width: 640,
            height: 480,
            millis: 0,
        })
        .expect("resolves");
    let event = cmd.events().next().expect("the answer comes back");
    let _ = app.update(event, &mut model);
    // Modified, Size, Dimensions.
    let cell = |model: &Model, row: usize, column: usize| {
        shown(&app, model).rows[row].cells[column].clone()
    };
    assert_eq!(cell(&model, 0, 2), "640 \u{d7} 480");
    assert_eq!(cell(&model, 1, 2), "", "not read yet");

    let mut timed = app.update(
        Event::ToggleColumn {
            column: Column::Duration,
        },
        &mut model,
    );
    assert_eq!(
        probed_names(&probes(&mut timed)),
        ["song.mp3"],
        "the video is already asked about"
    );
    sized[1]
        .resolve(MediaInfo {
            width: 1920,
            height: 1080,
            millis: 187_000,
        })
        .expect("resolves");
    let event = cmd.events().next().expect("the answer comes back");
    let _ = app.update(event, &mut model);
    assert_eq!(cell(&model, 1, 2), "1920 \u{d7} 1080");
    assert_eq!(cell(&model, 1, 3), "3:07");
    assert_eq!(cell(&model, 3, 2), "");

    // The inspector says the same of its item.
    let _ = app.update(Event::SetInspector { shown: true }, &mut model);
    click(&app, &mut model, tab, 1, false, false);
    let item = shown(&app, &model)
        .inspector
        .and_then(|inspector| inspector.item)
        .expect("one item");
    assert_eq!(
        (item.dimensions.as_str(), item.duration.as_str()),
        ("1920 \u{d7} 1080", "3:07")
    );

    // The file changed: what was read is about what it used to hold.
    let mut changed = entries.clone();
    changed[1].meta.size = 99;
    let mut cmd = app.update(Event::Refresh { tab }, &mut model);
    let mut list = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::ListDir(request) => Some(request),
            _ => None,
        })
        .expect("listed again");
    let view = list.operation.view.clone();
    list.resolve(listing(&changed, view)).expect("resolves");
    let event = cmd.events().next().expect("the listing comes back");
    let mut again = app.update(event, &mut model);
    assert!(probed_names(&probes(&mut again)).contains(&"clip.mp4".to_owned()));
    assert_eq!(cell(&model, 1, 2), "");
}

#[test]
fn the_inspector_alone_asks_about_its_media_item() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[entry("p.png", false), entry("q.png", false)],
    );
    // Neither column is shown: nothing is read for the list.
    let mut quiet = app.update(Event::SetInspector { shown: true }, &mut model);
    assert!(probes(&mut quiet).is_empty());
    let mut cmd = app.update(
        Event::Click {
            tab,
            row: 1,
            command: false,
            shift: false,
        },
        &mut model,
    );
    assert_eq!(probed_names(&probes(&mut cmd)), ["q.png"]);
}

/// Answers the ordering `cmd` asks for with the listing's entries as they
/// are, as if sorted, and hands the answer to the app.
fn answer_order(app: &DirFleet, model: &mut Model, mut cmd: Command<Effect, Event>) {
    let mut ask = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::OrderListing(request) => Some(request),
            _ => None,
        })
        .expect("the order is asked for again");
    let listing = Arc::clone(&ask.operation.listing);
    let order = (0..listing.len() as u32).collect();
    let view = ask.operation.view.clone();
    ask.resolve(Ordered {
        listing,
        order,
        view,
    })
    .expect("resolves");
    let event = cmd.events().next().expect("the order comes back");
    let _ = app.update(event, model);
}

/// The shown tab's groups as `label first..count`, and its rows' names.
fn groups_shown(app: &DirFleet, model: &Model) -> (Vec<String>, Vec<String>) {
    let tab = shown(app, model);
    (
        tab.groups
            .iter()
            .map(|group| format!("{} {}+{}", group.label, group.first_row, group.count))
            .collect(),
        tab.rows.iter().map(|row| row.name.clone()).collect(),
    )
}

#[test]
fn a_folder_s_entries_are_put_into_groups_selected_by_group_and_ungrouped_again() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let entries = [
        entry("a.txt", false),
        entry("b.png", false),
        entry("sub", true),
        entry("c.txt", false),
    ];
    open(&app, &mut model, tab, "/x", &entries);
    assert!(
        shown(&app, &model).groups.is_empty(),
        "not grouped at first"
    );
    assert_eq!(
        app.view(&model)
            .groupings
            .iter()
            .map(|choice| choice.label.as_str())
            .collect::<Vec<_>>(),
        ["None", "Kind", "Date modified", "Size", "Name"]
    );

    let cmd = app.update(
        Event::SetGroupBy {
            tab,
            group: GroupBy::Kind,
        },
        &mut model,
    );
    answer_order(&app, &mut model, cmd);
    assert_eq!(shown(&app, &model).view.group, GroupBy::Kind);
    let (groups, names) = groups_shown(&app, &model);
    assert_eq!(groups, ["Folder 0+1", "PNG image 1+1", "TXT file 2+2"]);
    assert_eq!(names, ["sub", "b.png", "a.txt", "c.txt"]);

    // A group's heading selects its rows, and only those.
    click(&app, &mut model, tab, 0, false, false);
    let _ = app.update(Event::SelectGroup { tab, group: 2 }, &mut model);
    assert_eq!(rows_selected(&app, &model), ["a.txt", "c.txt"]);
    let _ = app.update(Event::SelectGroup { tab, group: 9 }, &mut model);
    assert_eq!(
        rows_selected(&app, &model),
        ["a.txt", "c.txt"],
        "no such group"
    );

    // The folder keeps its grouping when it is read again, and the selection
    // stays on the same entries.
    open(&app, &mut model, tab, "/x", &entries);
    let (groups, _) = groups_shown(&app, &model);
    assert_eq!(groups, ["Folder 0+1", "PNG image 1+1", "TXT file 2+2"]);

    let cmd = app.update(
        Event::SetGroupBy {
            tab,
            group: GroupBy::None,
        },
        &mut model,
    );
    answer_order(&app, &mut model, cmd);
    let (groups, names) = groups_shown(&app, &model);
    assert_eq!(groups, Vec::<String>::new());
    assert_eq!(names, ["a.txt", "b.png", "sub", "c.txt"]);
}

#[test]
fn a_folder_s_grouping_is_kept_with_its_view() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = start(&app, &mut model, Some("/x"), LoadedState::Missing);
    let tab = shown(&app, &model).id;
    let mut cmd = app.update(
        Event::SetGroupBy {
            tab,
            group: GroupBy::Size,
        },
        &mut model,
    );
    let saved = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::SaveState(request) => Some(request.operation.state),
            _ => None,
        })
        .expect("the state is saved");
    let text = serde_json::to_string(&saved).expect("serializes");
    let read: PersistedState = serde_json::from_str(&text).expect("reads back");
    assert_eq!(read, saved);
    assert!(text.contains("\"group\":\"Size\""), "{text}");
    // A view saved before there were groups reads as not grouped.
    let old: FolderView = serde_json::from_str(
        r#"{"sort":"Name","invert":false,"display":"List","show_hidden":false}"#,
    )
    .expect("reads");
    assert_eq!(old.group, GroupBy::None);
}

/// A file found by a scan: where it is, how large, and when it changed.
fn found_file(path: &str, size: u64, modified: u32) -> DirEntry {
    let mut entry = DirEntry::test_new(path);
    entry.meta.size = size;
    entry.meta.modified_at = TimestampSeconds::from_unix(modified);
    entry
}

/// The scan `cmd` starts.
fn scan_asked(cmd: &mut Command<Effect, Event>) -> Request<FindDuplicates> {
    cmd.effects()
        .find_map(|effect| match effect {
            Effect::FindDuplicates(request) => Some(request),
            _ => None,
        })
        .expect("a scan is started")
}

/// Answers `scan` with the files of each set in `sets` (a set's size, then
/// its files as path and time changed), and hands the answer to the app.
fn scan_found(
    app: &DirFleet,
    model: &mut Model,
    cmd: &mut Command<Effect, Event>,
    scan: &mut Request<FindDuplicates>,
    sets: &[(u64, &[(&str, u32)])],
) -> Command<Effect, Event> {
    let mut entries = Vec::new();
    let mut set_of = Vec::new();
    for (set, (size, files)) in sets.iter().enumerate() {
        for (path, modified) in *files {
            entries.push(found_file(path, *size, *modified));
            set_of.push(set as u32);
        }
    }
    let files = entries.len() as u64 + 7;
    scan.resolve(DuplicatesUpdate::Found(DuplicateSets {
        output: listing(&entries, scan.operation.view.clone()),
        set_of: Arc::new(set_of),
        sizes: Arc::new(sets.iter().map(|(size, _)| *size).collect()),
        unread: 0,
        files,
    }))
    .expect("resolves");
    let event = cmd.events().next().expect("what was found comes back");
    app.update(event, model)
}

/// A tab of duplicates beside a tab at `/x`: two sets, the first of two
/// large files and the second of three small ones.
fn duplicates_tab(app: &DirFleet, model: &mut Model) -> u32 {
    let tab = window(app, model);
    open(app, model, tab, "/x", &[entry("docs", true)]);
    let mut cmd = app.update(
        Event::FindDuplicates {
            tab,
            selected: false,
        },
        model,
    );
    let mut scan = scan_asked(&mut cmd);
    let _ = scan_found(
        app,
        model,
        &mut cmd,
        &mut scan,
        &[
            (1000, &[("/x/docs/film.bin", 50), ("/x/pics/film.bin", 90)]),
            (
                5,
                &[
                    ("/x/docs/n.txt", 30),
                    ("/x/pics/n.txt", 10),
                    ("/x/pics/old/n.txt", 20),
                ],
            ),
        ],
    );
    shown(app, model).id
}

/// `path` with `/` between its parts, whatever this system puts there.
fn slashed(path: &str) -> String {
    path.replace('\\', "/")
}

/// The shown tab's rows as `folder/name`.
fn rows_with_folders(app: &DirFleet, model: &Model) -> Vec<String> {
    shown(app, model)
        .rows
        .iter()
        .map(|row| slashed(&format!("{}/{}", row.folder, row.name)))
        .collect()
}

/// The scans `cmd` stops, and the one it starts.
fn scans(cmd: &mut Command<Effect, Event>) -> (Vec<u64>, Option<u64>) {
    let mut stopped = Vec::new();
    let mut started = None;
    for effect in cmd.effects() {
        match effect {
            Effect::StopDuplicates(request) => stopped.push(request.operation.id),
            Effect::FindDuplicates(request) => started = Some(request.operation.id),
            _ => {}
        }
    }
    (stopped, started)
}

#[test]
fn looking_for_duplicates_opens_a_tab_that_shows_the_sets_found() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[entry("docs", true), entry("a.txt", false)],
    );
    // Whatever is selected, it is the tab's folder that is looked through.
    click(&app, &mut model, tab, 0, false, false);
    let mut cmd = app.update(
        Event::FindDuplicates {
            tab,
            selected: false,
        },
        &mut model,
    );
    let mut scan = scan_asked(&mut cmd);
    assert_eq!(scan.operation.roots, ["/x"], "the tab's own folder");

    // A tab of its own beside the folder's, which stays as it was.
    let (tabs, active) = tabs_of(&app, &model, 0);
    assert_eq!(tabs.len(), 2);
    assert_eq!(active, tabs[1]);
    let results = shown(&app, &model);
    assert_ne!(results.id, tab);
    assert_eq!(results.title, "Duplicates in x");
    assert_eq!(results.path, "");
    assert!(results.loading);
    let bar = results.results.expect("a results tab");
    assert!(bar.running);
    assert_eq!(bar.summary, "");
    // Nothing is made or renamed among results.
    assert!(!results.location.caps.writable && !results.location.caps.rename);
    assert!(results.location.caps.real_paths && results.location.caps.trash);

    scan.resolve(DuplicatesUpdate::Progress(ScanProgress {
        found: 1200,
        ..ScanProgress::default()
    }))
    .expect("resolves");
    let event = cmd.events().next().expect("progress comes back");
    let _ = app.update(event, &mut model);
    assert_eq!(shown(&app, &model).progress, "Reading folders… 1200 items");
    scan.resolve(DuplicatesUpdate::Progress(ScanProgress {
        found: 1300,
        candidates: 40,
        compared: 250_000,
        to_compare: 1_000_000,
    }))
    .expect("resolves");
    let event = cmd.events().next().expect("progress comes back");
    let _ = app.update(event, &mut model);
    assert_eq!(
        shown(&app, &model).progress,
        "Comparing files… 25% of 1.0 MB"
    );

    let _ = scan_found(
        &app,
        &mut model,
        &mut cmd,
        &mut scan,
        &[
            (1000, &[("/x/docs/film.bin", 50), ("/x/pics/film.bin", 90)]),
            (
                5,
                &[
                    ("/x/docs/n.txt", 30),
                    ("/x/pics/n.txt", 10),
                    ("/x/pics/old/n.txt", 20),
                ],
            ),
        ],
    );
    let results = shown(&app, &model);
    assert!(!results.loading);
    assert_eq!(results.progress, "");
    assert!(results.show_folder);
    let bar = results.results.expect("a results tab");
    assert!(!bar.running);
    assert_eq!(
        bar.summary,
        "2 sets of duplicates, 5 files. The extra copies take 1.0 KB"
    );
    assert_eq!((bar.sets, bar.whole_sets), (2, 0));
    // Each set under a heading, its files with the whole of their folders.
    let (groups, _) = groups_shown(&app, &model);
    assert_eq!(groups, ["1.0 KB each 0+2", "5 B each 2+3"]);
    assert_eq!(
        rows_with_folders(&app, &model),
        [
            "/x/docs/film.bin",
            "/x/pics/film.bin",
            "/x/docs/n.txt",
            "/x/pics/n.txt",
            "/x/pics/old/n.txt"
        ]
    );
}

#[test]
fn a_file_found_is_previewed_by_its_own_path_and_its_tab_is_not_kept() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = duplicates_tab(&app, &mut model);
    click(&app, &mut model, tab, 4, false, false);
    let mut cmd = app.update(Event::QuickLook { tab }, &mut model);
    let look = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::QuickLook(request) => Some(request.operation),
            _ => None,
        })
        .expect("previewed");
    assert_eq!(
        slashed(&look.paths[look.index as usize]),
        "/x/pics/old/n.txt"
    );

    // A tab without a folder is not kept between launches.
    assert_eq!(DirFleet::persisted(&model).windows[0].tabs, ["/x"]);
}

#[test]
fn the_folders_selected_are_the_ones_searched_for_duplicates() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[
            entry("docs", true),
            entry("pics", true),
            entry("a.txt", false),
        ],
    );
    click(&app, &mut model, tab, 0, false, false);
    click(&app, &mut model, tab, 1, true, false);
    click(&app, &mut model, tab, 2, true, false);
    let find = |tab, selected| Event::FindDuplicates { tab, selected };
    let mut cmd = app.update(find(tab, true), &mut model);
    let roots: Vec<String> = scan_asked(&mut cmd)
        .operation
        .roots
        .iter()
        .map(|root| slashed(root))
        .collect();
    assert_eq!(roots, ["/x/docs", "/x/pics"], "the folders, not the file");
    assert_eq!(shown(&app, &model).title, "Duplicates in 2 folders");

    // Nothing is looked for from a tab without a folder of its own, or
    // among a selection that holds no folder.
    let results = shown(&app, &model).id;
    let mut cmd = app.update(find(results, false), &mut model);
    assert!(cmd.effects().next().is_none());
    let _ = app.update(Event::CloseTab { tab: results }, &mut model);
    click(&app, &mut model, tab, 2, false, false);
    let mut cmd = app.update(find(tab, true), &mut model);
    assert!(cmd.effects().next().is_none());
    assert_eq!(tabs_of(&app, &model, 0).0.len(), 1);
}

#[test]
fn all_but_one_file_of_each_set_is_selected_and_whole_sets_are_counted() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = duplicates_tab(&app, &mut model);

    let select = |model: &mut Model, keep| {
        let _ = app.update(Event::SelectDuplicates { tab, keep }, model);
        shown(&app, model)
            .selection
            .iter()
            .flat_map(|range| range.start..range.end)
            .collect::<Vec<u32>>()
    };
    // Rows 0-1 are the films (changed at 50, 90), rows 2-4 the notes
    // (changed at 30, 10, 20).
    assert_eq!(select(&mut model, Keep::Newest), [0, 3, 4]);
    assert_eq!(select(&mut model, Keep::Oldest), [1, 2, 4]);
    assert_eq!(select(&mut model, Keep::First), [1, 3, 4]);
    let bar = shown(&app, &model).results.expect("a results tab");
    assert_eq!(bar.whole_sets, 0, "one file of each set stays");

    // Every film selected: deleting them would leave no copy.
    click(&app, &mut model, tab, 0, false, false);
    click(&app, &mut model, tab, 1, true, false);
    click(&app, &mut model, tab, 2, true, false);
    let bar = shown(&app, &model).results.expect("a results tab");
    assert_eq!(bar.whole_sets, 1);
    let _ = app.update(Event::SelectAll { tab }, &mut model);
    let bar = shown(&app, &model).results.expect("a results tab");
    assert_eq!(bar.whole_sets, 2);

    // A heading selects its set, like any group's.
    let _ = app.update(Event::SelectGroup { tab, group: 1 }, &mut model);
    assert_eq!(rows_selected(&app, &model), ["n.txt", "n.txt", "n.txt"]);

    // Nothing is renamed or copied among results.
    click(&app, &mut model, tab, 0, false, false);
    let _ = app.update(Event::StartRename { tab }, &mut model);
    assert_eq!(shown(&app, &model).renaming, None);
    let mut cmd = app.update(Event::Duplicate { tab }, &mut model);
    assert!(file_ops(&mut cmd).is_empty());
}

#[test]
fn files_deleted_leave_the_results_and_a_set_of_one_is_no_set() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = duplicates_tab(&app, &mut model);

    // The second and third note go to the bin.
    click(&app, &mut model, tab, 3, false, false);
    click(&app, &mut model, tab, 4, true, false);
    let mut cmd = app.update(Event::TrashSelection { tab }, &mut model);
    let mut run = file_ops(&mut cmd).pop().expect("an operation runs");
    assert_eq!(
        run.operation.op,
        crate::fileops::FileOp::Trash(vec![
            PathBuf::from("/x/pics/n.txt"),
            PathBuf::from("/x/pics/old/n.txt")
        ])
    );
    let id = run.operation.id;
    run.resolve(FileOpUpdate(crate::fileops::FileOpEvent::Finished(
        crate::fileops::Outcome {
            affected_dirs: vec![PathBuf::from("/x/pics"), PathBuf::from("/x/pics/old")],
            ..outcome(id, crate::fileops::Origin::User)
        },
    )))
    .expect("resolves");
    let event = cmd.events().next().expect("the outcome comes back");
    let mut done = app.update(event, &mut model);
    // Nothing is scanned again: the files in those folders are asked about.
    let effects: Vec<Effect> = done.effects().collect();
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::FindDuplicates(_)))
    );
    let mut check = effects
        .into_iter()
        .find_map(|effect| match effect {
            Effect::StillThere(request) => Some(request),
            _ => None,
        })
        .expect("the files are checked");
    let asked = |check: &Request<StillThere>| -> Vec<String> {
        check
            .operation
            .paths
            .iter()
            .map(|path| slashed(path))
            .collect()
    };
    assert_eq!(
        asked(&check),
        ["/x/pics/film.bin", "/x/pics/n.txt", "/x/pics/old/n.txt"]
    );
    check.resolve(vec![true, false, false]).expect("resolves");
    let event = done.events().next().expect("the answer comes back");
    let cmd = app.update(event, &mut model);
    answer_order(&app, &mut model, cmd);

    // One note is left, which is no set; the films stay.
    let (groups, _) = groups_shown(&app, &model);
    assert_eq!(groups, ["1.0 KB each 0+2"]);
    assert_eq!(
        rows_with_folders(&app, &model),
        ["/x/docs/film.bin", "/x/pics/film.bin"]
    );
    let bar = shown(&app, &model).results.expect("a results tab");
    assert_eq!(
        bar.summary,
        "1 set of duplicates, 2 files. The extra copies take 1.0 KB"
    );

    // Undo puts a note back: the set shows again.
    let mut checks = model
        .tabs
        .iter()
        .filter_map(|(id, state)| state.check_results(id.number(), &["/x/pics".to_owned()]))
        .collect::<Vec<_>>();
    let mut cmd = checks.pop().expect("the results are checked");
    let mut check = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::StillThere(request) => Some(request),
            _ => None,
        })
        .expect("the files are checked");
    assert_eq!(
        asked(&check),
        ["/x/pics/film.bin", "/x/pics/n.txt"],
        "a file found gone is asked about again"
    );
    check.resolve(vec![true, true]).expect("resolves");
    let event = cmd.events().next().expect("the answer comes back");
    let cmd = app.update(event, &mut model);
    answer_order(&app, &mut model, cmd);
    let (groups, _) = groups_shown(&app, &model);
    assert_eq!(groups, ["1.0 KB each 0+2", "5 B each 2+2"]);
}

#[test]
fn a_scan_is_stopped_started_over_and_ended_by_leaving() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("docs", true)]);
    let find = |tab| Event::FindDuplicates {
        tab,
        selected: false,
    };
    let mut cmd = app.update(find(tab), &mut model);
    let first = scan_asked(&mut cmd).operation.id;
    let results = shown(&app, &model).id;

    let mut cmd = app.update(Event::StopResults { tab: results }, &mut model);
    assert_eq!(scans(&mut cmd), (vec![first], None));
    let view = shown(&app, &model);
    assert!(!view.loading);
    let bar = view.results.expect("a results tab");
    assert!(!bar.running);
    assert_eq!(bar.summary, "Stopped before the scan was done");
    // Nothing runs now: stopping again changes nothing.
    let mut cmd = app.update(Event::StopResults { tab: results }, &mut model);
    assert!(cmd.effects().next().is_none());

    // Refresh scans again, and a scan under way is stopped for the next.
    let mut cmd = app.update(Event::Refresh { tab: results }, &mut model);
    let (stopped, second) = scans(&mut cmd);
    let second = second.expect("a scan is started");
    assert_eq!(stopped, Vec::<u64>::new());
    assert_ne!(second, first);
    assert!(shown(&app, &model).results.expect("a results tab").running);
    let mut cmd = app.update(Event::Refresh { tab: results }, &mut model);
    let (stopped, third) = scans(&mut cmd);
    let third = third.expect("a scan is started");
    assert_eq!(stopped, [second]);

    // Going to a folder ends the scan, and the tab is a folder's again.
    let mut cmd = app.update(
        Event::Navigate {
            tab: results,
            path: "/y".into(),
        },
        &mut model,
    );
    assert_eq!(scans(&mut cmd), (vec![third], None));
    let view = shown(&app, &model);
    assert_eq!((view.path.as_str(), view.results), ("/y", None));
    assert!(view.location.caps.writable);

    // Closing a tab of results stops its scan too.
    let mut cmd = app.update(find(results), &mut model);
    let running = scan_asked(&mut cmd).operation.id;
    let closing = shown(&app, &model).id;
    let mut cmd = app.update(Event::CloseTab { tab: closing }, &mut model);
    assert_eq!(scans(&mut cmd), (vec![running], None));
}

/// Hands the tab what a scan found: `listing` in `order`, with each entry's
/// set and each set's size.
fn scan_answers(
    app: &DirFleet,
    model: &mut Model,
    tab: u32,
    (listing, order): (Listing, Vec<u32>),
    (set_of, sizes): (Vec<u32>, Vec<u64>),
    unread: u32,
) {
    let generation = model.tabs[&tab_id(tab).expect("an id")].generation;
    let view = shown(app, model).view.list_view();
    let _ = app.update(
        Event::DuplicatesScanned {
            tab,
            generation,
            update: DuplicatesUpdate::Found(DuplicateSets {
                output: ListDirOutput::Listed {
                    listing: Arc::new(listing),
                    order,
                    view,
                    opened: Arc::default(),
                },
                set_of: Arc::new(set_of),
                sizes: Arc::new(sizes),
                unread,
                files: 9000,
            }),
        },
        model,
    );
}

#[test]
fn sets_stay_together_however_the_results_are_sorted_and_past_a_point_share_a_heading() {
    let entries: Vec<DirEntry> = (0..6)
        .map(|index| found_file(&format!("/x/{index}/f.bin"), 10, index))
        .collect();
    let listing = Listing::from_entries(&entries);
    // No sets are known for a listing that is not the scan's: it stays as it is.
    let mut order: Vec<u32> = vec![5, 4, 3, 2, 1, 0];
    assert_eq!(
        results::Results::default().arrange(&listing, &mut order),
        Vec::new()
    );
    assert_eq!(order, [5, 4, 3, 2, 1, 0]);

    let app = DirFleet;
    let mut model = Model::default();
    let tab = duplicates_tab(&app, &mut model);
    // As sorted the other way round.
    scan_answers(
        &app,
        &mut model,
        tab,
        (listing, order),
        (vec![0, 1, 0, 1, 2, 2], vec![30, 20, 10]),
        0,
    );
    // Sets in their own order, each set's files in the sorted one.
    let (groups, _) = groups_shown(&app, &model);
    assert_eq!(groups, ["30 B each 0+2", "20 B each 2+2", "10 B each 4+2"]);
    assert_eq!(
        rows_with_folders(&app, &model),
        [
            "/x/2/f.bin",
            "/x/0/f.bin",
            "/x/3/f.bin",
            "/x/1/f.bin",
            "/x/5/f.bin",
            "/x/4/f.bin"
        ]
    );

    // More sets than get a heading: the rest share the last one.
    let many = SETS_HEADED + 3;
    let entries: Vec<DirEntry> = (0..many * 2)
        .map(|index| found_file(&format!("/x/{index}.bin"), 10, 0))
        .collect();
    let files = many as u32 * 2;
    scan_answers(
        &app,
        &mut model,
        tab,
        (Listing::from_entries(&entries), (0..files).collect()),
        ((0..files).map(|entry| entry / 2).collect(), vec![10; many]),
        2,
    );
    let view = shown(&app, &model);
    assert_eq!(view.groups.len(), SETS_HEADED + 1);
    let last = view.groups.last().expect("groups");
    assert_eq!(
        (last.label.as_str(), last.first_row, last.count),
        ("More duplicates", SETS_HEADED as u32 * 2, 6)
    );
    let bar = view.results.expect("a results tab");
    assert_eq!(bar.sets as usize, many);
    assert!(
        bar.summary.ends_with(". 2 files could not be read"),
        "{}",
        bar.summary
    );
}

/// A window with two panes: the left one shows `/l/a`, the right one, which
/// has the focus, shows `right`. Returns the two tabs.
fn two_panes(app: &DirFleet, model: &mut Model, right: &str) -> (u32, u32) {
    let left = window(app, model);
    open(app, model, left, "/l/a", &[]);
    let _ = app.update(
        Event::SplitNew {
            tab: left,
            side: Side::Right,
        },
        model,
    );
    let beside = shown(app, model).id;
    open(app, model, beside, right, &[]);
    (left, beside)
}

/// The comparisons `cmd` stops, and the one it starts.
fn compares(cmd: &mut Command<Effect, Event>) -> (Vec<u64>, Option<Request<CompareFolders>>) {
    let mut stopped = Vec::new();
    let mut started = None;
    for effect in cmd.effects() {
        match effect {
            Effect::StopCompare(request) => stopped.push(request.operation.id),
            Effect::CompareFolders(request) => started = Some(request),
            _ => {}
        }
    }
    (stopped, started)
}

/// Answers `scan` with the files that differ (each as its path, its size,
/// when it was changed and how it differs; a file under `/l/a` is the left
/// folder's), and hands the answer to the app.
fn compare_found(
    app: &DirFleet,
    model: &mut Model,
    cmd: &mut Command<Effect, Event>,
    scan: &mut Request<CompareFolders>,
    files: &[(&str, u64, u32, Difference)],
) -> Command<Effect, Event> {
    let entries: Vec<DirEntry> = files
        .iter()
        .map(|(path, size, modified, _)| found_file(path, *size, *modified))
        .collect();
    let listing = Arc::new(Listing::from_entries(&entries));
    let (left, right) = (scan.operation.left.clone(), scan.operation.right.clone());
    let on_left: Vec<bool> = listing
        .dirs()
        .iter()
        .map(|dir| slashed(dir).starts_with(&left))
        .collect();
    let other_dirs = listing
        .dirs()
        .iter()
        .map(|dir| {
            let dir = slashed(dir);
            dir.strip_prefix(&left).map_or_else(
                || format!("{left}{}", dir.strip_prefix(&right).unwrap_or_default()),
                |rest| format!("{right}{rest}"),
            )
        })
        .collect();
    scan.resolve(CompareUpdate::Found(Differences {
        output: ListDirOutput::Listed {
            order: (0..entries.len() as u32).collect(),
            listing,
            view: scan.operation.view.clone(),
            opened: Arc::default(),
        },
        kinds: files.iter().map(|(.., kind)| *kind).collect(),
        other_dirs: Arc::new(other_dirs),
        on_left: Arc::new(on_left),
        alike: 40,
        unread: 0,
    }))
    .expect("resolves");
    let event = cmd.events().next().expect("what was found comes back");
    app.update(event, model)
}

/// What differs between `/l/a` and `/r/b` in the tests below: two files
/// only on the left, one only on the right, one newer on each side, and
/// one that differs though changed at the same time.
const DIFFERING: [(&str, u64, u32, Difference); 6] = [
    ("/l/a/only left.txt", 10, 100, Difference::OnlyLeft),
    ("/r/b/sub/only right.txt", 10, 100, Difference::OnlyRight),
    ("/l/a/sub/newer left.txt", 20, 900, Difference::NewerLeft),
    ("/r/b/newer right.txt", 20, 900, Difference::NewerRight),
    ("/l/a/odd.txt", 5, 100, Difference::Different),
    ("/l/a/sub/second.txt", 10, 100, Difference::OnlyLeft),
];

/// A tab that shows how `/l/a` and `/r/b` differ ([`DIFFERING`]).
fn compare_tab(app: &DirFleet, model: &mut Model) -> u32 {
    let _ = two_panes(app, model, "/r/b");
    let window = app.view(model).windows[0].id;
    let mut cmd = app.update(Event::CompareFolders { window }, model);
    let mut scan = compares(&mut cmd).1.expect("the folders are compared");
    let _ = compare_found(app, model, &mut cmd, &mut scan, &DIFFERING);
    shown(app, model).id
}

/// The files `run` copies, each with where it goes.
fn pairs_copied(run: &Request<RunFileOp>) -> Vec<(String, String)> {
    let crate::fileops::FileOp::CopyPairs { pairs, into } = &run.operation.op else {
        panic!("not a copy of pairs: {:?}", run.operation.op);
    };
    assert_eq!(slashed(&into.to_string_lossy()), "/r/b");
    pairs
        .iter()
        .map(|(from, to)| {
            (
                slashed(&from.to_string_lossy()),
                slashed(&to.to_string_lossy()),
            )
        })
        .collect()
}

/// Ends `run` as done, with `affected` as the folders it changed.
fn finish_op(
    app: &DirFleet,
    model: &mut Model,
    cmd: &mut Command<Effect, Event>,
    mut run: Request<RunFileOp>,
    affected: &[&str],
) -> Command<Effect, Event> {
    let id = run.operation.id;
    run.resolve(FileOpUpdate(crate::fileops::FileOpEvent::Finished(
        crate::fileops::Outcome {
            affected_dirs: affected.iter().map(PathBuf::from).collect(),
            ..outcome(id, crate::fileops::Origin::User)
        },
    )))
    .expect("resolves");
    let event = cmd.events().next().expect("the outcome comes back");
    app.update(event, model)
}

fn compare_bar(app: &DirFleet, model: &Model) -> CompareView {
    shown(app, model)
        .results
        .and_then(|results| results.compare)
        .expect("a tab of compared folders")
}

#[test]
fn two_folders_side_by_side_and_apart_are_what_a_window_compares() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/l/a", &[]);
    let offered = |model: &Model| app.view(model).windows[0].compare.clone();
    assert_eq!(
        offered(&model),
        None,
        "one pane has nothing to compare with"
    );
    let _ = app.update(
        Event::SplitNew {
            tab,
            side: Side::Right,
        },
        &mut model,
    );
    assert_eq!(offered(&model), None, "nor has a folder shown twice");
    let right = shown(&app, &model).id;
    open(&app, &mut model, right, "/l/a/inside", &[]);
    assert_eq!(offered(&model), None, "nor a folder and one inside it");
    let window = app.view(&model).windows[0].id;
    let mut cmd = app.update(Event::CompareFolders { window }, &mut model);
    assert!(cmd.effects().next().is_none());
    assert_eq!(tabs_of(&app, &model, 0).0.len(), 2);

    open(&app, &mut model, right, "/r/b", &[]);
    assert_eq!(
        offered(&model),
        Some(ComparePair {
            left: "a".to_owned(),
            right: "b".to_owned()
        })
    );
    let mut cmd = app.update(Event::CompareFolders { window }, &mut model);
    let scan = compares(&mut cmd).1.expect("the folders are compared");
    assert_eq!(
        (
            scan.operation.left.as_str(),
            scan.operation.right.as_str(),
            scan.operation.contents
        ),
        ("/l/a", "/r/b", false),
        "the pane further left is the left folder, whichever has the focus"
    );
    // A tab of its own beside the focused one, which hides one of the two
    // folders: there is no pair to compare while it shows.
    let (tabs, active) = tabs_of(&app, &model, 0);
    assert_eq!((tabs.len(), active), (3, tabs[2]));
    assert_eq!(offered(&model), None);
}

#[test]
fn comparing_two_panes_opens_a_tab_that_shows_what_differs_kind_by_kind() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = two_panes(&app, &mut model, "/r/b");
    let window = app.view(&model).windows[0].id;
    let mut cmd = app.update(Event::CompareFolders { window }, &mut model);
    let mut scan = compares(&mut cmd).1.expect("the folders are compared");
    let results = shown(&app, &model);
    assert_eq!(results.title, "Compare a with b");
    assert_eq!(results.path, "");
    assert!(results.loading);
    assert_eq!(results.view.display, DisplayType::List);
    assert!(!results.location.caps.writable && !results.location.caps.rename);
    let bar = results.results.expect("a results tab");
    assert!(bar.running);
    assert_eq!(compare_bar(&app, &model).differences, 0);

    scan.resolve(CompareUpdate::Progress(ScanProgress {
        found: 300,
        ..ScanProgress::default()
    }))
    .expect("resolves");
    let event = cmd.events().next().expect("progress comes back");
    let _ = app.update(event, &mut model);
    assert_eq!(shown(&app, &model).progress, "Reading folders… 300 items");

    let _ = compare_found(&app, &mut model, &mut cmd, &mut scan, &DIFFERING);
    let results = shown(&app, &model);
    assert!(!results.loading && results.show_folder);
    let bar = results.results.expect("a results tab");
    assert!(!bar.running);
    assert_eq!(
        bar.summary,
        "6 differences: 2 only in a, 1 only in b, 1 newer in a, 1 newer in b, 1 different"
    );
    assert_eq!((bar.sets, bar.whole_sets), (0, 0));
    let (groups, _) = groups_shown(&app, &model);
    assert_eq!(
        groups,
        [
            "Only in a 0+2",
            "Only in b 2+1",
            "Newer in a 3+1",
            "Newer in b 4+1",
            "Different, though changed at the same time 5+1"
        ]
    );
    // Each path by the file of one side, with the whole of its folder.
    assert_eq!(
        rows_with_folders(&app, &model),
        [
            "/l/a/only left.txt",
            "/l/a/sub/second.txt",
            "/r/b/sub/only right.txt",
            "/l/a/sub/newer left.txt",
            "/r/b/newer right.txt",
            "/l/a/odd.txt"
        ]
    );
    // With nothing selected, every difference would be copied.
    let bar = compare_bar(&app, &model);
    assert_eq!((bar.left.as_str(), bar.right.as_str()), ("a", "b"));
    assert_eq!(bar.differences, 6);
    assert_eq!(
        bar.to_left,
        SyncCount {
            copies: 4,
            replaces: 3,
            newer: 1
        }
    );
    assert_eq!(
        bar.to_right,
        SyncCount {
            copies: 5,
            replaces: 3,
            newer: 1
        }
    );
    // A tab without a folder is not kept between launches.
    assert_eq!(
        DirFleet::persisted(&model).windows[0].tabs,
        ["/l/a", "/r/b"]
    );
}

#[test]
fn folders_called_the_same_are_told_apart_by_their_paths() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = two_panes(&app, &mut model, "/r/A");
    let window = app.view(&model).windows[0].id;
    let mut cmd = app.update(Event::CompareFolders { window }, &mut model);
    let mut scan = compares(&mut cmd).1.expect("the folders are compared");
    let _ = compare_found(
        &app,
        &mut model,
        &mut cmd,
        &mut scan,
        &[("/l/a/x.txt", 1, 1, Difference::OnlyLeft)],
    );
    assert_eq!(shown(&app, &model).title, "Compare /l/a with /r/A");
    assert_eq!(groups_shown(&app, &model).0, ["Only in /l/a 0+1"]);
    let bar = shown(&app, &model).results.expect("a results tab");
    assert_eq!(bar.summary, "1 difference: 1 only in /l/a");
}

#[test]
fn files_are_copied_to_the_other_folder_and_it_is_asked_before_any_is_replaced() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = compare_tab(&app, &mut model);
    let sync = |to_left| Event::SyncSelection { tab, to_left };
    let answer = |choice| Event::AnswerSync { tab, choice };
    let new_ones = [
        (
            "/l/a/only left.txt".to_owned(),
            "/r/b/only left.txt".to_owned(),
        ),
        (
            "/l/a/sub/second.txt".to_owned(),
            "/r/b/sub/second.txt".to_owned(),
        ),
    ];

    // The files only on the left, selected by their heading: copied to the
    // right at once, since nothing is replaced.
    let _ = app.update(Event::SelectGroup { tab, group: 0 }, &mut model);
    let bar = compare_bar(&app, &model);
    assert_eq!((bar.to_right.copies, bar.to_right.replaces), (2, 0));
    assert_eq!(bar.to_left, SyncCount::default(), "nothing to bring left");
    let mut cmd = app.update(sync(true), &mut model);
    assert!(cmd.effects().next().is_none());
    assert_eq!(compare_bar(&app, &model).asking, None);
    let mut cmd = app.update(sync(false), &mut model);
    let run = file_ops(&mut cmd).pop().expect("an operation runs");
    assert_eq!(pairs_copied(&run), new_ones);
    assert_eq!(run.operation.op.describe(), "Copying 2 items to b");
    assert_eq!(run.operation.op.describe_done(), "Copied 2 items to b");
    let _ = finish_op(&app, &mut model, &mut cmd, run, &[]);

    // Everything, with nothing selected: three files would be replaced, so
    // it is asked first.
    let _ = app.update(Event::ClearSelection { tab }, &mut model);
    let mut cmd = app.update(sync(false), &mut model);
    assert!(file_ops(&mut cmd).is_empty());
    assert_eq!(
        compare_bar(&app, &model).asking,
        Some(SyncAsk {
            to_left: false,
            from: "a".to_owned(),
            into: "b".to_owned(),
            count: SyncCount {
                copies: 5,
                replaces: 3,
                newer: 1
            },
        })
    );
    // Closed without an answer: nothing is copied.
    let mut cmd = app.update(answer(ConflictChoice::Cancel), &mut model);
    assert!(file_ops(&mut cmd).is_empty());
    assert_eq!(compare_bar(&app, &model).asking, None);
    let mut cmd = app.update(answer(ConflictChoice::Replace), &mut model);
    assert!(file_ops(&mut cmd).is_empty(), "no question, no answer");

    // Skipping the ones that replace a file copies the new ones alone.
    let _ = app.update(sync(false), &mut model);
    let mut cmd = app.update(answer(ConflictChoice::Skip), &mut model);
    let run = file_ops(&mut cmd).pop().expect("an operation runs");
    assert_eq!(pairs_copied(&run), new_ones);
    let _ = finish_op(&app, &mut model, &mut cmd, run, &[]);

    // Replacing copies them all: from the left, whichever file the row
    // shows, and never the file that is only on the right.
    let _ = app.update(sync(false), &mut model);
    let mut cmd = app.update(answer(ConflictChoice::Replace), &mut model);
    let run = file_ops(&mut cmd).pop().expect("an operation runs");
    let mut all = new_ones.to_vec();
    all.extend(
        [
            ("/l/a/sub/newer left.txt", "/r/b/sub/newer left.txt"),
            ("/l/a/newer right.txt", "/r/b/newer right.txt"),
            ("/l/a/odd.txt", "/r/b/odd.txt"),
        ]
        .map(|(from, to)| (from.to_owned(), to.to_owned())),
    );
    assert_eq!(pairs_copied(&run), all);
    assert_eq!(compare_bar(&app, &model).asking, None);
}

#[test]
fn what_was_copied_is_compared_again_and_leaves_once_the_folders_agree() {
    use Difference::{NewerLeft, Same};
    let app = DirFleet;
    let mut model = Model::default();
    let tab = compare_tab(&app, &mut model);
    // The file only on the right, and the one newer there, go left.
    click(&app, &mut model, tab, 2, false, false);
    click(&app, &mut model, tab, 4, true, false);
    let _ = app.update(Event::SyncSelection { tab, to_left: true }, &mut model);
    let mut cmd = app.update(
        Event::AnswerSync {
            tab,
            choice: ConflictChoice::Replace,
        },
        &mut model,
    );
    let run = file_ops(&mut cmd).pop().expect("an operation runs");
    let crate::fileops::FileOp::CopyPairs { pairs, into } = &run.operation.op else {
        panic!("not a copy of pairs");
    };
    assert_eq!(slashed(&into.to_string_lossy()), "/l/a");
    let copied: Vec<(String, String)> = pairs
        .iter()
        .map(|(from, to)| {
            (
                slashed(&from.to_string_lossy()),
                slashed(&to.to_string_lossy()),
            )
        })
        .collect();
    assert_eq!(
        copied,
        [
            (
                "/r/b/sub/only right.txt".to_owned(),
                "/l/a/sub/only right.txt".to_owned()
            ),
            (
                "/r/b/newer right.txt".to_owned(),
                "/l/a/newer right.txt".to_owned()
            ),
        ]
    );

    // The folders are not compared again: the paths in the folders that
    // changed are, on whichever side their rows' files are.
    let mut done = finish_op(&app, &mut model, &mut cmd, run, &["/l/a", "/l/a/sub"]);
    let effects: Vec<Effect> = done.effects().collect();
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::CompareFolders(_)))
    );
    let mut check = effects
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Recompare(request) => Some(request),
            _ => None,
        })
        .expect("the paths are compared again");
    assert!(!check.operation.contents);
    assert_eq!(pairs_asked(&check), every_pair());
    check
        .resolve(vec![
            Difference::OnlyLeft,
            Same,
            NewerLeft,
            Same,
            Difference::Different,
            Difference::OnlyLeft,
        ])
        .expect("resolves");
    let event = done.events().next().expect("the answer comes back");
    let cmd = app.update(event, &mut model);
    answer_order(&app, &mut model, cmd);
    let (groups, _) = groups_shown(&app, &model);
    assert_eq!(
        groups,
        [
            "Only in a 0+2",
            "Newer in a 2+1",
            "Different, though changed at the same time 3+1"
        ]
    );
    let bar = shown(&app, &model).results.expect("a results tab");
    assert_eq!(
        bar.summary,
        "4 differences: 2 only in a, 1 newer in a, 1 different"
    );
}

/// The paths [`DIFFERING`] is about, each in the left folder and in the
/// right one.
fn every_pair() -> Vec<(String, String)> {
    [
        "only left.txt",
        "sub/only right.txt",
        "sub/newer left.txt",
        "newer right.txt",
        "odd.txt",
        "sub/second.txt",
    ]
    .iter()
    .map(|path| (format!("/l/a/{path}"), format!("/r/b/{path}")))
    .collect()
}

/// The paths `check` asks about.
fn pairs_asked(check: &Request<Recompare>) -> Vec<(String, String)> {
    check
        .operation
        .pairs
        .iter()
        .map(|(left, right)| (slashed(left), slashed(right)))
        .collect()
}

/// Has the shown tab's results checked after a file operation in
/// `affected`, answers with `kinds`, and returns the paths asked about.
fn recompared(
    app: &DirFleet,
    model: &mut Model,
    affected: &str,
    kinds: Vec<Difference>,
) -> Vec<(String, String)> {
    let mut checks = model
        .tabs
        .iter()
        .filter_map(|(id, state)| state.check_results(id.number(), &[affected.to_owned()]))
        .collect::<Vec<_>>();
    let mut cmd = checks.pop().expect("the results are checked");
    let mut check = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::Recompare(request) => Some(request),
            _ => None,
        })
        .expect("the paths are compared again");
    let asked = pairs_asked(&check);
    check.resolve(kinds).expect("resolves");
    let event = cmd.events().next().expect("the answer comes back");
    let cmd = app.update(event, model);
    answer_order(app, model, cmd);
    asked
}

#[test]
fn a_path_that_differs_again_comes_back_to_the_results() {
    use Difference::{NewerLeft, OnlyLeft, OnlyRight, Same};
    let app = DirFleet;
    let mut model = Model::default();
    let _ = compare_tab(&app, &mut model);
    let every = every_pair();
    let in_sub = [every[1].clone(), every[2].clone(), every[5].clone()];
    // The files under `sub` come to agree: on either side, whichever
    // folder the operation touched.
    let asked = recompared(&app, &mut model, "/r/b/sub", vec![Same, Same, Same]);
    assert_eq!(asked, in_sub);
    let (groups, _) = groups_shown(&app, &model);
    assert_eq!(
        groups,
        [
            "Only in a 0+1",
            "Newer in b 1+1",
            "Different, though changed at the same time 2+1"
        ]
    );
    // Undo takes the copies back: the paths are asked about again though
    // they no longer differ, and their rows are back under their headings.
    let asked = recompared(
        &app,
        &mut model,
        "/l/a/sub",
        vec![OnlyRight, NewerLeft, OnlyLeft],
    );
    assert_eq!(asked, in_sub);
    let (groups, _) = groups_shown(&app, &model);
    assert_eq!(
        groups,
        [
            "Only in a 0+2",
            "Only in b 2+1",
            "Newer in a 3+1",
            "Newer in b 4+1",
            "Different, though changed at the same time 5+1"
        ]
    );
}

#[test]
fn reading_the_files_compares_again_and_a_comparison_is_stopped_and_left() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = compare_tab(&app, &mut model);
    let contents = |contents| Event::SetCompareOptions { tab, contents };

    // Asking for what is already so compares nothing again.
    let mut cmd = app.update(contents(false), &mut model);
    assert!(cmd.effects().next().is_none());
    let mut cmd = app.update(contents(true), &mut model);
    let (stopped, started) = compares(&mut cmd);
    let scan = started.expect("the folders are compared again");
    assert_eq!(stopped, Vec::<u64>::new(), "nothing ran");
    assert!(scan.operation.contents);
    let first = scan.operation.id;
    let bar = shown(&app, &model).results.expect("a results tab");
    assert!(bar.running);
    assert!(compare_bar(&app, &model).contents);

    // Nothing is copied while the folders are being compared.
    assert_eq!(compare_bar(&app, &model).to_right, SyncCount::default());
    let mut cmd = app.update(
        Event::SyncSelection {
            tab,
            to_left: false,
        },
        &mut model,
    );
    assert!(cmd.effects().next().is_none());
    assert_eq!(compare_bar(&app, &model).asking, None);

    // Stopped: what the comparison before found stays.
    let mut cmd = app.update(Event::StopResults { tab }, &mut model);
    assert_eq!(compares(&mut cmd).0, [first]);
    let bar = shown(&app, &model).results.expect("a results tab");
    assert!(!bar.running);
    assert_eq!(
        bar.summary,
        "6 differences: 2 only in a, 1 only in b, 1 newer in a, 1 newer in b, 1 different. \
         The new scan was stopped"
    );

    // Refresh compares again; duplicates' sets mean nothing here.
    let mut cmd = app.update(Event::Refresh { tab }, &mut model);
    let (stopped, started) = compares(&mut cmd);
    let second = started.expect("compared again").operation.id;
    assert_eq!(stopped, Vec::<u64>::new());
    assert_ne!(second, first);
    let _ = app.update(
        Event::SelectDuplicates {
            tab,
            keep: Keep::Newest,
        },
        &mut model,
    );
    assert_eq!(rows_selected(&app, &model), Vec::<String>::new());

    // Going to a folder ends the comparison and the results.
    let mut cmd = app.update(
        Event::Navigate {
            tab,
            path: "/x".into(),
        },
        &mut model,
    );
    assert_eq!(compares(&mut cmd).0, [second]);
    assert_eq!(shown(&app, &model).results, None);
}

/// A window with two panes: the left one shows `/l/a`; the right one, which
/// has the focus, shows `/x` with `entries`. Returns the window and the two
/// tabs.
fn beside(app: &DirFleet, model: &mut Model, entries: &[DirEntry]) -> (u32, u32, u32) {
    let left = window(app, model);
    open(app, model, left, "/l/a", &[]);
    let _ = app.update(
        Event::SplitNew {
            tab: left,
            side: Side::Right,
        },
        model,
    );
    let right = shown(app, model).id;
    open(app, model, right, "/x", entries);
    (app.view(model).windows[0].id, left, right)
}

/// The texts of the messages shown.
fn toasts(app: &DirFleet, model: &Model) -> Vec<String> {
    app.view(model)
        .toasts
        .iter()
        .map(|toast| toast.text.clone())
        .collect()
}

/// Answers the check of names `cmd` asks for with no clash, and returns
/// the operation that then runs.
fn unclashing(
    app: &DirFleet,
    model: &mut Model,
    mut cmd: Command<Effect, Event>,
) -> crate::fileops::FileOp {
    let mut check = cmd
        .effects()
        .find_map(|effect| match effect {
            Effect::FindConflicts(request) => Some(request),
            _ => None,
        })
        .expect("names are checked");
    check.resolve(Conflicts(Vec::new())).expect("resolves");
    let event = cmd.events().next().expect("the answer comes back");
    let mut cmd = app.update(event, model);
    let run = file_ops(&mut cmd).pop().expect("an operation runs");
    run.operation.op
}

#[test]
fn the_selection_is_copied_or_moved_into_the_folder_the_other_pane_shows() {
    let app = DirFleet;
    let mut model = Model::default();
    model.settings.queue_mode = QueueMode::Unlimited;
    let (window, left, right) = beside(
        &app,
        &mut model,
        &[entry("one.txt", false), entry("two.txt", false)],
    );
    let other = |model: &Model| app.view(model).windows[0].other_pane.clone();
    assert_eq!(
        other(&model),
        Some(OtherPane {
            tab: left,
            title: "a".to_owned(),
            path: "/l/a".to_owned(),
            writable: true,
            reason: String::new(),
        })
    );
    click(&app, &mut model, right, 0, false, false);
    let cmd = app.update(Event::CopyToOtherPane { window }, &mut model);
    assert!(matches!(
        unclashing(&app, &mut model, cmd),
        crate::fileops::FileOp::Copy { srcs, dst, .. }
            if srcs == [Path::new("/x").join("one.txt")] && dst == Path::new("/l/a")
    ));
    click(&app, &mut model, right, 1, false, false);
    let cmd = app.update(Event::MoveToOtherPane { window }, &mut model);
    assert!(matches!(
        unclashing(&app, &mut model, cmd),
        crate::fileops::FileOp::Move { srcs, dst, .. }
            if srcs == [Path::new("/x").join("two.txt")] && dst == Path::new("/l/a")
    ));

    // The other pane is the one after the focused pane: from the left
    // pane, it is the right one.
    let _ = app.update(Event::ActivateTab { tab: left }, &mut model);
    assert_eq!(other(&model).map(|pane| pane.path), Some("/x".to_owned()));
    let _ = app.update(Event::ActivateTab { tab: right }, &mut model);

    // Nothing is put where nothing can be: the reason is said instead.
    let _ = app.update(Event::OpenTrash { tab: left }, &mut model);
    let pane = other(&model).expect("the other pane");
    assert!(!pane.writable);
    assert_eq!(
        pane.reason,
        "Things get into the Recycle Bin only by being deleted"
    );
    let mut cmd = app.update(Event::CopyToOtherPane { window }, &mut model);
    assert!(
        cmd.effects()
            .all(|effect| !matches!(effect, Effect::RunFileOp(_) | Effect::FindConflicts(_)))
    );
    assert_eq!(toasts(&app, &model), [pane.reason]);
}

#[test]
fn a_window_of_one_pane_has_no_other_pane_to_act_with() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("one.txt", false)]);
    let window = app.view(&model).windows[0].id;
    assert_eq!(app.view(&model).windows[0].other_pane, None);
    click(&app, &mut model, tab, 0, false, false);
    for (event, said) in [
        (
            Event::CopyToOtherPane { window },
            "There is no other pane to copy to",
        ),
        (
            Event::MoveToOtherPane { window },
            "There is no other pane to move to",
        ),
        (Event::SwapPanes { window }, "This window has one pane"),
    ] {
        let mut cmd = app.update(event, &mut model);
        assert!(cmd.effects().all(|effect| !matches!(
            effect,
            Effect::RunFileOp(_) | Effect::FindConflicts(_) | Effect::ListDir(_)
        )));
        assert_eq!(toasts(&app, &model).last().map(String::as_str), Some(said));
    }
}

#[test]
fn panes_change_places_and_a_folder_is_shown_in_the_other_pane_too() {
    let app = DirFleet;
    let mut model = Model::default();
    let (window, left, right) = beside(&app, &mut model, &[]);
    let panes = |model: &Model| -> (Vec<u32>, u32) {
        let window = app.view(model).windows[0].clone();
        (
            window.panes.iter().map(|pane| pane.shown.id).collect(),
            window.active.id,
        )
    };
    assert_eq!(panes(&model), (vec![left, right], right));
    let mut cmd = app.update(Event::SwapPanes { window }, &mut model);
    assert_eq!(
        listed_paths(&mut cmd),
        Vec::<String>::new(),
        "nothing is read"
    );
    assert_eq!(
        panes(&model),
        (vec![right, left], right),
        "the focus stays with its tab"
    );
    let _ = app.update(Event::SwapPanes { window }, &mut model);
    assert_eq!(panes(&model), (vec![left, right], right));

    // The focused pane's folder, shown in the other pane as well.
    let mut cmd = app.update(Event::MirrorToOtherPane { window }, &mut model);
    assert_eq!(listed_paths(&mut cmd), ["/x"]);
    let view = app.view(&model).windows[0].clone();
    assert_eq!(view.panes[0].shown.path, "/x");
    assert_eq!(view.active.id, right, "the focus stays where it was");
    // Already there: nothing is read again.
    let mut cmd = app.update(Event::MirrorToOtherPane { window }, &mut model);
    assert_eq!(listed_paths(&mut cmd), Vec::<String>::new());
    // The other pane goes back like after any visit.
    assert!(view.panes[0].shown.nav.back);
}

#[test]
fn rows_are_selected_and_deselected_by_a_pattern_written_as_a_search_is() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[
            entry("a.jpg", false),
            entry("b.JPG", false),
            entry("notes.txt", false),
            entry("docs", true),
            entry("c.png", false),
        ],
    );
    let dialog = |model: &Model| shown(&app, model).select_pattern;
    let typed = |model: &mut Model, text: &str| {
        let _ = app.update(
            Event::SetSelectPattern {
                tab,
                text: text.to_owned(),
            },
            model,
        );
        dialog(model).expect("the dialog is open")
    };
    assert_eq!(dialog(&model), None);
    // Typing with no dialog open changes nothing.
    let _ = app.update(
        Event::SetSelectPattern {
            tab,
            text: "*".to_owned(),
        },
        &mut model,
    );
    assert_eq!(dialog(&model), None);

    let _ = app.update(Event::OpenSelectPattern { tab }, &mut model);
    assert_eq!(dialog(&model), Some(SelectPatternView::default()));
    let view = typed(&mut model, "*.jpg");
    assert_eq!((view.matches, view.error.as_str()), (2, ""));
    let apply = |model: &mut Model, select| {
        let _ = app.update(Event::ApplySelectPattern { tab, select }, model);
        rows_selected(&app, model)
    };
    assert_eq!(apply(&mut model, true), ["a.jpg", "b.JPG"]);
    assert_eq!(dialog(&model), None, "applying closes the dialog");

    // The pattern used last is offered again; what matches is added.
    let _ = app.update(Event::OpenSelectPattern { tab }, &mut model);
    assert_eq!(
        dialog(&model).map(|view| view.text),
        Some("*.jpg".to_owned())
    );
    assert_eq!(typed(&mut model, "ext:png").matches, 1);
    assert_eq!(apply(&mut model, true), ["a.jpg", "b.JPG", "c.png"]);
    // Deselecting takes what matches out of the selection.
    let _ = app.update(Event::OpenSelectPattern { tab }, &mut model);
    assert_eq!(typed(&mut model, "a*").matches, 1);
    assert_eq!(apply(&mut model, false), ["b.JPG", "c.png"]);

    // Every way a search is written: kinds, words, several at once.
    let _ = app.update(Event::OpenSelectPattern { tab }, &mut model);
    assert_eq!(typed(&mut model, "kind:folder").matches, 1);
    assert_eq!(typed(&mut model, "o").matches, 2, "notes.txt and docs");
    assert_eq!(typed(&mut model, "o -notes").matches, 1);
    assert_eq!(
        typed(&mut model, "").matches,
        0,
        "nothing typed, nothing matched"
    );
    // A pattern that can't be read says why and matches nothing.
    let broken = typed(&mut model, "/[/");
    assert_eq!(broken.matches, 0);
    assert_ne!(broken.error, "");
    // Closed without applying: the selection stays, and so does the
    // pattern used last.
    let _ = app.update(Event::CloseSelectPattern { tab }, &mut model);
    assert_eq!(dialog(&model), None);
    assert_eq!(rows_selected(&app, &model), ["b.JPG", "c.png"]);
    let _ = app.update(Event::OpenSelectPattern { tab }, &mut model);
    assert_eq!(dialog(&model).map(|view| view.text), Some("a*".to_owned()));
}

#[test]
fn rows_that_end_as_the_selected_ones_do_are_selected() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[
            entry("a.jpg", false),
            entry("docs", true),
            entry("b.JPG", false),
            entry("notes.txt", false),
            entry("more", true),
            entry("README", false),
            entry("LICENSE", false),
        ],
    );
    let same = |model: &mut Model| {
        let _ = app.update(Event::SelectSameExtension { tab }, model);
        rows_selected(&app, model)
    };
    // Nothing selected and no cursor: nothing to go by.
    assert_eq!(same(&mut model), Vec::<String>::new());
    click(&app, &mut model, tab, 0, false, false);
    assert_eq!(same(&mut model), ["a.jpg", "b.JPG"], "whatever the case");
    // Folders go with folders, files without an ending with their like.
    click(&app, &mut model, tab, 1, false, false);
    assert_eq!(same(&mut model), ["docs", "more"]);
    click(&app, &mut model, tab, 5, false, false);
    click(&app, &mut model, tab, 3, true, false);
    assert_eq!(same(&mut model), ["notes.txt", "README", "LICENSE"]);
}

#[test]
fn no_two_commands_share_a_shortcut_and_each_has_a_name() {
    for shortcut in [CommandId::mac_shortcut, CommandId::other_shortcut] {
        let mut seen: Vec<&str> = Vec::new();
        for command in CommandId::ALL {
            assert_ne!(command.label(), "", "{command:?}");
            let keys = shortcut(command);
            if keys.is_empty() {
                continue;
            }
            assert!(!seen.contains(&keys), "{keys} is taken twice ({command:?})");
            seen.push(keys);
        }
    }
    // The two-pane commands have keys where function keys are at hand.
    assert_eq!(CommandId::CopyToOtherPane.other_shortcut(), "Shift+F5");
    assert_eq!(CommandId::MoveToOtherPane.other_shortcut(), "Shift+F6");
}

/// A tab at `/x` with `entries`, all of them selected and the batch rename
/// dialog open.
fn renaming_all(app: &DirFleet, model: &mut Model, entries: &[DirEntry]) -> u32 {
    let tab = window(app, model);
    open(app, model, tab, "/x", entries);
    let _ = app.update(Event::SelectAll { tab }, model);
    let _ = app.update(Event::StartBatchRename { tab }, model);
    tab
}

/// The new names the dialog shows, each with its problem when it has one.
fn new_names(app: &DirFleet, model: &Model) -> Vec<String> {
    batch(app, model)
        .items
        .into_iter()
        .map(|item| {
            if item.problem.is_empty() {
                item.new
            } else {
                format!("{} ({})", item.new, item.problem)
            }
        })
        .collect()
}

#[test]
fn names_are_numbered_by_a_counter_that_starts_steps_and_pads_as_told() {
    let app = DirFleet;
    let mut model = Model::default();
    let entries = [
        entry("c.jpg", false),
        entry("a.jpg", false),
        entry("b.jpg", false),
    ];
    let tab = renaming_all(&app, &mut model, &entries);
    assert_eq!(batch(&app, &model).fields, BatchFields::default());
    assert_eq!(
        new_names(&app, &model),
        ["c.jpg", "a.jpg", "b.jpg"],
        "as it opens, nothing changes"
    );
    let numbered = |model: &mut Model, start, step, padding| {
        let fields = BatchFields {
            template: "{n} {name}{ext}".into(),
            start,
            step,
            padding,
            ..BatchFields::default()
        };
        let _ = app.update(Event::SetBatchRename { tab, fields }, model);
        new_names(&app, model)
    };
    // In the order shown, as many digits as the last number needs.
    assert_eq!(
        numbered(&mut model, 1, 1, 0),
        ["1 c.jpg", "2 a.jpg", "3 b.jpg"]
    );
    assert_eq!(
        numbered(&mut model, 8, 1, 0),
        ["08 c.jpg", "09 a.jpg", "10 b.jpg"]
    );
    assert_eq!(
        numbered(&mut model, 10, 10, 4),
        ["0010 c.jpg", "0020 a.jpg", "0030 b.jpg"]
    );
    // A step of nothing gives every item the same number.
    assert_eq!(
        numbered(&mut model, 5, 0, 0),
        ["5 c.jpg", "5 a.jpg", "5 b.jpg"]
    );
    let view = batch(&app, &model);
    assert_eq!((view.total, view.changes, view.problems), (3, 3, 0));
}

#[test]
fn names_may_be_swapped_or_handed_on_but_not_shared_or_taken_from_others() {
    let app = DirFleet;
    let mut model = Model::default();
    let entries = [
        entry("a.txt", false),
        entry("b.txt", false),
        entry("other.txt", false),
    ];
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &entries);
    click(&app, &mut model, tab, 0, false, false);
    click(&app, &mut model, tab, 1, true, false);
    let _ = app.update(Event::StartBatchRename { tab }, &mut model);
    let set = |model: &mut Model, fields: BatchFields| {
        let _ = app.update(Event::SetBatchRename { tab, fields }, model);
        new_names(&app, model)
    };
    let regex = |find: &str, replace: &str| BatchFields {
        find: find.into(),
        replace: replace.into(),
        regex: true,
        ..BatchFields::default()
    };
    // a takes b's name while b moves on: the name is free by then.
    assert_eq!(
        set(&mut model, regex("^b", "c")),
        ["a.txt", "c.txt"],
        "one name changes"
    );
    let handed_on = BatchFields {
        template: "{n}{ext}".into(),
        ..regex("^1", "b")
    };
    let _ = set(&mut model, handed_on);
    // Counted 1 and 2, then 1 becomes b: "b.txt" and "2.txt".
    assert_eq!(new_names(&app, &model), ["b.txt", "2.txt"]);
    assert_eq!(batch(&app, &model).problems, 0);
    let mut cmd = app.update(Event::ApplyBatchRename { tab }, &mut model);
    let run = file_ops(&mut cmd).pop().expect("the renames run");
    let x = Path::new("/x");
    assert_eq!(
        run.operation.op,
        crate::fileops::FileOp::RenameMany(vec![
            (x.join("a.txt"), x.join("b.txt")),
            (x.join("b.txt"), x.join("2.txt")),
        ])
    );

    // The name of an item that is not being renamed can't be taken, however
    // it is written; nor can two items share one.
    let _ = app.update(Event::StartBatchRename { tab }, &mut model);
    assert_eq!(
        set(&mut model, regex("^a", "OTHER")),
        [
            "OTHER.txt (An item with this name already exists.)",
            "b.txt"
        ]
    );
    assert_eq!(
        set(&mut model, regex("^.", "Same")),
        [
            "Same.txt (Two items would get this name.)",
            "Same.txt (Two items would get this name.)"
        ]
    );
    // Its own name written another way is an item's to take.
    let upper = BatchFields {
        case: BatchCase::Upper,
        ..BatchFields::default()
    };
    assert_eq!(set(&mut model, upper), ["A.TXT", "B.TXT"]);
    assert_eq!(batch(&app, &model).problems, 0);
}

#[test]
fn an_expression_that_cannot_be_read_stops_the_rename_and_says_why() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = renaming_all(
        &app,
        &mut model,
        &[entry("a.txt", false), entry("b.txt", false)],
    );
    let fields = BatchFields {
        template: "x{n}".into(),
        find: "(".into(),
        regex: true,
        ..BatchFields::default()
    };
    let _ = app.update(Event::SetBatchRename { tab, fields }, &mut model);
    let view = batch(&app, &model);
    assert_ne!(view.error, "");
    assert_eq!((view.changes, view.problems), (2, 0));
    let mut cmd = app.update(Event::ApplyBatchRename { tab }, &mut model);
    assert!(file_ops(&mut cmd).is_empty(), "not while it can't be read");
    assert!(shown(&app, &model).batch_rename.is_some());
}

// `{modified}` is the template's own placeholder.
#[allow(clippy::literal_string_with_formatting_args)]
#[test]
fn dates_in_names_are_local_once_the_system_has_said_how_far_from_utc() {
    let app = DirFleet;
    let mut model = Model::default();
    // 2025-11-08 23:30 and 2025-07-01 12:00, UTC.
    let mut late = entry("late.jpg", false);
    late.meta.modified_at = TimestampSeconds::from_unix(1_762_644_600);
    let mut noon = entry("noon.jpg", false);
    noon.meta.modified_at = TimestampSeconds::from_unix(1_751_371_200);
    let tab = renaming_all(&app, &mut model, &[late, noon]);
    let dated = BatchFields {
        template: "{modified} {name}{ext}".into(),
        ..BatchFields::default()
    };
    let asked = |cmd: &mut Command<Effect, Event>| {
        cmd.effects().find_map(|effect| match effect {
            Effect::LocalOffsets(request) => Some(request),
            _ => None,
        })
    };
    // Nothing is asked while no name takes a date.
    let plain = BatchFields {
        template: "{n}{ext}".into(),
        ..BatchFields::default()
    };
    let mut cmd = app.update(Event::SetBatchRename { tab, fields: plain }, &mut model);
    assert!(asked(&mut cmd).is_none());

    let set = Event::SetBatchRename { tab, fields: dated };
    let mut cmd = app.update(set.clone(), &mut model);
    let mut request = asked(&mut cmd).expect("local time is asked about");
    assert_eq!(
        request.operation.times,
        [1_762_644_600, 0, 1_751_371_200, 0],
        "each item's time changed and time made"
    );
    assert_eq!(
        new_names(&app, &model),
        ["2025-11-08 late.jpg", "2025-07-01 noon.jpg"],
        "UTC until the answer comes"
    );
    // One hour east in winter, two in summer.
    request
        .resolve(vec![3600, 3600, 7200, 7200])
        .expect("resolves");
    let event = cmd.events().next().expect("the answer comes back");
    let _ = app.update(event, &mut model);
    assert_eq!(
        new_names(&app, &model),
        ["2025-11-09 late.jpg", "2025-07-01 noon.jpg"],
        "half past midnight, the next day"
    );
    // Asked once: the answer holds while the dialog is open.
    let mut cmd = app.update(set, &mut model);
    assert!(asked(&mut cmd).is_none());
    // An answer about other items is not taken.
    let _ = app.update(
        Event::BatchOffsets {
            tab,
            times: vec![1, 2, 3, 4],
            offsets: vec![-3600; 4],
        },
        &mut model,
    );
    assert_eq!(new_names(&app, &model)[0], "2025-11-09 late.jpg");
}

#[test]
fn a_long_selection_is_shown_by_its_beginning_after_what_cannot_be_used() {
    let app = DirFleet;
    let mut model = Model::default();
    let entries: Vec<DirEntry> = (0..BATCH_PREVIEW + 50)
        .map(|index| entry(&format!("f{index:03}.txt"), false))
        .collect();
    let tab = renaming_all(&app, &mut model, &entries);
    let view = batch(&app, &model);
    assert_eq!(view.items.len(), BATCH_PREVIEW);
    assert_eq!(view.total as usize, BATCH_PREVIEW + 50);
    assert_eq!(view.items[0].old, "f000.txt");

    // The last two would share a name: they are shown first, though they
    // are past what the dialog is sent.
    let fields = BatchFields {
        find: "f24[89]".into(),
        replace: "twin".into(),
        regex: true,
        ..BatchFields::default()
    };
    let _ = app.update(Event::SetBatchRename { tab, fields }, &mut model);
    let view = batch(&app, &model);
    assert_eq!((view.changes, view.problems), (2, 2));
    assert_eq!(view.items.len(), BATCH_PREVIEW);
    let first: Vec<&str> = view.items[..3]
        .iter()
        .map(|item| item.old.as_str())
        .collect();
    assert_eq!(first, ["f248.txt", "f249.txt", "f000.txt"]);
}

#[test]
fn several_items_are_renamed_only_where_names_can_be_changed() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    let entries = [entry("a.txt", false), entry("b.txt", false)];
    // On a server items are renamed one at a time.
    open(&app, &mut model, tab, "sftp://office/files", &entries);
    let _ = app.update(Event::SelectAll { tab }, &mut model);
    let _ = app.update(Event::StartBatchRename { tab }, &mut model);
    assert_eq!(shown(&app, &model).batch_rename, None);
    // One item is renamed in place, not in the dialog.
    open(&app, &mut model, tab, "/x", &entries);
    click(&app, &mut model, tab, 0, false, false);
    let _ = app.update(Event::StartBatchRename { tab }, &mut model);
    assert_eq!(shown(&app, &model).batch_rename, None);
    let _ = app.update(Event::SelectAll { tab }, &mut model);
    let _ = app.update(Event::StartBatchRename { tab }, &mut model);
    assert!(shown(&app, &model).batch_rename.is_some());
}

/// The command as the view has it.
fn command_view(app: &DirFleet, model: &Model, command: CommandId) -> CommandView {
    app.view(model)
        .commands
        .into_iter()
        .find(|view| view.id == command)
        .expect("every command is listed")
}

/// `text` as this system shows shortcuts.
fn shown_as(text: &str) -> String {
    crate::shortcut::parse_command(text)
        .expect("a shortcut")
        .display()
}

fn set_shortcut(app: &DirFleet, model: &mut Model, command: CommandId, text: &str) {
    let _ = app.update(
        Event::SetShortcut {
            command,
            text: text.to_owned(),
        },
        model,
    );
}

#[test]
fn a_command_is_given_another_shortcut_which_is_kept_or_taken_away_or_reset() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = start(&app, &mut model, None, LoadedState::Missing);
    let command = CommandId::NewTab;
    let own = command.shortcut().to_owned();
    let before = command_view(&app, &model, command);
    assert_eq!(
        (
            before.shortcut.as_str(),
            before.default_shortcut.as_str(),
            before.customized,
            before.category.as_str()
        ),
        (own.as_str(), own.as_str(), false, "Tabs and Windows")
    );
    assert_eq!(before.key.map(|key| key.key), Some("T".to_owned()));

    let _ = app.update(Event::EditShortcut { command }, &mut model);
    assert_eq!(app.view(&model).keymap.recording, Some(command));
    set_shortcut(&app, &mut model, command, "Ctrl+Alt+W");
    let keymap = app.view(&model).keymap;
    assert_eq!((keymap.recording, keymap.customized), (None, 1));
    let after = command_view(&app, &model, command);
    assert_eq!(after.shortcut, shown_as("Ctrl+Alt+W"));
    assert_eq!(after.default_shortcut, own);
    assert!(after.customized);
    assert_eq!(
        after.key,
        Some(KeyView {
            key: "W".to_owned(),
            command: true,
            control: false,
            alt: true,
            shift: false,
        })
    );
    // The palette shows it too.
    let _ = app.update(
        Event::SetPaletteText {
            text: "new tab".to_owned(),
        },
        &mut model,
    );
    let palette = app.view(&model).palette.expect("the palette is open");
    assert_eq!(palette.items[0].detail, shown_as("Ctrl+Alt+W"));
    let _ = app.update(Event::ClosePalette, &mut model);

    // Kept between launches, by the command's name.
    let saved = DirFleet::persisted(&model);
    assert_eq!(
        saved.keymap,
        BTreeMap::from([("NewTab".to_owned(), "Ctrl+Alt+W".to_owned())])
    );
    let mut again = Model::default();
    let _ = start(&app, &mut again, None, LoadedState::Found(saved));
    assert_eq!(
        command_view(&app, &again, command).shortcut,
        shown_as("Ctrl+Alt+W")
    );

    // Taken away: the command has no key, which is kept too.
    set_shortcut(&app, &mut model, command, "");
    let none = command_view(&app, &model, command);
    assert_eq!(
        (none.shortcut.as_str(), none.key, none.customized),
        ("", None, true)
    );
    assert_eq!(
        DirFleet::persisted(&model).keymap,
        BTreeMap::from([("NewTab".to_owned(), String::new())])
    );
    // Reset: the app's own again, and nothing to keep.
    let _ = app.update(Event::ResetShortcut { command }, &mut model);
    assert_eq!(
        command_view(&app, &model, command),
        before_as(&app, command)
    );
    assert_eq!(DirFleet::persisted(&model).keymap, BTreeMap::new());
    // Giving a command the shortcut it has anyway keeps nothing either.
    let neutral = command
        .default_shortcut()
        .expect("a shortcut")
        .neutral_text();
    set_shortcut(&app, &mut model, command, &neutral);
    assert_eq!(DirFleet::persisted(&model).keymap, BTreeMap::new());
}

/// The command as a model that changed nothing shows it.
fn before_as(app: &DirFleet, command: CommandId) -> CommandView {
    command_view(app, &Model::default(), command)
}

#[test]
fn a_shortcut_something_else_has_is_taken_only_once_it_is_confirmed() {
    let app = DirFleet;
    let mut model = Model::default();
    let (asker, holder) = (CommandId::NewTab, CommandId::Refresh);
    let wanted = holder.default_shortcut().expect("a shortcut");
    set_shortcut(&app, &mut model, asker, &wanted.neutral_text());
    assert_eq!(
        app.view(&model).keymap.conflict,
        Some(ShortcutConflictView {
            command: asker,
            shortcut: wanted.display(),
            holder: holder.label().to_owned(),
        })
    );
    // Nothing changes while it is asked, nor when the answer is no.
    assert_eq!(command_view(&app, &model, asker), before_as(&app, asker));
    let _ = app.update(Event::CancelShortcut, &mut model);
    assert_eq!(app.view(&model).keymap, KeymapView::default());
    assert_eq!(command_view(&app, &model, holder), before_as(&app, holder));
    let _ = app.update(Event::ConfirmShortcut, &mut model);
    assert_eq!(
        command_view(&app, &model, asker),
        before_as(&app, asker),
        "no question, no answer"
    );

    // Confirmed: the shortcut changes hands, and its holder is left with none.
    set_shortcut(&app, &mut model, asker, &wanted.neutral_text());
    let _ = app.update(Event::ConfirmShortcut, &mut model);
    assert_eq!(command_view(&app, &model, asker).shortcut, wanted.display());
    let left = command_view(&app, &model, holder);
    assert_eq!((left.shortcut.as_str(), left.customized), ("", true));
    let keymap = app.view(&model).keymap;
    assert_eq!((keymap.conflict, keymap.customized), (None, 2));
    // No two commands ever share a shortcut.
    let mut shortcuts: Vec<String> = app
        .view(&model)
        .commands
        .into_iter()
        .map(|view| view.shortcut)
        .filter(|shortcut| !shortcut.is_empty())
        .collect();
    let count = shortcuts.len();
    shortcuts.sort();
    shortcuts.dedup();
    assert_eq!(shortcuts.len(), count);

    // Resetting the holder asks the same question the other way round.
    let _ = app.update(Event::ResetShortcut { command: holder }, &mut model);
    assert_eq!(
        app.view(&model)
            .keymap
            .conflict
            .map(|conflict| conflict.holder),
        Some(asker.label().to_owned())
    );
    let _ = app.update(Event::ConfirmShortcut, &mut model);
    assert_eq!(command_view(&app, &model, holder), before_as(&app, holder));
    assert_eq!(command_view(&app, &model, asker).shortcut, "");

    // Every command gets the app's own back at once.
    let _ = app.update(Event::ResetAllShortcuts, &mut model);
    assert_eq!(
        app.view(&model).commands,
        app.view(&Model::default()).commands
    );
    assert_eq!(DirFleet::persisted(&model).keymap, BTreeMap::new());
}

#[test]
fn a_shortcut_that_cannot_be_taken_is_refused_with_the_reason() {
    let app = DirFleet;
    let mut model = Model::default();
    let command = CommandId::NewTab;
    let _ = app.update(Event::EditShortcut { command }, &mut model);
    for (text, reason) in [
        (
            "Down",
            "Down moves through lists and dialogs, and stays that way",
        ),
        ("M", "\"M\" needs Ctrl or Alt, so typing a name still works"),
        ("Ctrl+Nope", "unknown key \"Nope\" in \"Ctrl+Nope\""),
    ] {
        set_shortcut(&app, &mut model, command, text);
        let keymap = app.view(&model).keymap;
        assert_eq!(keymap.error, reason);
        assert_eq!(keymap.recording, Some(command), "another key may do");
        assert_eq!(
            command_view(&app, &model, command),
            before_as(&app, command)
        );
    }
    // A key that can be taken ends it.
    set_shortcut(&app, &mut model, command, "Ctrl+Alt+F9");
    assert_eq!(
        app.view(&model).keymap,
        KeymapView {
            customized: 1,
            ..KeymapView::default()
        }
    );
    // What was saved and no longer reads, or names no command, is dropped.
    let mut saved = DirFleet::persisted(&model);
    saved.keymap = BTreeMap::from([
        ("NewTab".to_owned(), "Ctrl+Alt+F9".to_owned()),
        ("Refresh".to_owned(), "nonsense".to_owned()),
        ("NoSuchCommand".to_owned(), "Ctrl+Alt+F8".to_owned()),
    ]);
    let mut again = Model::default();
    let _ = start(&app, &mut again, None, LoadedState::Found(saved));
    assert_eq!(
        DirFleet::persisted(&again).keymap,
        BTreeMap::from([("NewTab".to_owned(), "Ctrl+Alt+F9".to_owned())])
    );
}

#[test]
fn commands_and_plugin_actions_take_shortcuts_from_each_other_only_when_told() {
    let app = DirFleet;
    let mut model = Model::default();
    let _ = keys_running(&app, &mut model);
    let tidy = "io.example.keys.tidy";
    let bound = |model: &Model| -> Vec<String> {
        app.view(model)
            .plugin_shortcuts
            .into_iter()
            .map(|binding| binding.action)
            .collect()
    };
    assert!(bound(&model).contains(&tidy.to_owned()));
    // A command asks for the action's shortcut: the action is named.
    let command = CommandId::NewTab;
    set_shortcut(&app, &mut model, command, "Ctrl+Alt+M");
    assert_eq!(
        app.view(&model)
            .keymap
            .conflict
            .map(|conflict| conflict.holder),
        Some("Tidy".to_owned())
    );
    let _ = app.update(Event::ConfirmShortcut, &mut model);
    assert_eq!(
        command_view(&app, &model, command).shortcut,
        shown_as("Ctrl+Alt+M")
    );
    assert!(
        !bound(&model).contains(&tidy.to_owned()),
        "the action let go"
    );

    // An action can't take a shortcut a command was given.
    set_shortcut(&app, &mut model, CommandId::Refresh, "Ctrl+Alt+K");
    let _ = app.update(
        Event::SetPluginShortcut {
            plugin: "io.example.keys".to_owned(),
            action: "tidy".to_owned(),
            text: "Ctrl+Alt+K".to_owned(),
        },
        &mut model,
    );
    assert!(!bound(&model).contains(&tidy.to_owned()));
    let note = app.view(&model).plugins.plugins[0].shortcuts[0]
        .note
        .clone();
    assert_eq!(
        note,
        format!(
            "{} is used by \u{201c}Refresh\u{201d}",
            shown_as("Ctrl+Alt+K")
        )
    );
    // The shortcut a command gave up is free for it.
    let _ = app.update(
        Event::SetPluginShortcut {
            plugin: "io.example.keys".to_owned(),
            action: "tidy".to_owned(),
            text: CommandId::Refresh
                .default_shortcut()
                .expect("a shortcut")
                .neutral_text(),
        },
        &mut model,
    );
    assert!(bound(&model).contains(&tidy.to_owned()));
}

/// What `cmd` asks of the tags.
fn tag_ops(cmd: &mut Command<Effect, Event>) -> Vec<Request<TagOp>> {
    cmd.effects()
        .filter_map(|effect| match effect {
            Effect::Tags(request) => Some(request),
            _ => None,
        })
        .collect()
}

/// Tells the app which item has which tag now.
fn tagged_now(
    app: &DirFleet,
    model: &mut Model,
    tagged: &[(&str, &[u32])],
) -> Command<Effect, Event> {
    let tagged = Tagged(
        tagged
            .iter()
            .map(|(path, tags)| ((*path).to_owned(), tags.to_vec()))
            .collect(),
    );
    app.update(Event::TagsChanged { tagged }, model)
}

/// The item `name` of `/x`, as rows name their items.
fn in_x(name: &str) -> String {
    crate::location::Location::parse("/x")
        .join(name)
        .to_string()
}

#[test]
fn tags_are_given_to_the_selection_shown_on_its_rows_and_taken_again() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[
            entry("a.txt", false),
            entry("b.txt", false),
            entry("docs", true),
        ],
    );
    // Seven tags to start with, one of each colour, on nothing yet.
    let tags = app.view(&model).tags;
    let names: Vec<&str> = tags.iter().map(|tag| tag.name.as_str()).collect();
    assert_eq!(
        names,
        ["Red", "Orange", "Yellow", "Green", "Blue", "Purple", "Gray"]
    );
    assert_eq!(
        tags[3],
        TagView {
            id: 4,
            name: "Green".to_owned(),
            color: TagColor::Green,
            count: 0,
        }
    );

    click(&app, &mut model, tab, 0, false, false);
    click(&app, &mut model, tab, 1, true, false);
    // The first thing the tags are told is their names, to search by.
    let mut cmd = app.update(Event::ToggleTag { tab, tag: 4 }, &mut model);
    let mut names = tag_ops(&mut cmd).pop().expect("the tags are told");
    assert!(matches!(&names.operation, TagOp::Names(names) if names.len() == 7));
    names.resolve(Tagged::default()).expect("resolves");
    let op = tag_ops(&mut cmd).pop().expect("then what to tag").operation;
    assert_eq!(
        op,
        TagOp::Set {
            paths: vec![in_x("a.txt"), in_x("b.txt")],
            tag: 4,
            on: true
        }
    );
    // The effects answer with every item's tags: rows show them as dots
    // of the tags' colours, in the tags' order.
    let (a, b) = (in_x("a.txt"), in_x("b.txt"));
    let _ = tagged_now(&app, &mut model, &[(&a, &[4, 1]), (&b, &[4])]);
    let view = shown(&app, &model);
    assert_eq!(view.rows[0].tags, [TagColor::Red, TagColor::Green]);
    assert_eq!(view.rows[1].tags, [TagColor::Green]);
    assert_eq!(view.rows[2].tags, Vec::<TagColor>::new());
    assert_eq!(view.selection_tags, [4], "the tag both selected items have");
    let counts: Vec<u32> = app.view(&model).tags.iter().map(|tag| tag.count).collect();
    assert_eq!(counts, [1, 0, 0, 2, 0, 0, 0]);

    // Every selected item has it: toggled, it is taken. One lacks it: given.
    let mut cmd = app.update(Event::ToggleTag { tab, tag: 4 }, &mut model);
    assert!(matches!(
        tag_ops(&mut cmd).pop().expect("told").operation,
        TagOp::Set {
            on: false,
            tag: 4,
            ..
        }
    ));
    let mut cmd = app.update(Event::ToggleTag { tab, tag: 1 }, &mut model);
    assert!(matches!(
        tag_ops(&mut cmd).pop().expect("told").operation,
        TagOp::Set {
            on: true,
            tag: 1,
            ..
        }
    ));
    let mut cmd = app.update(
        Event::SetTag {
            tab,
            tag: 1,
            on: false,
        },
        &mut model,
    );
    assert!(matches!(
        tag_ops(&mut cmd).pop().expect("told").operation,
        TagOp::Set {
            on: false,
            tag: 1,
            ..
        }
    ));
    let mut cmd = app.update(Event::ClearTags { tab }, &mut model);
    assert_eq!(
        tag_ops(&mut cmd).pop().expect("told").operation,
        TagOp::Clear { paths: vec![a, b] }
    );
    // A tag there is not is given to nothing.
    let mut cmd = app.update(Event::ToggleTag { tab, tag: 99 }, &mut model);
    assert!(tag_ops(&mut cmd).is_empty());
}

#[test]
fn a_tag_s_items_show_in_a_tab_of_their_own_wherever_they_are() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(&app, &mut model, tab, "/x", &[entry("a.txt", false)]);
    let listing_asked = |cmd: &mut Command<Effect, Event>| {
        cmd.effects().find_map(|effect| match effect {
            Effect::ListTagged(request) => Some(request),
            _ => None,
        })
    };
    let mut cmd = app.update(Event::OpenTag { tab, tag: 2 }, &mut model);
    let mut request = listing_asked(&mut cmd).expect("the tag's items are asked for");
    assert_eq!(request.operation.tag, 2);
    let (tabs, active) = tabs_of(&app, &model, 0);
    assert_eq!(
        (tabs.len(), active),
        (2, tabs[1]),
        "a tab beside the folder's"
    );
    let view = shown(&app, &model);
    assert_eq!((view.title.as_str(), view.path.as_str()), ("Orange", ""));
    assert!(view.loading);
    assert!(!view.location.caps.writable);

    let entries = [
        found_file("/p/report.txt", 10, 50),
        found_file("/q/deep/plan.txt", 20, 90),
    ];
    let view_asked = request.operation.view.clone();
    request
        .resolve(TaggedListing {
            output: listing(&entries, view_asked),
            missing: vec!["/gone/old.txt".to_owned()],
        })
        .expect("resolves");
    let event = cmd.events().next().expect("the items come back");
    let _ = app.update(event, &mut model);
    let view = shown(&app, &model);
    assert!(!view.loading && view.show_folder);
    assert_eq!(
        rows_with_folders(&app, &model),
        ["/p/report.txt", "/q/deep/plan.txt"]
    );
    let bar = view.results.expect("a results tab");
    assert_eq!(bar.summary, "2 items. 1 more is no longer where it was");
    assert_eq!(
        bar.tag,
        Some(TagPlaceView {
            id: 2,
            name: "Orange".to_owned(),
            color: TagColor::Orange,
            missing: 1,
        })
    );
    assert_eq!((bar.running, bar.sets, bar.compare), (false, 0, None));

    // The sidebar lists the tags, the one shown marked.
    let section = |model: &Model| {
        app.view(model).windows[0]
            .sidebar
            .iter()
            .find(|section| section.section == SidebarSection::Tags)
            .cloned()
    };
    let listed = section(&model).expect("the tags are listed");
    assert_eq!(listed.items.len(), 7);
    let orange = &listed.items[1];
    assert_eq!(
        (
            orange.title.as_str(),
            orange.tag,
            orange.active,
            orange.icon
        ),
        ("Orange", Some(2), true, SidebarIcon::Tag(TagColor::Orange))
    );
    assert!(!listed.items[0].active);

    // Asked for again, the tab that shows it is shown; nothing new opens.
    let _ = app.update(Event::ActivateTab { tab }, &mut model);
    let _ = app.update(Event::OpenTag { tab, tag: 2 }, &mut model);
    let (tabs, active) = tabs_of(&app, &model, 0);
    assert_eq!((tabs.len(), active), (2, tabs[1]));
    // The items that are gone lose the tag when told to.
    let shown_tab = active;
    let mut cmd = app.update(Event::RemoveMissingTagged { tab: shown_tab }, &mut model);
    assert_eq!(
        tag_ops(&mut cmd).pop().expect("told").operation,
        TagOp::Clear {
            paths: vec!["/gone/old.txt".to_owned()]
        }
    );
    // Whenever tags change, the tab lists its items anew.
    let mut cmd = tagged_now(&app, &mut model, &[("/p/report.txt", &[2])]);
    assert!(listing_asked(&mut cmd).is_some());
    // A tab without a folder is not kept; the setting hides the section.
    assert_eq!(DirFleet::persisted(&model).windows[0].tabs, ["/x"]);
    model.settings.show_tags = false;
    assert_eq!(section(&model), None);
}

#[test]
fn tags_are_made_renamed_recoloured_moved_deleted_and_kept() {
    let app = DirFleet;
    let mut model = Model::default();
    let draft = |model: &Model| app.view(model).tag_draft;
    let typed = |model: &mut Model, name: &str, color| {
        let _ = app.update(
            Event::SetTagDraft {
                name: name.to_owned(),
                color,
            },
            model,
        );
        draft(model).expect("the editor is open").problem
    };
    let _ = app.update(Event::EditTag { tag: None }, &mut model);
    let new = draft(&model).expect("the editor is open");
    assert_eq!((new.id, new.name.as_str()), (None, ""));
    assert_eq!(new.problem, "A tag needs a name.");
    assert_eq!(
        typed(&mut model, "red", TagColor::Blue),
        "Another tag has this name."
    );
    assert_eq!(
        typed(&mut model, "two words", TagColor::Blue),
        "A tag's name is one word."
    );
    // Not kept while something is wrong with it.
    let mut cmd = app.update(Event::ApplyTagDraft, &mut model);
    assert!(tag_ops(&mut cmd).is_empty());
    assert!(draft(&model).is_some());
    assert_eq!(app.view(&model).tags.len(), 7);

    assert_eq!(typed(&mut model, "Work", TagColor::Blue), "");
    let mut cmd = app.update(Event::ApplyTagDraft, &mut model);
    assert_eq!(draft(&model), None);
    let made = app.view(&model).tags.last().cloned().expect("a tag");
    assert_eq!(
        (made.id, made.name.as_str(), made.color),
        (8, "Work", TagColor::Blue)
    );
    // The effects hear the names, to answer searches by them.
    let TagOp::Names(names) = tag_ops(&mut cmd).pop().expect("told").operation else {
        panic!("the names are told");
    };
    assert_eq!(names.last(), Some(&(8, "Work".to_owned())));

    // Changed: its own name is no clash.
    let _ = app.update(Event::EditTag { tag: Some(8) }, &mut model);
    assert_eq!(
        draft(&model).map(|draft| draft.name),
        Some("Work".to_owned())
    );
    assert_eq!(typed(&mut model, "work", TagColor::Green), "");
    let _ = app.update(Event::ApplyTagDraft, &mut model);
    let changed = app.view(&model).tags.last().cloned().expect("a tag");
    assert_eq!(
        (changed.name.as_str(), changed.color),
        ("work", TagColor::Green)
    );
    // Cancelled, nothing changes.
    let _ = app.update(Event::EditTag { tag: Some(1) }, &mut model);
    let _ = typed(&mut model, "Crimson", TagColor::Red);
    let _ = app.update(Event::CancelTagDraft, &mut model);
    assert_eq!(app.view(&model).tags[0].name, "Red");

    // Moved to the front, and kept with the app's state.
    let _ = app.update(Event::MoveTag { tag: 8, to: 0 }, &mut model);
    let ids =
        |model: &Model| -> Vec<u32> { app.view(model).tags.iter().map(|tag| tag.id).collect() };
    assert_eq!(ids(&model), [8, 1, 2, 3, 4, 5, 6, 7]);
    let saved = DirFleet::persisted(&model);
    assert_eq!(saved.tags.as_ref().map(Vec::len), Some(8));
    let mut again = Model::default();
    let _ = start(&app, &mut again, None, LoadedState::Found(saved.clone()));
    assert_eq!(app.view(&again).tags, app.view(&model).tags);
    // A state from before there were tags has the ones the app starts with.
    let mut old = Model::default();
    let before_tags = PersistedState {
        tags: None,
        ..saved
    };
    let _ = start(&app, &mut old, None, LoadedState::Found(before_tags));
    assert_eq!(ids(&old), [1, 2, 3, 4, 5, 6, 7]);

    // Deleted: no item has it any more, and its number is not used again.
    let mut cmd = app.update(Event::DeleteTag { tag: 8 }, &mut model);
    let ops: Vec<TagOp> = tag_ops(&mut cmd)
        .into_iter()
        .map(|request| request.operation)
        .collect();
    assert!(ops.contains(&TagOp::Forget { tag: 8 }), "{ops:?}");
    assert!(
        ops.iter()
            .any(|op| matches!(op, TagOp::Names(names) if names.len() == 7))
    );
    assert_eq!(ids(&model), [1, 2, 3, 4, 5, 6, 7]);
    let _ = app.update(Event::EditTag { tag: None }, &mut model);
    let _ = typed(&mut model, "Later", TagColor::Gray);
    let _ = app.update(Event::ApplyTagDraft, &mut model);
    assert_eq!(ids(&model).last(), Some(&9));
}

#[test]
fn tags_follow_what_is_renamed_and_are_searched_for_by_name() {
    let app = DirFleet;
    let mut model = Model::default();
    let tab = window(&app, &mut model);
    open(
        &app,
        &mut model,
        tab,
        "/x",
        &[
            entry("a.mp4", false),
            entry("b.mp4", false),
            entry("c.txt", false),
        ],
    );
    let (a, b) = (in_x("a.mp4"), in_x("b.mp4"));
    let _ = tagged_now(&app, &mut model, &[(&a, &[1, 5]), (&b, &[5])]);

    // Written as any filter, alone or with others; names in any case.
    assert_eq!(
        crate::search::query::parse("tag:Blue *.mp4").chips[0],
        crate::search::Chip::Tag("Blue".to_owned())
    );
    assert_eq!(
        crate::search::query::chip_token(&crate::search::Chip::Tag("Blue".to_owned())),
        "tag:Blue"
    );
    let _ = app.update(Event::OpenSelectPattern { tab }, &mut model);
    let matching = |model: &mut Model, text: &str| {
        let _ = app.update(
            Event::SetSelectPattern {
                tab,
                text: text.to_owned(),
            },
            model,
        );
        shown(&app, model)
            .select_pattern
            .expect("the dialog is open")
            .matches
    };
    assert_eq!(matching(&mut model, "tag:blue"), 2);
    assert_eq!(matching(&mut model, "tag:BLUE tag:red"), 1);
    assert_eq!(matching(&mut model, "tag:blue b"), 1);
    assert_eq!(matching(&mut model, "tag:green"), 0);
    assert_eq!(matching(&mut model, "tag:nosuch"), 0);
    let _ = app.update(Event::CloseSelectPattern { tab }, &mut model);

    // A renamed item keeps its tags: the effects are told its new name.
    click(&app, &mut model, tab, 0, false, false);
    let _ = app.update(Event::StartRename { tab }, &mut model);
    let mut cmd = app.update(
        Event::Rename {
            tab,
            name: "z.mp4".into(),
        },
        &mut model,
    );
    let mut done = finish(
        &app,
        &mut model,
        &mut cmd,
        crate::fileops::Outcome {
            undo: Some(crate::fileops::Undo::Rename {
                from: PathBuf::from("/x/a.mp4"),
                to: PathBuf::from("/x/z.mp4"),
            }),
            ..outcome(0, crate::fileops::Origin::User)
        },
    );
    let pairs = vec![("/x/a.mp4".to_owned(), "/x/z.mp4".to_owned())];
    let ops: Vec<TagOp> = tag_ops(&mut done)
        .into_iter()
        .map(|request| request.operation)
        .collect();
    assert_eq!(ops, [TagOp::Rename(pairs)]);
}
