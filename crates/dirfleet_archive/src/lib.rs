//! Reading and extracting archives, for DirFleet.
//!
//! This is the extraction core of uunpack (<https://github.com/Leinnan/uunpack>,
//! `core/` at commit 5391b51a1811), copied here without its Swift
//! bindings. Changes to how archives are read belong there first; this copy
//! follows it.
//!
//! Open an archive (or any volume of a multi-part set) with [`open_archive`],
//! list its [`Entry`] tree, pick a destination with
//! [`ArchiveHandle::plan_destination`], and run [`ArchiveHandle::extract`] on a
//! background thread with a [`ProgressObserver`] and a [`CancelToken`].
// Linted in uunpack, under its own rules.
#![allow(clippy::all, clippy::pedantic, clippy::nursery)]

mod archive;
mod codecs;
mod destination;
mod error;
mod extract;
mod formats;
mod names;
mod output;
mod platform;
mod source;
mod volumes;

pub use archive::{ArchiveHandle, ArchiveInfo, Entry, open_archive};
pub use destination::DestinationPlan;
pub use error::CoreError;
pub use extract::{CancelToken, ConflictPolicy, ExtractRequest, ExtractSummary, ProgressObserver};

/// File name extensions of the archives and compressed files the core opens.
pub fn supported_extensions() -> Vec<String> {
    formats::detect::EXTENSIONS
        .iter()
        .map(|ext| ext.to_string())
        .collect()
}

/// Whether a file named `name` is an archive, compressed file or archive
/// volume the core opens; the content decides when it is opened.
pub fn is_archive_name(name: String) -> bool {
    volumes::is_archive_name(&name)
}

/// Decoders keep large buffers on the stack, especially in debug builds, and
/// callers such as Swift's concurrency pool run on threads with small stacks.
/// Archive work therefore runs on a scoped thread with a stack of its own.
pub(crate) fn with_large_stack<T: Send>(work: impl FnOnce() -> T + Send) -> T {
    const STACK_SIZE: usize = 64 * 1024 * 1024;
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("uunpack-core".into())
            .stack_size(STACK_SIZE)
            .spawn_scoped(scope, work)
            .expect("spawn archive thread")
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}
