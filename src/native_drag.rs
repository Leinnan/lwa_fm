//! Dragging files out of the window into other applications, e.g. a video
//! onto a player. egui drags only exist inside the app, so when one leaves
//! the window it is handed to the operating system here.
//!
//! The drag starts from the window that has focus: the press that began it
//! activated that window, whichever viewport it belongs to, and eframe only
//! exposes a native handle for the main one.
//!
//! Windows uses the shell's own data object and drag loop, the same Explorer
//! uses; macOS goes through the `drag` crate. Linux (winit has no drag source
//! on X11 or Wayland) is not supported yet.

use std::path::PathBuf;

/// Whether this system can drag files out of the window.
pub const SUPPORTED: bool = cfg!(any(windows, target_os = "macos"));

/// Starts an OS drag of `paths` from the focused window, offering copy and
/// link (never move, so the target can't take the files away). On Windows
/// this returns when the drop is done.
pub fn start(paths: &[PathBuf]) -> anyhow::Result<()> {
    anyhow::ensure!(!paths.is_empty(), "Nothing to drag");
    platform::start(paths)
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod platform {
    use std::{iter::once, os::windows::ffi::OsStrExt, path::PathBuf};
    use windows::{
        Win32::{
            Foundation::{HWND, LPARAM, POINT, RECT, WPARAM},
            Graphics::Gdi::ScreenToClient,
            System::{
                Com::{IBindCtx, IDataObject},
                Ole::{DROPEFFECT_COPY, DROPEFFECT_LINK, IDropSource},
            },
            UI::{
                Input::KeyboardAndMouse::GetActiveWindow,
                Shell::{
                    BHID_DataObject, Common::ITEMIDLIST, ILCreateFromPathW, ILFree,
                    SHCreateShellItemArrayFromIDLists, SHDoDragDrop,
                },
                WindowsAndMessaging::{GetClientRect, GetCursorPos, PostMessageW, WM_LBUTTONUP},
            },
        },
        core::PCWSTR,
    };

    /// `WM_MOUSELEAVE`, which `windows` only exports under `Win32_UI_Controls`.
    const WM_MOUSELEAVE: u32 = 0x02A3;

    pub fn start(paths: &[PathBuf]) -> anyhow::Result<()> {
        // SAFETY: no preconditions; null when this thread has no active window.
        let hwnd = unsafe { GetActiveWindow() };
        anyhow::ensure!(!hwnd.is_invalid(), "No window to drag from");
        let wide: Vec<Vec<u16>> = paths
            .iter()
            .map(|path| path.as_os_str().encode_wide().chain(once(0)).collect())
            .collect();
        let pidls: Vec<*mut ITEMIDLIST> = wide
            .iter()
            // SAFETY: each buffer is NUL terminated and outlives the call.
            .map(|path| unsafe { ILCreateFromPathW(PCWSTR(path.as_ptr())) })
            .filter(|pidl| !pidl.is_null())
            .collect();
        anyhow::ensure!(!pidls.is_empty(), "The files no longer exist");
        let items: Vec<*const ITEMIDLIST> = pidls.iter().map(|pidl| pidl.cast_const()).collect();
        // SAFETY: the item ID lists are valid until freed below. With no drop
        // source the shell supplies its own, which also draws the drag image.
        let result = unsafe {
            SHCreateShellItemArrayFromIDLists(&items)
                .and_then(|array| {
                    array.BindToHandler::<_, IDataObject>(None::<&IBindCtx>, &BHID_DataObject)
                })
                .and_then(|data| {
                    SHDoDragDrop(
                        Some(hwnd),
                        &data,
                        None::<&IDropSource>,
                        DROPEFFECT_COPY | DROPEFFECT_LINK,
                    )
                })
        };
        for pidl in items {
            // SAFETY: each list came from `ILCreateFromPathW` and is freed once.
            unsafe { ILFree(Some(pidl)) };
        }
        release_pointer(hwnd);
        result?;
        Ok(())
    }

    /// Gives the window back the button release that the drag loop
    /// swallowed, so egui doesn't think the button is still held. Posting it
    /// lets winit deliver it to whichever viewport owns `hwnd`.
    fn release_pointer(hwnd: HWND) {
        let mut pos = POINT::default();
        let mut client = RECT::default();
        // SAFETY: `hwnd` is a live window of this thread and the out
        // parameters are valid locals.
        let inside = unsafe {
            if GetCursorPos(&raw mut pos).is_err() || !ScreenToClient(hwnd, &raw mut pos).as_bool()
            {
                return;
            }
            GetClientRect(hwnd, &raw mut client).is_ok()
                && (client.left..client.right).contains(&pos.x)
                && (client.top..client.bottom).contains(&pos.y)
        };
        // MAKELPARAM: client coordinates as two signed 16-bit halves.
        let coords = (i32::from(pos.y as i16) << 16 | i32::from(pos.x as u16)) as isize;
        // SAFETY: posting plain mouse messages to our own window.
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_LBUTTONUP, WPARAM(0), LPARAM(coords));
            if !inside {
                let _ = PostMessageW(Some(hwnd), WM_MOUSELEAVE, WPARAM(0), LPARAM(0));
            }
        }
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod platform {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSView};
    use raw_window_handle::{
        AppKitWindowHandle, HandleError, HasWindowHandle, RawWindowHandle, WindowHandle,
    };
    use std::{path::PathBuf, ptr::NonNull};

    /// A transparent pixel; macOS shows the file icons alongside it.
    const BLANK_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    /// The content view of the key window, as a handle the `drag` crate takes.
    struct KeyView(objc2::rc::Retained<NSView>);

    impl HasWindowHandle for KeyView {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            let view = NonNull::from(&*self.0).cast();
            let raw = RawWindowHandle::AppKit(AppKitWindowHandle::new(view));
            // SAFETY: the view is retained for as long as `self` is borrowed.
            Ok(unsafe { WindowHandle::borrow_raw(raw) })
        }
    }

    pub fn start(paths: &[PathBuf]) -> anyhow::Result<()> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| anyhow::anyhow!("Drags start on the main thread"))?;
        let view = NSApplication::sharedApplication(mtm)
            .keyWindow()
            .and_then(|window| window.contentView())
            .ok_or_else(|| anyhow::anyhow!("No window to drag from"))?;
        drag::start_drag(
            &KeyView(view),
            drag::DragItem::Files(paths.to_vec()),
            drag::Image::Raw(BLANK_PNG.to_vec()),
            |_result, _position| {},
            drag::Options::default(),
        )?;
        Ok(())
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use std::path::PathBuf;

    pub fn start(_paths: &[PathBuf]) -> anyhow::Result<()> {
        anyhow::bail!(
            "Dragging to other apps isn't supported on this system yet. Use Copy (Ctrl+C) and paste."
        )
    }
}
