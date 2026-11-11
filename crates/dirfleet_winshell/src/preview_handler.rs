//! Showing a file with the preview handler registered for its kind: what
//! Explorer's preview pane shows for a PDF, a document, a video.
//!
//! The handler draws into a child window of its own, put over the owner's
//! content, and usually runs in another process (`prevhost.exe`). Every call
//! to it may let that process answer while this thread waits, so a host is
//! made, placed and dropped from a place where the thread's messages may run.
#![allow(unsafe_code)]

use std::path::Path;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, CLSIDFromString, CoCreateInstance, IBindCtx,
    IStream, STGM_READ, STGM_SHARE_DENY_NONE,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::PropertiesSystem::{IInitializeWithFile, IInitializeWithStream};
use windows::Win32::UI::Shell::{
    ASSOCF_INIT_DEFAULTTOSTAR, ASSOCSTR_SHELLEXTENSION, AssocQueryStringW, IInitializeWithItem,
    IPreviewHandler, IShellItem, SHCreateItemFromParsingName, SHCreateStreamOnFileEx,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetClientRect, HWND_TOP, SWP_NOACTIVATE, SetWindowPos,
    WINDOW_EX_STYLE, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_VISIBLE,
};
use windows::core::{GUID, Interface as _, PCWSTR, PWSTR, w};

use crate::com::Com;
use crate::raw::{hwnd, wide};

/// The kind of shell extension a preview handler is, as the registry names it.
const PREVIEW_HANDLER: PCWSTR = w!("{8895b1c6-b41f-4c1c-a562-0d564250836f}");

/// The space left free around the preview, inside the owner's client area,
/// in the owner's own units (96 to the inch).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Insets {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

/// The handler registered for files named like `path`, if there is one.
fn handler_of(path: &Path) -> Option<GUID> {
    let extension = format!(".{}", path.extension()?.to_string_lossy());
    let extension = wide(extension);
    let mut class = [0_u16; 64];
    let mut length = class.len() as u32;
    // SAFETY: the strings are NUL-terminated and outlive the call; `class`
    // is writable for `length` characters.
    unsafe {
        AssocQueryStringW(
            ASSOCF_INIT_DEFAULTTOSTAR,
            ASSOCSTR_SHELLEXTENSION,
            PCWSTR(extension.as_ptr()),
            PREVIEW_HANDLER,
            Some(PWSTR(class.as_mut_ptr())),
            &raw mut length,
        )
    }
    .ok()
    .ok()?;
    // SAFETY: the call above left a NUL-terminated string in `class`.
    unsafe { CLSIDFromString(PCWSTR(class.as_ptr())) }.ok()
}

/// Whether a preview handler is registered for files named like `path`.
#[must_use]
pub fn has_preview_handler(path: &Path) -> bool {
    handler_of(path).is_some()
}

/// Gives the handler the file, the way it takes one: as a stream, an item or
/// a path. Most take a stream, which also keeps them away from the file.
fn initialize(handler: &IPreviewHandler, path: &Path) -> Option<()> {
    let name = wide(path);
    let name = PCWSTR(name.as_ptr());
    let mode = (STGM_READ | STGM_SHARE_DENY_NONE).0;
    if let Ok(with_stream) = handler.cast::<IInitializeWithStream>() {
        // SAFETY: `name` is NUL-terminated and outlives the calls.
        let stream = unsafe { SHCreateStreamOnFileEx(name, mode, 0, false, None::<&IStream>) };
        // SAFETY: the stream is live.
        return unsafe { with_stream.Initialize(&stream.ok()?, mode) }.ok();
    }
    if let Ok(with_item) = handler.cast::<IInitializeWithItem>() {
        // SAFETY: `name` is NUL-terminated and outlives the calls.
        let item: IShellItem =
            unsafe { SHCreateItemFromParsingName(name, None::<&IBindCtx>) }.ok()?;
        // SAFETY: the item is live.
        return unsafe { with_item.Initialize(&item, mode) }.ok();
    }
    let with_file = handler.cast::<IInitializeWithFile>().ok()?;
    // SAFETY: `name` is NUL-terminated and outlives the call.
    unsafe { with_file.Initialize(name, mode) }.ok()
}

/// A file shown by its preview handler, in a child window of the owner.
/// Dropping it takes the preview away.
pub struct PreviewHost {
    handler: IPreviewHandler,
    owner: HWND,
    window: HWND,
    insets: Insets,
    /// Last, so the handler is released while COM is still up.
    _com: Com,
}

impl PreviewHost {
    /// Shows `path` inside the window `owner`, leaving `insets` free around
    /// it. `None` when no handler is registered for the file's kind, or the
    /// one that is would not take the file.
    #[must_use]
    pub fn show(owner: isize, path: &Path, insets: Insets) -> Option<Self> {
        let class = handler_of(path)?;
        let com = Com::enter();
        // Handlers are written to run in the shell's helper process; a few
        // only load in the caller's.
        // SAFETY: plain COM activation.
        let handler: IPreviewHandler =
            unsafe { CoCreateInstance(&raw const class, None, CLSCTX_LOCAL_SERVER) }
                .or_else(|_| unsafe {
                    CoCreateInstance(&raw const class, None, CLSCTX_INPROC_SERVER)
                })
                .ok()?;
        initialize(&handler, path)?;
        let owner = hwnd(owner);
        let frame = frame(owner, insets);
        // SAFETY: `owner` is a window of this thread; the class is the
        // system's.
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                PCWSTR::null(),
                WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                frame.left,
                frame.top,
                frame.right - frame.left,
                frame.bottom - frame.top,
                Some(owner),
                None,
                None,
                None,
            )
        }
        .ok()?;
        let host = Self {
            handler,
            owner,
            window,
            insets,
            _com: com,
        };
        // Above the owner's own content, which is a child window too.
        host.place(frame);
        let inside = inside(frame);
        // SAFETY: the window is live and `inside` outlives the calls. A
        // handler that fails to draw leaves the host to be dropped.
        unsafe {
            host.handler
                .SetWindow(host.window, &raw const inside)
                .ok()?;
            host.handler.DoPreview().ok()?;
        }
        Some(host)
    }

    /// Fits the preview to the owner again, after it was resized.
    pub fn fit(&self) {
        let frame = frame(self.owner, self.insets);
        self.place(frame);
        let inside = inside(frame);
        // SAFETY: `inside` outlives the call.
        let _ = unsafe { self.handler.SetRect(&raw const inside) };
    }

    fn place(&self, frame: RECT) {
        // SAFETY: the window is this host's own.
        let _ = unsafe {
            SetWindowPos(
                self.window,
                Some(HWND_TOP),
                frame.left,
                frame.top,
                frame.right - frame.left,
                frame.bottom - frame.top,
                SWP_NOACTIVATE,
            )
        };
    }
}

impl Drop for PreviewHost {
    fn drop(&mut self) {
        // SAFETY: the handler is live; the window may be gone already with
        // its owner, which only makes the call fail.
        unsafe {
            let _ = self.handler.Unload();
            let _ = DestroyWindow(self.window);
        }
    }
}

/// Where the preview goes in `owner`'s client area, in pixels.
fn frame(owner: HWND, insets: Insets) -> RECT {
    let mut client = RECT::default();
    // SAFETY: `client` is writable; a dead owner leaves it empty.
    let _ = unsafe { GetClientRect(owner, &raw mut client) };
    // SAFETY: no preconditions; 0 for a dead owner.
    let dpi = unsafe { GetDpiForWindow(owner) };
    within(client, insets, dpi)
}

/// `client` less `insets`, which are in units of 96 to the inch on a screen
/// of `dpi`. Never smaller than one pixel each way.
fn within(client: RECT, insets: Insets, dpi: u32) -> RECT {
    let scale = if dpi == 0 { 1.0 } else { f64::from(dpi) / 96.0 };
    let pixels = |length: f64| (length * scale).round() as i32;
    let left = client.left + pixels(insets.left);
    let top = client.top + pixels(insets.top);
    RECT {
        left,
        top,
        right: (client.right - pixels(insets.right)).max(left + 1),
        bottom: (client.bottom - pixels(insets.bottom)).max(top + 1),
    }
}

/// The whole of a window placed at `frame`, in its own coordinates.
const fn inside(frame: RECT) -> RECT {
    RECT {
        left: 0,
        top: 0,
        right: frame.right - frame.left,
        bottom: frame.bottom - frame.top,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preview_keeps_clear_of_what_is_around_it() {
        let client = RECT {
            left: 0,
            top: 0,
            right: 900,
            bottom: 640,
        };
        let insets = Insets {
            left: 16.0,
            top: 16.0,
            right: 16.0,
            bottom: 44.0,
        };
        let plain = within(client, insets, 96);
        assert_eq!(
            (plain.left, plain.top, plain.right, plain.bottom),
            (16, 16, 884, 596)
        );
        // On a denser screen the same space takes more pixels.
        let dense = within(client, insets, 144);
        assert_eq!(
            (dense.left, dense.top, dense.right, dense.bottom),
            (24, 24, 876, 574)
        );
    }

    #[test]
    fn a_window_too_small_still_leaves_a_pixel() {
        let tiny = within(
            RECT {
                left: 0,
                top: 0,
                right: 10,
                bottom: 10,
            },
            Insets {
                left: 16.0,
                top: 16.0,
                right: 16.0,
                bottom: 44.0,
            },
            96,
        );
        assert_eq!((tiny.right - tiny.left, tiny.bottom - tiny.top), (1, 1));
    }

    #[test]
    fn a_file_without_an_extension_has_no_handler() {
        assert!(!has_preview_handler(Path::new("C:\\somewhere\\LICENSE")));
    }
}
