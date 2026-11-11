//! The application: owns the core and declares one native window for each of
//! the core's windows.
use std::rc::Rc;
use std::time::{Duration, Instant};

use dirfleet_core::app::{Effect, Event, PickFolderOutput};
use windows_reactor::{
    Application, ApplicationContext, ApplicationView, ApplicationViewContext, Callback,
};

use crate::core_link::CoreLink;
use crate::launch::Launch;
use crate::preview::{self, PreviewInput, PreviewWindow};
use crate::store::Store;
use crate::window::{WindowInput, WindowRoot};
use crate::{hwnd, trace, ui_thread};

/// How long the first window may take to arrive from the core.
const FIRST_WINDOW_TIMEOUT: Duration = Duration::from_secs(10);

pub struct DirFleetApp {
    core: CoreLink,
    store: Store,
    /// Windows the core asked for that its view model does not show yet.
    to_show: Vec<u32>,
    /// Sent by a window when it goes away.
    on_closed: Callback<u32>,
    /// The user's home folder.
    home: Rc<str>,
    /// The files Quick Look shows, while its window is open.
    preview: Option<Previewed>,
    /// The Quick Look window is to be shown once it is declared.
    show_preview: bool,
    on_preview_closed: Callback<()>,
}

/// What the Quick Look window shows.
struct Previewed {
    tab: u32,
    paths: Rc<Vec<String>>,
    index: u32,
}

#[derive(Clone, Copy, Debug)]
pub enum AppMessage {
    /// The core rendered, or left an effect for the shell.
    CoreWoke,
    /// The native window of this core window is gone.
    WindowClosed(u32),
    /// The Quick Look window is gone.
    PreviewClosed,
}

impl DirFleetApp {
    /// Takes what the core left since the last call: effects first, then the view model.
    fn absorb(&mut self, context: &ApplicationContext<Self>) {
        for effect in self.core.take_effects() {
            match effect {
                Effect::OpenWindow(request) => self.to_show.push(request.operation.window),
                Effect::QuitApp(_) => {
                    // Windows closing from now on stay saved for the next launch.
                    self.core.send(Event::Quitting);
                    context.exit();
                }
                // A dialog has a message loop of its own, so it opens once this
                // callback has returned.
                Effect::PickFolder(mut request) => {
                    let core = self.core.clone();
                    ui_thread::defer(move || {
                        let picked = dirfleet_winshell::pick_folder(hwnd::active(), None);
                        core.resolve(&mut request, picked_output(picked));
                    });
                }
                Effect::PickApp(mut request) => {
                    let core = self.core.clone();
                    ui_thread::defer(move || {
                        let picked = dirfleet_winshell::pick_program(hwnd::active(), None);
                        core.resolve(&mut request, picked_output(picked));
                    });
                }
                Effect::QuickLook(request) => {
                    let asked = request.operation;
                    // Asked again for what is showing: Quick Look closes.
                    let showing = self
                        .preview
                        .as_ref()
                        .is_some_and(|preview| *preview.paths == asked.paths);
                    if showing || asked.paths.is_empty() {
                        self.preview = None;
                    } else {
                        self.preview = Some(Previewed {
                            tab: asked.tab,
                            paths: Rc::new(asked.paths),
                            index: asked.index,
                        });
                        self.show_preview = true;
                    }
                }
                _ => {}
            }
        }
        if let Some(view) = trace::timed("taking the view model", || self.core.take_view()) {
            trace::timed("splitting the view model", || self.store.update(view));
            trace::note(|| {
                let rows = self
                    .store
                    .windows
                    .first()
                    .and_then(|parts| parts.panes.first())
                    .map(|pane| &pane.rows);
                format!(
                    "view: {} windows, rows {}+{} of {}",
                    self.store.windows.len(),
                    rows.map_or(0, |rows| rows.first_row),
                    rows.map_or(0, |rows| rows.rows.len()),
                    rows.map_or(0, |rows| rows.total_rows),
                )
            });
        }
        // The Quick Look window is declared by the next view; shown after it.
        if std::mem::take(&mut self.show_preview) && self.preview.is_some() {
            context.show_window(preview::WINDOW_KEY);
        }
        // A window can only be shown once the view model declares it.
        let store = &self.store;
        self.to_show.retain(|&id| {
            let declared = store.window(id).is_some();
            if declared {
                context.show_window(id);
            }
            !declared
        });
    }
}

/// What the core is told a picker returned.
fn picked_output(picked: Option<std::path::PathBuf>) -> PickFolderOutput {
    picked.map_or(PickFolderOutput::Cancelled, |path| {
        PickFolderOutput::Picked(path.to_string_lossy().into_owned())
    })
}

impl Application for DirFleetApp {
    type Input = Launch;
    type Message = AppMessage;

    fn create(launch: &Launch, context: &ApplicationContext<Self>) -> Self {
        let core = CoreLink::new(launch.state_file());
        let sender = context.sender();
        ui_thread::install(move || {
            _ = sender.send(AppMessage::CoreWoke);
        });
        let home = directories::UserDirs::new().map_or_else(String::new, |dirs| {
            dirs.home_dir().to_string_lossy().into_owned()
        });
        core.send(Event::Started {
            open: launch.folder.clone(),
            home: home.clone(),
        });
        let mut app = Self {
            core,
            store: Store::default(),
            to_show: Vec::new(),
            on_closed: context.callback(AppMessage::WindowClosed),
            home: home.into(),
            preview: None,
            show_preview: false,
            on_preview_closed: context.callback(|()| AppMessage::PreviewClosed),
        };
        // An application without a window ends, so the first one has to be
        // declared before this returns; the core opens it once state is loaded.
        let deadline = Instant::now() + FIRST_WINDOW_TIMEOUT;
        loop {
            app.absorb(context);
            if !app.store.windows.is_empty() && app.to_show.is_empty() {
                break;
            }
            if Instant::now() > deadline {
                eprintln!("DirFleet: the core opened no window");
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        trace::note(|| "first window declared".to_owned());
        app
    }

    fn update(&mut self, message: AppMessage, context: &ApplicationContext<Self>) {
        match message {
            AppMessage::CoreWoke => self.absorb(context),
            AppMessage::WindowClosed(window) => {
                // Gone from the view model already: the core closed it itself.
                if self.store.window(window).is_none() {
                    return;
                }
                if self.store.windows.len() == 1 {
                    // Closing the last window quits; it comes back next launch.
                    self.core.send(Event::Quitting);
                }
                if self.store.windows.len() == 1 {
                    // Nothing is left to preview for.
                    self.preview = None;
                }
                self.core.send(Event::WindowClosed { window });
                self.absorb(context);
            }
            AppMessage::PreviewClosed => self.preview = None,
        }
    }

    fn view(&self, _: &Launch, _: &ApplicationViewContext<Self>) -> ApplicationView {
        let windows = self
            .store
            .windows
            .iter()
            .fold(ApplicationView::new(), |view, parts| {
                view.window::<WindowRoot>(
                    parts.id(),
                    WindowInput {
                        core: self.core.clone(),
                        chrome: parts.chrome.clone(),
                        panes: parts.panes.clone(),
                        columns: self.store.columns.clone(),
                        shared: self.store.shared.clone(),
                        shortcuts: self.store.shortcuts.clone(),
                        filters: self.store.filters.clone(),
                        home: self.home.clone(),
                        on_closed: self.on_closed.clone(),
                        leads: self
                            .store
                            .windows
                            .first()
                            .is_some_and(|first| first.id() == parts.id()),
                    },
                )
            });
        match &self.preview {
            Some(preview) => windows.window::<PreviewWindow>(
                preview::WINDOW_KEY,
                PreviewInput {
                    core: self.core.clone(),
                    tab: preview.tab,
                    paths: Rc::clone(&preview.paths),
                    index: preview.index,
                    on_closed: self.on_preview_closed.clone(),
                },
            ),
            None => windows,
        }
    }
}
