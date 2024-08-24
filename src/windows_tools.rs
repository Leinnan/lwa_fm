use std::{ffi::OsStr, iter::once, os::windows::ffi::OsStrExt};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{HANDLE, HINSTANCE, HWND, LPARAM, WPARAM},
        UI::{
            Shell::{ShellExecuteExW, SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW},
            WindowsAndMessaging::HMENU,
        },
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
    unsafe {
        match ShellExecuteExW(&mut sei) {
            Ok(_) => {}
            Err(e) => eprintln!("Failed to open properties window: {:?}", e),
        }
    }
}

pub fn open_context_menu(path: impl AsRef<OsStr>) {
    unsafe {
        use std::ffi::CString;
        use windows::{
            core::PCSTR,
            Win32::{
                Foundation::{HWND, POINT},
                UI::{
                    Shell::{
                        ShellExecuteExA, SEE_MASK_INVOKEIDLIST, SEE_MASK_NOCLOSEPROCESS,
                        SHELLEXECUTEINFOA,
                    },
                    WindowsAndMessaging::{
                        GetCursorPos, PostMessageA, TrackPopupMenu, TPM_LEFTALIGN, TPM_RETURNCMD,
                        WM_COMMAND,
                    },
                },
            },
        };
        let mut point: POINT = POINT { x: 0, y: 0 };

        // Get cursor position
        if GetCursorPos(&mut point).is_err() {
            return;
        }
        println!("LOG {:?}", point);

        // Prepare the SHELLEXECUTEINFO structure
        let file_path_cstring = CString::new(OsStr::new(&path).as_encoded_bytes()).unwrap();
        let mut sei = SHELLEXECUTEINFOA {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOA>() as u32,
            fMask: SEE_MASK_INVOKEIDLIST | SEE_MASK_NOCLOSEPROCESS,
            hwnd: HWND(std::ptr::null_mut()),
            lpVerb: PCSTR::null(),
            lpFile: PCSTR(file_path_cstring.as_ptr() as *const u8),
            lpParameters: PCSTR::null(),
            lpDirectory: PCSTR::null(),
            nShow: 1, // SW_SHOWNORMAL
            ..Default::default()
        };
        println!("LOG 2");
        // Show the context menu
        if ShellExecuteExA(&mut sei).is_ok() {
            println!("LOG 3");
            let hmenu = windows::Win32::UI::Shell::SHGetFileInfoA(
                PCSTR(file_path_cstring.as_ptr() as *const u8),
                windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
                0,
                windows::Win32::UI::Shell::SHGFI_EXETYPE,
            );

            println!("LOG HMENU: {}", hmenu);
            if hmenu == 0 {
                println!("LOG 4");
                let cmd_id = TrackPopupMenu(
                    HMENU(std::ptr::null_mut()),
                    TPM_LEFTALIGN | TPM_RETURNCMD,
                    point.x,
                    point.y,
                    0,
                    HWND(std::ptr::null_mut()),
                    None,
                );
                println!("LOG cmd_id: {}", cmd_id.0);

                println!("LOG 5");
                let _ = PostMessageA(
                    HWND(std::ptr::null_mut()),
                    WM_COMMAND,
                    WPARAM(cmd_id.0 as usize),
                    LPARAM(0),
                );
            }
        }
    }
    // unsafe {
    //     use windows::{
    //         core::PCWSTR,
    //         Win32::Foundation::HWND,
    //         // Win32::System::Com::{CoCreateInstance, CoInitializeEx, COINIT_MULTITHREADED},
    //         Win32::UI::Shell::{
    //             IContextMenu, SHCreateItemFromParsingName, CMF_NORMAL, CMINVOKECOMMANDINFO,
    //         },
    //         Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
    //     };
    //     // let bind_context = CreateBindCtx(0).unwrap();
    //     // Convert path to wide string (PCWSTR)

    //     // Create shell item
    //     let context_menu: IContextMenu = SHCreateItemFromParsingName(wide_path, None).unwrap();

    //     // Query the context menu
    //     let hwnd: HMENU = HMENU(std::ptr::null_mut()); // Replace with a valid window handle if available
    //     let _ = context_menu
    //         .QueryContextMenu(hwnd, 0, 1, 0x7FFF, CMF_NORMAL)
    //         .unwrap();

    //     // Invoke a command (example: first item in the context menu)
    //     let invoke_command_info = CMINVOKECOMMANDINFO {
    //         cbSize: std::mem::size_of::<CMINVOKECOMMANDINFO>() as u32,
    //         fMask: 0,
    //         hwnd: HWND(hwnd.0),
    //         lpVerb: windows::core::PCSTR::null(),
    //         lpParameters: windows::core::PCSTR::null(),
    //         lpDirectory: windows::core::PCSTR::null(),
    //         nShow: SW_SHOWNORMAL.0 as i32,
    //         dwHotKey: 0,
    //         hIcon: HANDLE::default(),
    //     };
    //     context_menu.InvokeCommand(&invoke_command_info).unwrap();
    // }
}
