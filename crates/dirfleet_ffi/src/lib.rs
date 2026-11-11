//! The bridge native shells call. Effects with a Rust handler never cross it;
//! only the ones a shell must handle reach it, as bincode bytes.
// The binary never needed `#[must_use]`; as a library this lint would ask for it everywhere.
#![allow(clippy::must_use_candidate)]
// `Facet` derives generate unsafe methods; serde's derives next to them are fine.
#![allow(clippy::unsafe_derive_deserialize)]
// `boltffi::export` generates and uses underscore-prefixed items.
#![allow(clippy::used_underscore_items)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use crux_core::Core;
use crux_core::bridge::{BincodeFfiFormat, EffectId};
use crux_core::effects::{EffectRouter, Routes, routes::Serialized};
use crux_core::macros::effect;
use crux_core::render::RenderOperation;
use dirfleet_core::app::{DirFleet, OpenWindow, PickApp, PickFolder, QuickLook};
use dirfleet_effects::Handlers;

/// The effects a native shell handles: the subset of the app's effects
/// without a Rust handler. Shells are generated from this list, not the app's.
#[effect(facet_typegen)]
pub enum Effect {
    Render(RenderOperation),
    PickFolder(PickFolder),
    OpenWindow(OpenWindow),
    QuickLook(QuickLook),
    PickApp(PickApp),
    QuitApp(dirfleet_core::app::QuitApp),
}

impl Effect {
    /// The shell's part of `effect`; `None` for effects handled in Rust.
    fn for_shell(effect: dirfleet_core::app::Effect) -> Option<Self> {
        use dirfleet_core::app::Effect as App;
        match effect {
            App::Render(request) => Some(Self::Render(request)),
            App::PickFolder(request) => Some(Self::PickFolder(request)),
            App::OpenWindow(request) => Some(Self::OpenWindow(request)),
            App::QuickLook(request) => Some(Self::QuickLook(request)),
            App::PickApp(request) => Some(Self::PickApp(request)),
            App::QuitApp(request) => Some(Self::QuitApp(request)),
            App::ListDir(_)
            | App::Watch(_)
            | App::Unwatch(_)
            | App::LoadState(_)
            | App::SaveState(_)
            | App::OrderListing(_)
            | App::ListTrash(_)
            | App::Trash(_)
            | App::SearchWalk(_)
            | App::StopWalk(_)
            | App::Delay(_)
            | App::UserFolders(_)
            | App::WatchDrives(_)
            | App::Eject(_)
            | App::System(_)
            | App::RunFileOp(_)
            | App::CancelFileOp(_)
            | App::PauseFileOp(_)
            | App::FindConflicts(_)
            | App::Clipboard(_)
            | App::ClipboardChanges(_)
            | App::ChildDirs(_)
            | App::FolderSize(_)
            | App::HashFile(_)
            | App::Materialize(_)
            | App::SetArchivePassword(_)
            | App::Connect(_)
            | App::Disconnect(_)
            | App::ForgetSecret(_)
            | App::StopHash(_)
            | App::ReadSidecar(_)
            | App::Thumbnail(_)
            | App::ThumbnailChoice(_)
            | App::PickerTiming(_)
            | App::PickerFrame(_)
            | App::LoadPlugins(_)
            | App::PluginControl(_)
            | App::PluginMessages(_)
            | App::CheckActivation(_)
            | App::WatchPlugins(_)
            | App::MediaNotices(_)
            | App::Clock(_)
            | App::DiskAccessCheck(_)
            | App::TerminalCheck(_)
            | App::OpenHistory(_) => None,
        }
    }
}

/// Implemented by the shell.
#[boltffi::export]
pub trait CruxShell: Send + Sync {
    /// Effects for the shell, as a bincode-serialized list of requests.
    fn process_effects(&self, bytes: Vec<u8>);
}

#[derive(Clone)]
struct ShellRoutes {
    serialized: Arc<Serialized<DirFleet, Self, BincodeFfiFormat>>,
    /// For the Rust handlers, which are made once the state file is known.
    router: Weak<EffectRouter<DirFleet, Self>>,
}

impl Routes<DirFleet> for ShellRoutes {
    fn new(router: Weak<EffectRouter<DirFleet, Self>>) -> Self {
        Self {
            serialized: Arc::new(Serialized::new(router.clone())),
            router,
        }
    }
}

/// The core as native shells see it.
pub struct CoreFfi {
    router: Arc<EffectRouter<DirFleet, ShellRoutes>>,
    /// A render was sent and the shell hasn't fetched the view since: more
    /// renders until then would only make it fetch the same view again.
    render_pending: Arc<AtomicBool>,
}

impl CoreFfi {
    fn build(shell: Arc<dyn CruxShell>, state_file: Option<PathBuf>) -> Self {
        dirfleet_effects::listing_io::init_locale();
        let render_pending = Arc::new(AtomicBool::new(false));
        let pending = Arc::clone(&render_pending);
        let router = EffectRouter::new(Core::new(), move |routes: ShellRoutes| {
            let handlers = Handlers::new(&routes.router, state_file);
            move |effect| {
                let Some(effect) = handlers.handle(effect).and_then(Effect::for_shell) else {
                    return;
                };
                if matches!(effect, Effect::Render(_)) && pending.swap(true, Ordering::AcqRel) {
                    return;
                }
                let bytes = routes
                    .serialized
                    .serialize(effect)
                    .expect("a shell request encodes");
                shell.process_effects(bytes);
            }
        });
        Self {
            router,
            render_pending,
        }
    }
}

#[boltffi::export]
#[allow(clippy::missing_panics_doc, clippy::needless_pass_by_value)]
impl CoreFfi {
    /// A core that keeps nothing between launches.
    pub fn new(shell: Arc<dyn CruxShell>) -> Self {
        Self::build(shell, None)
    }

    /// A core that keeps its state in `state_file` between launches.
    pub fn with_state_file(shell: Arc<dyn CruxShell>, state_file: String) -> Self {
        Self::build(shell, Some(PathBuf::from(state_file)))
    }

    /// Sends a bincode-serialized `Event`. Effects arrive through the shell.
    pub fn update(&self, data: &[u8]) -> Vec<u8> {
        self.router
            .routes
            .serialized
            .update(data)
            .expect("the event decodes");
        Vec::new()
    }

    /// Answers the shell effect `effect_id` with bincode-serialized output.
    pub fn resolve(&self, effect_id: u32, data: &[u8]) -> Vec<u8> {
        self.router
            .routes
            .serialized
            .resolve(EffectId(effect_id), data)
            .expect("the effect resolves");
        Vec::new()
    }

    /// The bincode-serialized `ViewModel`. Renders after this call reach the
    /// shell again.
    pub fn view(&self) -> Vec<u8> {
        self.render_pending.store(false, Ordering::Release);
        self.router
            .routes
            .serialized
            .view()
            .expect("the view model encodes")
    }
}
