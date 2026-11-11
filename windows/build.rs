//! Stages the self-contained Windows App SDK runtime beside the executable
//! and embeds the app icon.

fn main() {
    #[cfg(windows)]
    {
        windows_reactor_setup::as_self_contained();

        println!("cargo:rerun-if-changed=../static/icon.ico");
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("../static/icon.ico");
        if let Err(error) = resource.compile() {
            panic!("embedding the icon failed: {error}");
        }
    }
}
