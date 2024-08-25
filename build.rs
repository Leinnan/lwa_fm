extern crate embed_resource;

fn main() -> anyhow::Result<()> {
    let target = std::env::var("TARGET")?;
    if target.contains("windows") {
        embed_resource::compile("static/icon.rc", embed_resource::NONE);
    }
    Ok(())
}
