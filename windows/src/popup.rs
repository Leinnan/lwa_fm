//! Shows a menu at the pointer as a native popup menu.
//!
//! Reactor can only attach a menu to a button, so a menu opened by a right
//! click is a Win32 one. It runs a message loop of its own until it closes:
//! call [`show`] from [`crate::ui_thread::defer`], never from a reactor callback.
#![expect(unsafe_code, reason = "Win32 popup menus")]

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, HMENU, MF_CHECKED, MF_GRAYED,
    MF_POPUP, MF_SEPARATOR, MF_STRING, SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenuEx,
};
use windows::core::{HSTRING, PCWSTR};

use crate::menus::{Action, Entry};

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
                    AppendMenuW(menu, flags, actions.len(), &HSTRING::from(label.as_str()))
                }
                // The parent owns a submenu once appended, and destroys it.
                Entry::Submenu { label, entries } => {
                    build(entries, actions).map_or(Ok(()), |submenu| {
                        AppendMenuW(
                            menu,
                            MF_POPUP,
                            submenu.0 as usize,
                            &HSTRING::from(label.as_str()),
                        )
                    })
                }
            };
            if appended.is_err() {
                let _ = DestroyMenu(menu);
                return None;
            }
        }
        Some(menu)
    }
}
