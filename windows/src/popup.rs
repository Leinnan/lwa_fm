//! Shows a menu at the pointer as a native popup menu.
//!
//! Reactor can only attach a menu to a button, so a menu opened by a right
//! click is a Win32 one. It runs a message loop of its own until it closes:
//! call [`show`] from [`crate::ui_thread::defer`], never from a reactor callback.
#![expect(unsafe_code, reason = "Win32 popup menus")]

use std::sync::atomic::{AtomicI32, Ordering};

use dirfleet_core::app::Appearance;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, HMENU, MF_CHECKED, MF_GRAYED,
    MF_POPUP, MF_SEPARATOR, MF_STRING, SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenuEx,
};
use windows::core::{HSTRING, PCSTR, PCWSTR, w};

use crate::menus::{Action, Entry};

/// `uxtheme.dll` exports without a name: `SetPreferredAppMode` and
/// `FlushMenuThemes`.
const SET_PREFERRED_APP_MODE: usize = 135;
const FLUSH_MENU_THEMES: usize = 136;

/// The mode last set, so it is set again only when the appearance changes.
static MODE: AtomicI32 = AtomicI32::new(-1);

/// What `SetPreferredAppMode` is told for an appearance: follow the system's
/// setting, or be dark or light whatever it says.
const fn app_mode(appearance: Appearance) -> i32 {
    match appearance {
        Appearance::System => 1,
        Appearance::Dark => 2,
        Appearance::Light => 3,
    }
}

/// Makes the process's popup menus, Explorer's among them, look as the app
/// does. Cheap to call again with the same appearance.
///
/// Win32 menus follow a mode set through `uxtheme.dll` exports that have no
/// documented name; they have kept their ordinals since Windows 10 1903.
/// Where they are missing the menus stay light.
pub fn follow(appearance: Appearance) {
    type SetPreferredAppMode = unsafe extern "system" fn(i32) -> i32;
    type FlushMenuThemes = unsafe extern "system" fn();

    let mode = app_mode(appearance);
    if MODE.swap(mode, Ordering::Relaxed) == mode {
        return;
    }
    // SAFETY: loads a system library by name; it stays loaded.
    let Ok(module) = (unsafe { LoadLibraryW(w!("uxtheme.dll")) }) else {
        return;
    };
    // SAFETY: an ordinal is a valid name for `GetProcAddress`: a value below
    // 0x10000 in the pointer.
    let export = |ordinal: usize| unsafe { GetProcAddress(module, PCSTR(ordinal as *const u8)) };
    let (Some(set_mode), Some(flush)) = (export(SET_PREFERRED_APP_MODE), export(FLUSH_MENU_THEMES))
    else {
        return;
    };
    // SAFETY: these are the exports' signatures on every Windows that has them.
    unsafe {
        let set_mode = std::mem::transmute::<
            unsafe extern "system" fn() -> isize,
            SetPreferredAppMode,
        >(set_mode);
        let flush =
            std::mem::transmute::<unsafe extern "system" fn() -> isize, FlushMenuThemes>(flush);
        set_mode(mode);
        flush();
    }
}

/// A label as a menu takes it: a lone `&` would underline the next letter.
fn menu_label(label: &str) -> HSTRING {
    HSTRING::from(label.replace('&', "&&"))
}

/// Shows `entries` at the pointer, owned by `owner`, and returns what was
/// chosen; `None` when the menu was dismissed.
pub fn show(owner: isize, entries: &[Entry]) -> Option<Action> {
    let mut actions = Vec::new();
    // SAFETY: the menu handles made here are destroyed here; `owner` is a
    // live window of this thread.
    unsafe {
        let menu = build(entries, &mut actions)?;
        let mut at = POINT::default();
        let _ = GetCursorPos(&raw mut at);
        let owner = HWND(owner as *mut core::ffi::c_void);
        // Without this the menu would not close on a click elsewhere.
        let _ = SetForegroundWindow(owner);
        let chosen = TrackPopupMenuEx(
            menu,
            (TPM_RETURNCMD | TPM_RIGHTBUTTON).0,
            at.x,
            at.y,
            owner,
            None,
        );
        let _ = DestroyMenu(menu);
        // Ids count from 1: 0 says nothing was chosen.
        let index = usize::try_from(chosen.0).ok()?.checked_sub(1)?;
        actions.into_iter().nth(index)
    }
}

/// Builds the menu, collecting each item's action; an item's id is its
/// action's position plus one.
///
/// # Safety
/// The returned menu must be destroyed with `DestroyMenu`.
unsafe fn build(entries: &[Entry], actions: &mut Vec<Action>) -> Option<HMENU> {
    // SAFETY: plain menu calls on a menu this function owns until it returns it.
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        for entry in entries {
            let appended = match entry {
                Entry::Separator => AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()),
                Entry::Item {
                    label,
                    action,
                    enabled,
                    checked,
                } => {
                    actions.push(action.clone());
                    let mut flags = MF_STRING;
                    if !enabled {
                        flags |= MF_GRAYED;
                    }
                    if *checked {
                        flags |= MF_CHECKED;
                    }
                    AppendMenuW(menu, flags, actions.len(), &menu_label(label))
                }
                // The parent owns a submenu once appended, and destroys it.
                Entry::Submenu { label, entries } => build(entries, actions)
                    .map_or(Ok(()), |submenu| {
                        AppendMenuW(menu, MF_POPUP, submenu.0 as usize, &menu_label(label))
                    }),
            };
            if appended.is_err() {
                let _ = DestroyMenu(menu);
                return None;
            }
        }
        Some(menu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ampersand_in_a_name_is_shown_and_not_taken_for_a_shortcut() {
        assert_eq!(menu_label("Tom & Jerry").to_string(), "Tom && Jerry");
        assert_eq!(menu_label("Copy	Ctrl+C").to_string(), "Copy	Ctrl+C");
    }

    #[test]
    fn menus_follow_the_system_unless_the_app_is_set_dark_or_light() {
        assert_eq!(app_mode(Appearance::System), 1);
        assert_eq!(app_mode(Appearance::Dark), 2);
        assert_eq!(app_mode(Appearance::Light), 3);
    }
}
