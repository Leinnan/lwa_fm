//! Search: the state, query syntax, matching and saved searches live in
//! `dirfleet_core`.

pub use dirfleet_core::search::*;

pub mod filter;

pub use dirfleet_core::search::saved::{SavedScope, SavedSearch, SavedSearches};
