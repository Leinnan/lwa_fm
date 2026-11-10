//! The commit the core was built from, for Settings ▸ About.
#![allow(
    clippy::disallowed_types,
    reason = "it runs once at build time; the core itself starts no processes"
)]
fn main() {
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map_or_else(|| "unknown".to_owned(), |hash| hash.trim().to_owned());
    println!("cargo:rustc-env=DIRFLEET_COMMIT={commit}");
}
