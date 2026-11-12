//! The file manager's model and logic, shared by every shell (egui, `SwiftUI`).
//!
//! Nothing here draws, touches the file system, starts threads or reads the
//! clock: `clippy.toml` forbids it, and shells or `dirfleet_effects` do that
//! work instead.
// The binary never needed `#[must_use]`; as a library this lint would ask for it everywhere.
#![allow(clippy::must_use_candidate)]
// `Facet` derives generate unsafe methods; serde's derives next to them are fine.
#![allow(clippy::unsafe_derive_deserialize)]

pub mod app;
pub mod columns;
pub mod current_path;
pub mod drives;
pub mod fileops;
pub mod history;
pub mod ids;
pub mod listing;
pub mod location;
pub mod plugins;
pub mod search;
pub mod shortcut;
pub mod thumbnail_timeline;
pub mod view_settings;

pub use current_path::CurrentPath;
pub use ids::{IdAllocator, TabId, WindowId};
pub use location::Location;
pub use view_settings::{Data, DataSource, DisplayType, GroupBy, Sort};
