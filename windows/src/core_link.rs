//! The shared core (`dirfleet_core`) inside the Windows shell, with its Rust
//! effect handlers, linked directly: events and the view model are plain Rust
//! values, nothing is serialized.
//!
//! Handler threads leave what the shell must do in an inbox and wake the UI
//! thread, which takes a fresh view model only when the core rendered.
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use crux_core::capability::Operation;
use crux_core::effects::{EffectRouter, ResolveSink, Routes};
use crux_core::{Core, Request};
use dirfleet_core::app::{DirFleet, Effect, Event, ViewModel};
use dirfleet_effects::Handlers;

use crate::ui_thread;

type Router = EffectRouter<DirFleet, WinRoutes>;

/// What the core left for the UI thread.
#[derive(Default)]
struct Inbox {
    /// The core rendered since the view model was last taken.
    stale: AtomicBool,
    /// Effects only a shell can carry out: windows, pickers, previews, quitting.
    effects: Mutex<Vec<Effect>>,
}

#[derive(Clone)]
pub struct WinRoutes {
    inbox: Arc<Inbox>,
    router: Weak<Router>,
}

impl Routes<DirFleet> for WinRoutes {
    fn new(router: Weak<Router>) -> Self {
        Self {
            inbox: Arc::default(),
            router,
        }
    }
}

/// The core. Cloning shares it.
#[derive(Clone)]
pub struct CoreLink {
    router: Arc<Router>,
}

impl PartialEq for CoreLink {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.router, &other.router)
    }
}

impl CoreLink {
    /// Starts the core. Without a `state_file` nothing is loaded or saved.
    pub fn new(state_file: Option<PathBuf>) -> Self {
        let router = EffectRouter::new(Core::<DirFleet>::new(), move |routes: WinRoutes| {
            let handlers = Handlers::new(&routes.router, state_file);
            move |effect| {
                match handlers.handle(effect) {
                    None => return,
                    Some(Effect::Render(_)) => routes.inbox.stale.store(true, Ordering::SeqCst),
                    Some(effect) => routes
                        .inbox
                        .effects
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(effect),
                }
                ui_thread::wake();
            }
        });
        Self { router }
    }

    /// Sends `event` to the core; what it changes arrives with a later wake-up.
    pub fn send(&self, event: Event) {
        self.router.update(event);
    }

    /// The new view model, when the core rendered since the last call.
    pub fn take_view(&self) -> Option<ViewModel> {
        self.router
            .routes
            .inbox
            .stale
            .swap(false, Ordering::SeqCst)
            .then(|| self.router.view())
    }

    /// The effects waiting for the shell, oldest first.
    pub fn take_effects(&self) -> Vec<Effect> {
        std::mem::take(
            &mut *self
                .router
                .routes
                .inbox
                .effects
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// Answers a request the shell carried out.
    pub fn resolve<Op: Operation>(&self, request: &mut Request<Op>, output: Op::Output) {
        // An answer the core no longer waits for has nowhere to go.
        let _ = self.router.resolve_request(request, output);
    }
}
