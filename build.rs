fn main() -> anyhow::Result<()> {
    let target = std::env::var("CARGO_CFG_TARGET_OS")?;
    if target == "windows" {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("./static/icon.ico");
        res.compile()?;
    }

    Ok(())
}
