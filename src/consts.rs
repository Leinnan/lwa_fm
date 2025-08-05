pub const VERSION: &str = concat!(" v ", env!("CARGO_PKG_VERSION"), " ");
pub const HOMEPAGE: &str = env!("CARGO_PKG_HOMEPAGE");
#[cfg(debug_assertions)]
pub const GIT_HASH_INFO: &str =
    concat!("DirFleet", ", debug build, build hash: ", env!("GIT_HASH"));
#[cfg(not(debug_assertions))]
pub const GIT_HASH_INFO: &str = concat!("DirFleet", ", build hash: ", env!("GIT_HASH"));
pub const APP_NAME: &str = "DirFleet";
pub const VERTICAL_SPACING: f32 = 8.0;
pub const TOP_SIDE_MARGIN: f32 = 10.0;
