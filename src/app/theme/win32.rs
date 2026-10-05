//! Best-effort native accent and opaque caption styling.
use super::palette::{AccentShades, fluent_dark, fluent_light};
use egui::{Color32, Theme};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::{
    Win32::{
        Foundation::HWND,
        Graphics::Dwm::{
            DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
            DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DWMWINDOWATTRIBUTE,
            DwmSetWindowAttribute,
        },
        System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RegGetValueW},
    },
    core::w,
};

pub fn system_accent() -> Option<AccentShades> {
    let mut buffer = [0_u8; 32];
    let mut len = 32_u32;
    // SAFETY: the constant strings are NUL terminated, and the stack buffer
    // and its exact byte length remain valid for the synchronous call.
    #[allow(unsafe_code)]
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Accent"),
            w!("AccentPalette"),
            RRF_RT_REG_BINARY,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&raw mut len),
        )
    };
    if status.is_err() {
        return None;
    }
    AccentShades::from_accent_palette_bytes(buffer.get(..len as usize)?)
}

#[derive(Default)]
pub struct WindowChrome {
    applied: Option<(isize, Theme)>,
}

impl WindowChrome {
    pub fn sync(&mut self, window: &impl HasWindowHandle, theme: Theme) {
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return;
        };
        let key = (handle.hwnd.get(), theme);
        if self.applied == Some(key) {
            return;
        }
        let hwnd = HWND(handle.hwnd.get() as *mut std::ffi::c_void);
        let p = if theme == Theme::Dark {
            fluent_dark(AccentShades::WINDOWS_DEFAULT)
        } else {
            fluent_light(AccentShades::WINDOWS_DEFAULT)
        };
        set_attribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND.0 as u32);
        set_attribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            u32::from(theme == Theme::Dark),
        );
        set_attribute(hwnd, DWMWA_CAPTION_COLOR, colorref(p.toolbar_bg));
        set_attribute(hwnd, DWMWA_TEXT_COLOR, colorref(p.text));
        // Unsupported attributes are deliberately tolerated. Unavailable handles
        // return above without recording state, so the next frame retries.
        self.applied = Some(key);
    }
}

fn set_attribute(hwnd: HWND, attribute: DWMWINDOWATTRIBUTE, value: u32) {
    // SAFETY: HWND is supplied by the live eframe window; DWM reads exactly
    // four bytes from a local value that outlives the synchronous call.
    #[allow(unsafe_code)]
    let _ = unsafe { DwmSetWindowAttribute(hwnd, attribute, (&raw const value).cast(), 4) };
}

fn colorref(color: Color32) -> u32 {
    u32::from(color.r()) | (u32::from(color.g()) << 8) | (u32::from(color.b()) << 16)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn caption_color_is_bgr() {
        assert_eq!(colorref(Color32::from_rgb(0x12, 0x34, 0x56)), 0x0056_3412);
    }

    #[test]
    fn unavailable_handles_are_retried_without_recording_an_applied_theme() {
        struct Unavailable;
        impl HasWindowHandle for Unavailable {
            fn window_handle(
                &self,
            ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError>
            {
                Err(raw_window_handle::HandleError::Unavailable)
            }
        }
        let mut chrome = WindowChrome::default();
        chrome.sync(&Unavailable, Theme::Dark);
        chrome.sync(&Unavailable, Theme::Light);
        assert_eq!(chrome.applied, None);
    }
}
