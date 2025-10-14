use crate::helper::DetachedSpawn;
use anyhow::Context;
use std::{ffi::OsStr, iter::once, os::windows::ffi::OsStrExt};
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND},
        UI::Shell::{SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW, ShellExecuteExW},
    },
    core::PCWSTR,
};

pub fn display_in_explorer(path: impl AsRef<OsStr>) -> anyhow::Result<()> {
    std::process::Command::new("explorer.exe")
        .arg("/select,")
        .arg(&path)
        .spawn_detached()
        .context("Failed to open explorer")?;
    Ok(())
}

/// Opens the explorer properties for a given path
pub fn open_properties(path: impl AsRef<OsStr>) {
    // Convert the path to a wide string
    let wide_path: Vec<u16> = OsStr::new(&path).encode_wide().chain(once(0)).collect();
    // Convert the verb "properties" to a wide string
    let wide_verb: Vec<u16> = OsStr::new("properties")
        .encode_wide()
        .chain(once(0))
        .collect();
    // Set up the SHELLEXECUTEINFOW structure
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST,
        hwnd: HWND(std::ptr::null_mut()),
        lpVerb: PCWSTR::from_raw(wide_verb.as_ptr()),
        lpFile: PCWSTR::from_raw(wide_path.as_ptr()),
        lpParameters: PCWSTR::null(),
        lpDirectory: PCWSTR::null(),
        nShow: 0,
        hInstApp: HINSTANCE(std::ptr::null_mut()),
        ..Default::default()
    };

    // Open the properties window
    #[allow(unsafe_code)]
    unsafe {
        match ShellExecuteExW(&mut sei) {
            Ok(()) => {}
            Err(e) => eprintln!("Failed to open properties window: {e:?}"),
        }
    }
}
