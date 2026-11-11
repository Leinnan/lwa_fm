//! What the operating systems do differently: download marks, permissions,
//! links, local time and process liveness. Both sides offer the same
//! functions; one that means nothing on a system does nothing there.

#[cfg(not(windows))]
mod macos;
#[cfg(not(windows))]
pub(crate) use macos::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::*;
