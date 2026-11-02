pub const HOMEPAGE: &str = env!("CARGO_PKG_HOMEPAGE");
#[cfg(debug_assertions)]
pub const GIT_HASH_INFO: &str =
    concat!("DirFleet", ", debug build, build hash: ", env!("GIT_HASH"));
#[cfg(not(debug_assertions))]
pub const GIT_HASH_INFO: &str = concat!("DirFleet", ", build hash: ", env!("GIT_HASH"));
pub const APP_NAME: &str = "DirFleet";
/// Names the app's folders. `static/Info.plist` and `Cargo.toml` repeat it.
pub const APP_ID: &str = "io.github.leinnan.dirfleet";
/// The icon of every window. `just make_win_icon` generates it.
pub const WINDOW_ICON_PNG: &[u8] = include_bytes!("../static/window_icon.png");
