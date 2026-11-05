//! Explorer's own context menu for files and folders, shown on demand from
//! the app's menu. This is the classic menu, with the items shell extensions
//! add (what Windows 11 puts behind "Show more options").
//!
//! The menu runs a modal loop on the UI thread, like the shell drag in
//! `native_drag.rs`, so call it between frames: the app doesn't draw until it
//! returns.
#![allow(unsafe_code)]

use std::{cell::RefCell, iter::once, os::windows::ffi::OsStrExt, path::PathBuf};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx},
        UI::{
            Input::KeyboardAndMouse::GetActiveWindow,
            Shell::{
                CMF_CANRENAME, CMF_NORMAL, CMIC_MASK_PTINVOKE, CMINVOKECOMMANDINFO,
                CMINVOKECOMMANDINFOEX, Common::ITEMIDLIST, DefSubclassProc, GCS_VERBW,
                IContextMenu, IContextMenu2, IContextMenu3, ILCreateFromPathW, ILFindLastID,
                ILFree, IShellFolder, RemoveWindowSubclass, SEE_MASK_ASYNCOK, SEE_MASK_UNICODE,
                SHBindToParent, SetWindowSubclass,
            },
            WindowsAndMessaging::{
                CreatePopupMenu, DestroyMenu, GetCursorPos, HMENU, SW_SHOWNORMAL, TPM_RETURNCMD,
                TPM_RIGHTBUTTON, TrackPopupMenuEx, WM_DRAWITEM, WM_INITMENUPOPUP, WM_MEASUREITEM,
                WM_MENUCHAR,
            },
        },
    },
    core::{Interface as _, PCSTR, PCWSTR, PSTR},
};

/// Ids handed to the shell for its items; 0 means the menu was dismissed.
const FIRST_COMMAND: u32 = 1;
const LAST_COMMAND: u32 = 0x7FFF;
/// Names this module's subclass on the owner window.
const SUBCLASS_ID: usize = 0x4C57_414D;

/// What came of showing the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Closed without choosing anything, or the shell ran the chosen item.
    Done,
    /// "Rename" was chosen. The shell can only do that inside its own view,
    /// so the app has to start its own rename.
    Rename,
}

/// Shows the menu for `paths`, all in one folder, at the pointer. Blocks until
/// it closes.
pub fn show(paths: &[PathBuf]) -> anyhow::Result<Outcome> {
    // SAFETY: no preconditions; null when this thread has no active window.
    let owner = unsafe { GetActiveWindow() };
    anyhow::ensure!(!owner.is_invalid(), "No window to show the menu from");
    let (shell_menu, popup) = build(owner, paths)?;

    let mut point = POINT::default();
    // SAFETY: `point` is a valid local.
    unsafe { GetCursorPos(&raw mut point) }?;
    let command = {
        let _forwarding = Forwarding::start(owner, &shell_menu);
        // SAFETY: `popup` and `owner` are live and belong to this thread. With
        // `TPM_RETURNCMD` the result is the chosen id, or 0.
        unsafe {
            TrackPopupMenuEx(
                popup.0,
                (TPM_RETURNCMD | TPM_RIGHTBUTTON).0,
                point.x,
                point.y,
                owner,
                None,
            )
        }
        .0
    };
    let Ok(command) = u32::try_from(command) else {
        return Ok(Outcome::Done);
    };
    if command < FIRST_COMMAND {
        return Ok(Outcome::Done);
    }
    let offset = (command - FIRST_COMMAND) as usize;
    if verb(&shell_menu, offset).is_some_and(|verb| verb.eq_ignore_ascii_case("rename")) {
        return Ok(Outcome::Rename);
    }
    invoke(&shell_menu, owner, offset, point)?;
    Ok(Outcome::Done)
}

/// The shell's menu object for `paths` and the popup it filled.
fn build(owner: HWND, paths: &[PathBuf]) -> anyhow::Result<(IContextMenu, PopupMenu)> {
    // SAFETY: initialising COM on the UI thread again is harmless (the window
    // already did), and a failure surfaces in the calls below.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let items = ItemList::new(paths)?;
    let first = *items
        .0
        .first()
        .ok_or_else(|| anyhow::anyhow!("Nothing selected"))?;
    let children: Vec<*const ITEMIDLIST> = items
        .0
        .iter()
        // SAFETY: each list is valid; the result points inside it, and it
        // outlives every use below.
        .map(|item| unsafe { ILFindLastID(item.cast_const()) }.cast_const())
        .collect();
    // SAFETY: `first` is a valid absolute item ID list; the children stay
    // valid until `items` drops.
    let shell_menu: IContextMenu = unsafe {
        let folder: IShellFolder = SHBindToParent(first.cast_const(), None)?;
        folder.GetUIObjectOf(owner, &children, None)?
    };
    // SAFETY: no preconditions; the handle is owned by `PopupMenu`.
    let popup = PopupMenu(unsafe { CreatePopupMenu() }?);
    // SAFETY: `popup` is a live menu this call fills.
    unsafe {
        shell_menu.QueryContextMenu(
            popup.0,
            0,
            FIRST_COMMAND,
            LAST_COMMAND,
            CMF_NORMAL | CMF_CANRENAME,
        )
    }
    .ok()?;
    Ok((shell_menu, popup))
}

/// The language-independent name of a menu item. Items from shell extensions
/// don't always have one.
fn verb(shell_menu: &IContextMenu, offset: usize) -> Option<String> {
    let mut buffer = [0u16; 64];
    // SAFETY: with `GCS_VERBW` the shell writes a NUL-terminated UTF-16 string
    // of at most `cchmax` characters to the pointer.
    unsafe {
        shell_menu.GetCommandString(
            offset,
            GCS_VERBW,
            None,
            PSTR(buffer.as_mut_ptr().cast()),
            buffer.len() as u32,
        )
    }
    .ok()?;
    let length = buffer.iter().position(|&unit| unit == 0)?;
    Some(String::from_utf16_lossy(&buffer[..length]))
}

/// Runs the chosen item. It may finish in the background, so a long delete
/// doesn't hold up the app.
fn invoke(
    shell_menu: &IContextMenu,
    owner: HWND,
    offset: usize,
    point: POINT,
) -> anyhow::Result<()> {
    let info = CMINVOKECOMMANDINFOEX {
        cbSize: std::mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32,
        fMask: SEE_MASK_UNICODE | SEE_MASK_ASYNCOK | CMIC_MASK_PTINVOKE,
        hwnd: owner,
        // MAKEINTRESOURCE: the item's offset in place of a verb string.
        lpVerb: PCSTR(std::ptr::without_provenance(offset)),
        lpVerbW: PCWSTR(std::ptr::without_provenance(offset)),
        nShow: SW_SHOWNORMAL.0,
        ptInvoke: point,
        ..Default::default()
    };
    // SAFETY: `info` is fully set up and sized by `cbSize`; the shell reads
    // the extended fields through the base pointer, as the API specifies.
    unsafe { shell_menu.InvokeCommand((&raw const info).cast::<CMINVOKECOMMANDINFO>()) }?;
    Ok(())
}

/// Item ID lists for a set of paths, freed on drop.
struct ItemList(Vec<*mut ITEMIDLIST>);

impl ItemList {
    fn new(paths: &[PathBuf]) -> anyhow::Result<Self> {
        let mut items = Self(Vec::with_capacity(paths.len()));
        for path in paths {
            let wide: Vec<u16> = path.as_os_str().encode_wide().chain(once(0)).collect();
            // SAFETY: `wide` is NUL-terminated and outlives the call.
            let item = unsafe { ILCreateFromPathW(PCWSTR(wide.as_ptr())) };
            anyhow::ensure!(!item.is_null(), "{} no longer exists", path.display());
            items.0.push(item);
        }
        Ok(items)
    }
}

impl Drop for ItemList {
    fn drop(&mut self) {
        for item in &self.0 {
            // SAFETY: each list came from `ILCreateFromPathW` and is freed once.
            unsafe { ILFree(Some(item.cast_const())) };
        }
    }
}

struct PopupMenu(HMENU);

impl Drop for PopupMenu {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by `self` and destroyed once.
        let _ = unsafe { DestroyMenu(self.0) };
    }
}

/// The newer interfaces of the shell's menu that take window messages.
#[derive(Clone)]
struct MessageHandlers {
    v3: Option<IContextMenu3>,
    v2: Option<IContextMenu2>,
}

thread_local! {
    /// The menu being shown on this thread, for the window subclass.
    static HANDLERS: RefCell<Option<MessageHandlers>> = const { RefCell::new(None) };
}

/// Passes the owner window's menu messages to the shell while the menu is up.
/// "Send to" and "Open with" fill their submenus on `WM_INITMENUPOPUP`, and
/// extensions draw their items on `WM_DRAWITEM`; without this they come up
/// empty or blank.
struct Forwarding(HWND);

impl Forwarding {
    fn start(owner: HWND, shell_menu: &IContextMenu) -> Self {
        HANDLERS.set(Some(MessageHandlers {
            v3: shell_menu.cast().ok(),
            v2: shell_menu.cast().ok(),
        }));
        // SAFETY: `owner` is a live window of this thread, and the subclass is
        // removed on drop, before the handlers it reads go away.
        let installed =
            unsafe { SetWindowSubclass(owner, Some(forward_menu_messages), SUBCLASS_ID, 0) };
        if !installed.as_bool() {
            log::warn!("Could not forward the context menu's messages");
        }
        Self(owner)
    }
}

impl Drop for Forwarding {
    fn drop(&mut self) {
        // SAFETY: removes what `start` installed, on the same thread.
        let _ = unsafe { RemoveWindowSubclass(self.0, Some(forward_menu_messages), SUBCLASS_ID) };
        HANDLERS.set(None);
    }
}

unsafe extern "system" fn forward_menu_messages(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    _data: usize,
) -> LRESULT {
    if matches!(
        message,
        WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR
    ) && let Some(result) = handle_menu_message(message, wparam, lparam)
    {
        return result;
    }
    // SAFETY: passes on the arguments this subclass was called with.
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}

/// The shell's answer to a menu message, `None` when it doesn't handle it.
fn handle_menu_message(message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    // Cloned: the shell can pump messages while handling one, which re-enters
    // this function.
    let handlers = HANDLERS.with_borrow(Clone::clone)?;
    if let Some(menu) = handlers.v3 {
        let mut result = LRESULT(0);
        // SAFETY: the message's own arguments, and `result` is a valid local.
        unsafe { menu.HandleMenuMsg2(message, wparam, lparam, Some(&raw mut result)) }.ok()?;
        return Some(result);
    }
    // SAFETY: the message's own arguments.
    unsafe { handlers.v2?.HandleMenuMsg(message, wparam, lparam) }.ok()?;
    Some(LRESULT(0))
}

#[cfg(test)]
mod tests {
    use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::GetMenuItemCount};

    /// Exercises the real shell; ignored because the items depend on the host.
    #[test]
    #[ignore = "depends on the Windows shell and its extensions"]
    fn builds_a_menu_with_rename_for_a_file() {
        let path = std::env::temp_dir().join(format!("lwa_context_{}.txt", std::process::id()));
        std::fs::write(&path, "x").expect("write fixture");
        let (shell_menu, popup) =
            super::build(HWND(std::ptr::null_mut()), std::slice::from_ref(&path))
                .expect("shell menu");
        // SAFETY: `popup` is a live menu.
        let count = unsafe { GetMenuItemCount(Some(popup.0)) };
        assert!(count > 0, "{count} items");
        let verbs: Vec<String> = (super::FIRST_COMMAND..super::LAST_COMMAND)
            .take(512)
            .filter_map(|id| super::verb(&shell_menu, (id - super::FIRST_COMMAND) as usize))
            .collect();
        assert!(
            verbs.iter().any(|verb| verb.eq_ignore_ascii_case("rename")),
            "{verbs:?}"
        );
        drop((shell_menu, popup));
        let _ = std::fs::remove_file(path);
    }
}
