//! The listing model lives in `dirfleet_core`; reading folders into it, in
//! `dirfleet_effects`.
pub use dirfleet_core::listing::*;
pub use dirfleet_effects::listing_io::{
    PushRaw, dir_entry_of, entry_type_of, entry_type_of_dir, metadata_of, read_listing,
};
