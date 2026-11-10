//! Effect handlers for the core: the file system, watchers, processes and OS integration.
//!
//! Each handler resolves its requests from its own thread through a
//! [`ResolveSink`], normally the shell's `crux_core::effects::EffectRouter`, so
//! every shell gets the same behavior and only draws.
// The binary never needed `#[must_use]`; as a library this lint would ask for it everywhere.
#![allow(clippy::must_use_candidate)]

pub mod access;
pub mod access_time;
pub mod child_process;
pub mod clipboard;
pub mod drives;
mod file_jobs;
pub mod fileops;
pub mod fs_enum;
pub mod fs_ops;
mod list;
pub mod listing_io;
pub mod media;
pub mod open_history;
pub mod paths;
mod picker;
pub mod places;
#[cfg(feature = "plugins")]
pub mod plugins;
mod searching;
pub mod sort;
mod storage;
pub mod system;
pub mod thumbnail_choices;
mod thumbnails;
pub mod trash_bin;
pub mod usage;
#[cfg(target_os = "macos")]
mod usage_bulk;
pub mod walk;
mod watch;
#[cfg(windows)]
pub mod windows_media;

use std::path::PathBuf;
use std::sync::{Arc, Weak};

use crux_core::effects::ResolveSink;
use dirfleet_core::app::{
    CheckActivation, ChildDirs, ClipboardChanges, ClipboardOp, Clock, Delay, DiskAccessCheck,
    Effect, Eject, FindConflicts, FolderSize, ListDir, ListTrash, LoadPlugins, LoadState,
    MediaNotices, OpenHistoryOp, OrderListing, PickerFrame, PickerTiming, PluginMessages,
    RunFileOp, SearchWalk, SystemOp, TerminalCheck, Thumbnail, ThumbnailChoiceOp, TrashOp,
    UserFolders, Watch, WatchDrives, WatchPlugins,
};

pub use list::list_dir;
pub use storage::{load as load_state, save as save_state};

/// Answers a request on the spot.
type Answer<T> = Arc<dyn Fn(&mut crux_core::Request<T>) + Send + Sync>;
type Threaded<T> = Arc<dyn Fn(crux_core::Request<T>) + Send + Sync>;

/// Every handler that runs in Rust.
pub struct Handlers {
    list: list::ListHandler,
    watch: watch::WatchHandler,
    walk: searching::WalkHandler,
    history: Arc<open_history::OpenHistory>,
    files: file_jobs::FileJobs,
    storage: storage::StorageHandler,
    thumbnails: thumbnails::ThumbnailHandler,
    notices: Arc<dyn Fn(crux_core::Request<MediaNotices>) + Send + Sync>,
    clock: Arc<dyn Fn(crux_core::Request<Clock>) + Send + Sync>,
    answer_access: Answer<DiskAccessCheck>,
    answer_terminal: Answer<TerminalCheck>,
    answer_clipboard: Answer<ClipboardChanges>,
    answer_choices: Answer<ThumbnailChoiceOp>,
    picker_timing: Threaded<PickerTiming>,
    picker_frame: Threaded<PickerFrame>,
    plugins: PluginEffects,
    delay: Arc<dyn Fn(crux_core::Request<Delay>) + Send + Sync>,
    places: Arc<dyn Fn(Effect) + Send + Sync>,
}

impl Handlers {
    /// `state_file` is where the app's state is kept between launches;
    /// without one nothing is loaded or saved. With one, thumbnails are
    /// cached in the user's cache folder under the name of the state file's
    /// folder, and downloaded tools go beside the state file; `FFmpeg` is
    /// then fetched at start when it is missing, unless the
    /// `DIRFLEET_NO_DOWNLOADS` environment variable is set (tests set it).
    pub fn new<R>(sink: &Weak<R>, state_file: Option<PathBuf>) -> Self
    where
        R: ResolveSink<ListDir>
            + ResolveSink<OrderListing>
            + ResolveSink<ListTrash>
            + ResolveSink<TrashOp>
            + ResolveSink<Watch>
            + ResolveSink<SearchWalk>
            + ResolveSink<Delay>
            + ResolveSink<UserFolders>
            + ResolveSink<WatchDrives>
            + ResolveSink<Eject>
            + ResolveSink<SystemOp>
            + ResolveSink<RunFileOp>
            + ResolveSink<FindConflicts>
            + ResolveSink<ClipboardOp>
            + ResolveSink<ChildDirs>
            + ResolveSink<FolderSize>
            + ResolveSink<LoadState>
            + ResolveSink<Thumbnail>
            + ResolveSink<MediaNotices>
            + ResolveSink<Clock>
            + ResolveSink<DiskAccessCheck>
            + ResolveSink<TerminalCheck>
            + ResolveSink<ClipboardChanges>
            + ResolveSink<ThumbnailChoiceOp>
            + ResolveSink<PickerTiming>
            + ResolveSink<PickerFrame>
            + ResolveSink<LoadPlugins>
            + ResolveSink<PluginMessages>
            + ResolveSink<CheckActivation>
            + ResolveSink<WatchPlugins>
            + Send
            + Sync
            + 'static,
    {
        let keeps_files = state_file.is_some();
        configure_media(state_file.as_deref());
        let history = Arc::new(open_history::OpenHistory::load(
            state_file
                .as_ref()
                .map(|file| file.with_file_name("open_history.json")),
        ));
        let choices = Arc::new(thumbnail_choices::ThumbnailChoices::load(
            state_file
                .as_ref()
                .map(|file| file.with_file_name("thumbnail_choices.json")),
        ));
        choices.install();
        configure_plugins(state_file.as_deref());
        Self {
            list: list::ListHandler::new(sink.clone(), Arc::clone(&history)),
            watch: watch::WatchHandler::new(sink.clone()),
            walk: searching::WalkHandler::new(sink.clone(), Arc::clone(&history)),
            history,
            files: file_jobs::FileJobs::new(sink.clone()),
            storage: storage::StorageHandler::new(sink.clone(), state_file),
            thumbnails: thumbnails::ThumbnailHandler::new(sink),
            notices: {
                let sink = sink.clone();
                let downloads = keeps_files && std::env::var_os("DIRFLEET_NO_DOWNLOADS").is_none();
                Arc::new(move |request| thumbnails::notices(request, &sink, downloads))
            },
            clock: {
                let sink = sink.clone();
                Arc::new(move |request| searching::clock(request, sink.clone()))
            },
            // A file check or two, or a counter: answered on the spot.
            answer_access: answer(sink, |_: &DiskAccessCheck| access::check()),
            answer_terminal: answer(sink, |check: &TerminalCheck| {
                access::terminal_found(&check.app)
            }),
            answer_clipboard: answer(sink, |_: &ClipboardChanges| clipboard::sequence_number()),
            // Answered on the spot, so a thumbnail asked for next uses it.
            answer_choices: answer(sink, move |op: &ThumbnailChoiceOp| choices.apply(op)),
            // Reading a video takes a while: each on its own thread.
            picker_timing: on_thread(sink, picker::timing),
            picker_frame: on_thread(sink, picker::frame),
            plugins: PluginEffects::new(sink),
            delay: {
                let sink = sink.clone();
                let timer = searching::Timer::new();
                Arc::new(move |request| timer.delay(request, sink.clone()))
            },
            places: {
                let sink = sink.clone();
                Arc::new(move |effect| match effect {
                    Effect::UserFolders(request) => places::answer_user_folders(request, &sink),
                    Effect::WatchDrives(request) => places::watch_drives(request, sink.clone()),
                    Effect::Eject(request) => places::eject(request, sink.clone()),
                    Effect::System(request) => system::handle(request, sink.clone()),
                    Effect::FindConflicts(request) => {
                        file_jobs::find_conflicts(request, sink.clone());
                    }
                    Effect::Clipboard(request) => file_jobs::clipboard(request, sink.clone()),
                    Effect::FolderSize(mut request) => {
                        let sink = sink.clone();
                        std::thread::spawn(move || {
                            let cancel = std::sync::atomic::AtomicBool::new(false);
                            let total = usage::measure(
                                std::path::Path::new(&request.operation.path),
                                &usage::Options::default(),
                                &cancel,
                            )
                            .ok()
                            .flatten()
                            .map(|found| found.bytes);
                            if let Some(sink) = sink.upgrade() {
                                let _ = sink.resolve_request(&mut request, total);
                            }
                        });
                    }
                    Effect::ChildDirs(mut request) => {
                        let sink = sink.clone();
                        std::thread::spawn(move || {
                            let ChildDirs { path, show_hidden } = &request.operation;
                            let names = listing_io::child_directories(
                                std::path::Path::new(path),
                                *show_hidden,
                            )
                            .map_err(|error| error.to_string());
                            if let Some(sink) = sink.upgrade() {
                                let _ = sink.resolve_request(
                                    &mut request,
                                    dirfleet_core::app::ChildDirsOutput(names),
                                );
                            }
                        });
                    }
                    _ => {}
                })
            },
        }
    }

    /// How many folders are being watched.
    pub fn watch_count(&self) -> usize {
        self.watch.count()
    }

    /// How many searches are reading subfolders.
    pub fn walk_count(&self) -> usize {
        self.walk.count()
    }

    /// Handles `effect`, or gives it back when the shell must (rendering, pickers, windows).
    pub fn handle(&self, effect: Effect) -> Option<Effect> {
        match effect {
            Effect::ListDir(request) => self.list.process(request),
            Effect::OrderListing(request) => self.list.order(request),
            Effect::ListTrash(request) => self.list.list_trash(request),
            Effect::Trash(request) => self.list.trash(request),
            Effect::Watch(request) => self.watch.start(request),
            Effect::Unwatch(request) => self.watch.stop(request.operation.id),
            Effect::SearchWalk(request) => self.walk.start(request),
            Effect::StopWalk(request) => self.walk.stop(request.operation.id),
            Effect::Delay(request) => (self.delay)(request),
            Effect::OpenHistory(request) => {
                let history = Arc::clone(&self.history);
                std::thread::spawn(move || {
                    let paths =
                        |paths: &[String]| paths.iter().map(PathBuf::from).collect::<Vec<_>>();
                    match &request.operation {
                        OpenHistoryOp::Record(list) => history.record(&paths(list)),
                        OpenHistoryOp::Forget(list) => history.forget(&paths(list)),
                        OpenHistoryOp::Rename(pairs) => {
                            let pairs: Vec<_> = pairs
                                .iter()
                                .map(|(from, to)| (PathBuf::from(from), PathBuf::from(to)))
                                .collect();
                            history.rename(&pairs);
                        }
                        OpenHistoryOp::Clear => history.clear(),
                    }
                });
            }
            Effect::RunFileOp(request) => self.files.run(request),
            Effect::CancelFileOp(request) => self.files.cancel(request.operation.id),
            effect @ (Effect::UserFolders(_)
            | Effect::WatchDrives(_)
            | Effect::Eject(_)
            | Effect::System(_)
            | Effect::FindConflicts(_)
            | Effect::Clipboard(_)
            | Effect::ChildDirs(_)
            | Effect::FolderSize(_)) => {
                (self.places)(effect);
            }
            Effect::LoadState(request) => self.storage.load(request),
            Effect::Thumbnail(request) => self.thumbnails.make(request),
            Effect::MediaNotices(request) => (self.notices)(request),
            Effect::Clock(request) => (self.clock)(request),
            Effect::DiskAccessCheck(mut request) => (self.answer_access)(&mut request),
            Effect::TerminalCheck(mut request) => (self.answer_terminal)(&mut request),
            Effect::ClipboardChanges(mut request) => (self.answer_clipboard)(&mut request),
            Effect::ThumbnailChoice(mut request) => (self.answer_choices)(&mut request),
            Effect::PickerTiming(request) => (self.picker_timing)(request),
            Effect::PickerFrame(request) => (self.picker_frame)(request),
            Effect::LoadPlugins(request) => (self.plugins.load)(request),
            Effect::PluginMessages(request) => (self.plugins.messages)(request),
            Effect::CheckActivation(request) => (self.plugins.activation)(request),
            Effect::WatchPlugins(request) => (self.plugins.watch)(request),
            #[cfg(feature = "plugins")]
            Effect::PluginControl(request) => self.plugins.handler.control(&request.operation),
            #[cfg(not(feature = "plugins"))]
            Effect::PluginControl(_) => {}
            Effect::SaveState(request) => self.storage.save(request.operation.state),
            effect @ (Effect::Render(_)
            | Effect::PickFolder(_)
            | Effect::OpenWindow(_)
            | Effect::QuickLook(_)
            | Effect::PickApp(_)
            | Effect::QuitApp(_)) => {
                return Some(effect);
            }
        }
        None
    }
}

/// Answers a request on the spot with `answer`.
fn answer<T, R>(
    sink: &Weak<R>,
    answer: impl Fn(&T) -> T::Output + Send + Sync + 'static,
) -> Answer<T>
where
    T: crux_core::capability::Operation,
    R: ResolveSink<T> + Send + Sync + 'static,
{
    let sink = sink.clone();
    Arc::new(move |request| {
        let output = answer(&request.operation);
        if let Some(sink) = sink.upgrade() {
            let _ = sink.resolve_request(request, output);
        }
    })
}

/// Answers each request on a thread of its own, for work that takes a while.
fn on_thread<T, R>(sink: &Weak<R>, answer: fn(&T) -> T::Output) -> Threaded<T>
where
    T: crux_core::capability::Operation,
    R: ResolveSink<T> + Send + Sync + 'static,
{
    let sink = sink.clone();
    Arc::new(move |mut request| {
        let sink = sink.clone();
        std::thread::spawn(move || {
            let output = answer(&request.operation);
            if let Some(sink) = sink.upgrade() {
                let _ = sink.resolve_request(&mut request, output);
            }
        });
    })
}

/// Thumbnails are cached in the user's cache folder under the name of the
/// state file's folder; downloaded tools go beside the state file.
/// Loading plugins, their workers and their messages; with the `plugins`
/// feature off, there are none.
struct PluginEffects {
    load: Threaded<LoadPlugins>,
    messages: Threaded<PluginMessages>,
    watch: Threaded<WatchPlugins>,
    activation: Threaded<CheckActivation>,
    #[cfg(feature = "plugins")]
    handler: Arc<plugins::PluginHandler>,
}

impl PluginEffects {
    fn new<R>(sink: &Weak<R>) -> Self
    where
        R: ResolveSink<LoadPlugins>
            + ResolveSink<PluginMessages>
            + ResolveSink<CheckActivation>
            + ResolveSink<WatchPlugins>
            + Send
            + Sync
            + 'static,
    {
        #[cfg(feature = "plugins")]
        {
            let handler = Arc::new(plugins::PluginHandler::default());
            let (load_sink, message_sink) = (sink.clone(), sink.clone());
            let (loader, listener) = (Arc::clone(&handler), Arc::clone(&handler));
            Self {
                load: Arc::new(move |request| loader.load(request, load_sink.clone())),
                messages: Arc::new(move |request| {
                    listener.messages(request, message_sink.clone());
                }),
                // It looks at the folder and its parents: a thread of its own.
                activation: on_thread(sink, plugins::activated),
                watch: {
                    let sink = sink.clone();
                    Arc::new(move |request| plugins::PluginHandler::watch(request, sink.clone()))
                },
                handler,
            }
        }
        #[cfg(not(feature = "plugins"))]
        {
            let activation = on_thread(sink, |_: &CheckActivation| {
                dirfleet_core::app::ActivatedPlugins(Vec::new())
            });
            let sink = sink.clone();
            Self {
                load: Arc::new(move |mut request| {
                    if let Some(sink) = sink.upgrade() {
                        let found = dirfleet_core::app::PluginsFound {
                            plugins: Vec::new(),
                            broken: Vec::new(),
                            ffmpeg: None,
                        };
                        let _ = sink.resolve_request(&mut request, found);
                    }
                }),
                messages: Arc::new(|_| {}),
                watch: Arc::new(|_| {}),
                activation,
            }
        }
    }
}

/// The user's plugins are in `plugins` beside the state file, and their
/// storage in `plugin-data`; without a state file only bundled ones load.
#[cfg(feature = "plugins")]
fn configure_plugins(state_file: Option<&std::path::Path>) {
    let folder = state_file.and_then(std::path::Path::parent);
    plugins::configure(plugins::PluginDirs {
        plugins: folder.map(|folder| folder.join("plugins")),
        data: folder.map(|folder| folder.join("plugin-data")),
    });
}

#[cfg(not(feature = "plugins"))]
const fn configure_plugins(_: Option<&std::path::Path>) {}

fn configure_media(state_file: Option<&std::path::Path>) {
    if let Some(folder) = state_file.and_then(std::path::Path::parent) {
        let cache = directories::BaseDirs::new()
            .zip(folder.file_name())
            .map_or_else(
                || folder.join("cache"),
                |(base, name)| base.cache_dir().join(name),
            );
        media::configure(media::MediaDirs {
            cache,
            data: folder.to_path_buf(),
        });
    }
}
