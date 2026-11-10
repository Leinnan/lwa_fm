//! The Lua runtime of plugins.
//!
//! Finding and loading them (`discovery`), a worker thread per plugin owning
//! its Lua state (`runtime`), jobs on threads of their own (`jobs`), the
//! sandbox and the `dirfleet` API. Lua never runs on the caller's thread.
//!
//! What plugin threads report goes to the sink installed with [`set_sink`]:
//! the egui app's command queue, or the shared core's stream.

pub mod activation;
pub mod api;
pub mod discovery;
mod handler;
pub mod jobs;
pub mod runtime;
pub mod sandbox;

pub use handler::PluginHandler;

use std::path::PathBuf;
use std::sync::{PoisonError, RwLock};

pub use activation::{ActivationExt, activated};
pub use dirfleet_core::plugins::{
    AppRequest, BarSegment, CallContext, Form, InspectorRow, JobEvent, NotifyLevel, PluginCode,
    PluginMsg, Request, RowUpdate, Source, TableSpec, TargetInfo, Values, manifest, permissions,
};

/// Where plugins live: users' plugin folders and plugins' own storage.
/// Without them only the bundled plugins load and storage is off.
#[derive(Debug, Clone, Default)]
pub struct PluginDirs {
    pub plugins: Option<PathBuf>,
    pub data: Option<PathBuf>,
}

static DIRS: RwLock<PluginDirs> = RwLock::new(PluginDirs {
    plugins: None,
    data: None,
});

/// Sets the plugin folders, before plugins load; replaces earlier ones.
pub fn configure(dirs: PluginDirs) {
    *DIRS.write().unwrap_or_else(PoisonError::into_inner) = dirs;
}

pub(crate) fn dirs() -> PluginDirs {
    DIRS.read().unwrap_or_else(PoisonError::into_inner).clone()
}

type Sink = dyn Fn(PluginMsg) + Send + Sync;
static SINK: RwLock<Option<Box<Sink>>> = RwLock::new(None);

/// Where plugin threads' messages go from now on; replaces the previous sink.
pub fn set_sink(sink: impl Fn(PluginMsg) + Send + Sync + 'static) {
    *SINK.write().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(sink));
}

/// Hands a message to the sink; dropped while there is none.
pub fn send(message: PluginMsg) {
    #[cfg(any(test, feature = "test-support"))]
    if DISCARD_MESSAGES.get() {
        return;
    }
    if let Some(sink) = SINK.read().unwrap_or_else(PoisonError::into_inner).as_ref() {
        sink(message);
    }
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    /// Set on a test's thread when it calls Lua directly, with no app to take
    /// the messages: a sink is shared, so another test would get them.
    pub static DISCARD_MESSAGES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
