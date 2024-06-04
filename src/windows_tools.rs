use std::{ffi::OsStr, iter::once, os::windows::ffi::OsStrExt};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{HINSTANCE, HWND},
        UI::Shell::{ShellExecuteExW, SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW},
    },
};

pub fn open_in_explorer(path: impl AsRef<OsStr>, is_dir: bool) {
    if is_dir {
        let _ = open::that_detached(path);
    } else {
        std::process::Command::new("explorer.exe")
            .arg("/select,")
            .arg(&path)
            .spawn()
            .unwrap();
    }
}

pub fn open_properties(path: impl AsRef<OsStr>) {
    // Convert the path to a wide string
    let wide_path: Vec<u16> = OsStr::new(&path).encode_wide().chain(once(0)).collect();
    // Set up the SHELLEXECUTEINFOW structure
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST,
        hwnd: HWND(0),
        lpVerb: PCWSTR::from_raw("properties\0".as_ptr() as *const u16),
        lpFile: PCWSTR::from_raw(wide_path.as_ptr()),
        lpParameters: PCWSTR::null(),
        lpDirectory: PCWSTR::null(),
        nShow: 0,
        hInstApp: HINSTANCE(0),
        ..Default::default()
    };

    // Open the properties window
    unsafe {
        if ShellExecuteExW(&mut sei).is_ok() {
            println!("Properties window opened successfully.");
        } else {
            eprintln!("Failed to open properties window.");
        }
    }
}
