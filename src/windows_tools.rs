use std::ffi::OsStr;

pub fn open_in_explorer(path: impl AsRef<OsStr>,is_dir : bool) {
    if is_dir {
        let _ = open::that_detached(path);
    }else {
        std::process::Command::new("explorer.exe")
        .arg("/select,")
        .arg(&path)
        .spawn()
        .unwrap();
    }
}