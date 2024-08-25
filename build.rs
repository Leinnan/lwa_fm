fn main() -> anyhow::Result<()> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("Failed to run git command");
    let git_hash = String::from_utf8(output.stdout).expect("Failed to get output from git command");
    println!("cargo:rustc-env=GIT_HASH={}", &git_hash[..7]);

    let target = std::env::var("CARGO_CFG_TARGET_OS")?;
    if target == "windows" {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("./static/icon.ico");
        res.compile()?;
    }

    Ok(())
}
