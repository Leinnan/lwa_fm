//! Explorer's own context menu for files and folders. This is the classic
//! menu, with the items shell extensions add (what Windows 11 puts behind
//! "Show more options").
//!
//! [`show`] puts it up at the pointer in one call. A shell that wants the
//! items inside its own popup menu uses [`ShellMenu`]: fill a menu with them,
//! pass the owner window's menu messages on while it is up, then invoke what
//! was chosen.
//!
//! A menu runs a modal loop on the thread of its owner window, like the shell
//! drag in `drag.rs`: an app that draws on that thread doesn't draw until the
//! menu closes. Everything here belongs on that thread.
#![allow(unsafe_code)]

use crate::{
    com::Com,
    error::{Error, ShellResult as _},
    raw::{ItemList, hwnd},
};
use std::{cell::RefCell, ffi::c_void, marker::PhantomData, path::PathBuf};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        UI::{
            Shell::{
                CMF_CANRENAME, CMF_NORMAL, CMIC_MASK_PTINVOKE, CMINVOKECOMMANDINFO,
                CMINVOKECOMMANDINFOEX, Common::ITEMIDLIST, DefSubclassProc, GCS_VERBW,
                IContextMenu, IContextMenu2, IContextMenu3, ILFindLastID, IShellFolder,
                RemoveWindowSubclass, SEE_MASK_ASYNCOK, SEE_MASK_UNICODE, SHBindToParent,
                SetWindowSubclass,
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

/// Shows the menu for `paths`, all in one folder, at the pointer, owned by
/// the window `owner`. Blocks until it closes.
///
/// # Errors
/// There is no owner window or nothing in `paths`, one of the paths is gone,
/// or the shell could not build or run the menu.
pub fn show(owner: isize, paths: &[PathBuf]) -> Result<Outcome, Error> {
    if owner == 0 {
        return Err(Error::new("No window to show the menu from"));
    }
    let mut shell_menu = ShellMenu::new(owner, paths)?;
    // SAFETY: no preconditions; the handle is owned by `PopupMenu`.
    let popup = PopupMenu(unsafe { CreatePopupMenu() }.shell()?);
    shell_menu.fill(popup.0.0 as isize, FIRST_COMMAND, LAST_COMMAND)?;

    let mut point = POINT::default();
    // SAFETY: `point` is a valid local.
    unsafe { GetCursorPos(&raw mut point) }.shell()?;
    let command = {
        let _forwarding = shell_menu.forward_owner_messages();
        // SAFETY: `popup` and the owner are live and belong to this thread.
        // With `TPM_RETURNCMD` the result is the chosen id, or 0.
        unsafe {
            TrackPopupMenuEx(
                popup.0,
                (TPM_RETURNCMD | TPM_RIGHTBUTTON).0,
                point.x,
                point.y,
                shell_menu.owner,
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
    shell_menu.invoke(command, (point.x, point.y))
}

/// The shell's menu for a set of files, for a popup menu of the caller's.
///
/// [`fill`](Self::fill) the menu with the shell's items, keep
/// [`handle_message`](Self::handle_message) fed (or
/// [`forward_owner_messages`](Self::forward_owner_messages)) while it is up,
/// then [`invoke`](Self::invoke) the chosen id.
///
/// It lives and dies on the thread that made it, which has to be the thread
/// of the owner window.
pub struct ShellMenu {
    menu: IContextMenu,
    handlers: MessageHandlers,
    owner: HWND,
    /// The id of the shell's first item, once a menu has been filled.
    first_id: Option<u32>,
    /// Last, so the interfaces above are released while COM is still up.
    _com: Com,
}

impl ShellMenu {
    /// The shell's menu for `paths`, all in one folder. `owner` is the window
    /// the menu will be shown from; dialogs its items open belong to it.
    ///
    /// # Errors
    /// Nothing is in `paths`, one of them is gone, or the shell has no menu
    /// for them.
    pub fn new(owner: isize, paths: &[PathBuf]) -> Result<Self, Error> {
        let com = Com::enter();
        let owner = hwnd(owner);
        let items = ItemList::all(paths)?;
        let first = *items
            .0
            .first()
            .ok_or_else(|| Error::new("Nothing selected"))?;
        let children: Vec<*const ITEMIDLIST> = items
            .0
            .iter()
            // SAFETY: each list is valid; the result points inside it, and it
            // outlives every use below.
            .map(|item| unsafe { ILFindLastID(item.cast_const()) }.cast_const())
            .collect();
        // SAFETY: `first` is a valid absolute item ID list; the children stay
        // valid until `items` drops, and the menu keeps its own copies.
        let menu: IContextMenu = unsafe {
            let folder: IShellFolder = SHBindToParent(first.cast_const(), None).shell()?;
            folder.GetUIObjectOf(owner, &children, None).shell()?
        };
        Ok(Self {
            handlers: MessageHandlers::of(&menu),
            menu,
            owner,
            first_id: None,
            _com: com,
        })
    }

    /// Puts the shell's items at the top of `menu` (an `HMENU`), with ids
    /// from `first_id` up to `last_id` at most. Returns how many of those ids
    /// the shell took: its items are `first_id..first_id + count`.
    ///
    /// Leave 0 out of the range when the menu is shown with `TPM_RETURNCMD`,
    /// where it means "dismissed".
    ///
    /// # Errors
    /// The shell could not add its items.
    pub fn fill(&mut self, menu: isize, first_id: u32, last_id: u32) -> Result<u32, Error> {
        // SAFETY: the caller's `menu` is a live menu this call fills.
        let result = unsafe {
            self.menu.QueryContextMenu(
                HMENU(menu as *mut c_void),
                0,
                first_id,
                last_id,
                CMF_NORMAL | CMF_CANRENAME,
            )
        };
        result.ok().shell()?;
        self.first_id = Some(first_id);
        // On success the code is the largest id offset used, plus one.
        Ok(u32::from(result.0 as u16))
    }

    /// Gives the shell one of the owner window's messages while the menu is
    /// up. "Send to" and "Open with" fill their submenus on
    /// `WM_INITMENUPOPUP`, and extensions draw their items on `WM_DRAWITEM`
    /// and `WM_MEASUREITEM` and pick one on `WM_MENUCHAR`; without this they
    /// come up empty or blank.
    ///
    /// Returns what the window procedure should return, or `None` when the
    /// message is another one or the shell doesn't handle it: pass it on.
    #[must_use]
    pub fn handle_message(&self, message: u32, wparam: usize, lparam: isize) -> Option<isize> {
        self.handlers
            .handle(message, WPARAM(wparam), LPARAM(lparam))
            .map(|result| result.0)
    }

    /// Subclasses the owner window to do [`handle_message`](Self::handle_message)
    /// for it, until the returned value is dropped. For a caller that doesn't
    /// see its window's messages. One menu per thread at a time.
    pub fn forward_owner_messages(&self) -> Forwarding<'_> {
        Forwarding::start(self.owner, &self.handlers)
    }

    /// Runs the item with the id `id`, chosen from a menu [`fill`](Self::fill)
    /// filled. `point` is where on the screen the menu was opened. It may
    /// finish in the background, so a long delete doesn't hold up the app.
    ///
    /// "Rename" is not run but returned as [`Outcome::Rename`].
    ///
    /// # Errors
    /// `id` is not one of the shell's, or the shell could not run the item.
    pub fn invoke(&self, id: u32, point: (i32, i32)) -> Result<Outcome, Error> {
        let offset = self
            .first_id
            .and_then(|first| id.checked_sub(first))
            .ok_or_else(|| Error::new("Not an item of the Windows context menu"))?
            as usize;
        if self
            .verb(offset)
            .is_some_and(|verb| verb.eq_ignore_ascii_case("rename"))
        {
            return Ok(Outcome::Rename);
        }
        let info = CMINVOKECOMMANDINFOEX {
            cbSize: std::mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32,
            fMask: SEE_MASK_UNICODE | SEE_MASK_ASYNCOK | CMIC_MASK_PTINVOKE,
            hwnd: self.owner,
            // MAKEINTRESOURCE: the item's offset in place of a verb string.
            lpVerb: PCSTR(std::ptr::without_provenance(offset)),
            lpVerbW: PCWSTR(std::ptr::without_provenance(offset)),
            nShow: SW_SHOWNORMAL.0,
            ptInvoke: POINT {
                x: point.0,
                y: point.1,
            },
            ..Default::default()
        };
        // SAFETY: `info` is fully set up and sized by `cbSize`; the shell reads
        // the extended fields through the base pointer, as the API specifies.
        unsafe {
            self.menu
                .InvokeCommand((&raw const info).cast::<CMINVOKECOMMANDINFO>())
        }
        .shell()?;
        Ok(Outcome::Done)
    }

    /// The language-independent name of the item at `offset` from the first
    /// id. Items from shell extensions don't always have one.
    fn verb(&self, offset: usize) -> Option<String> {
        let mut buffer = [0u16; 64];
        // SAFETY: with `GCS_VERBW` the shell writes a NUL-terminated UTF-16
        // string of at most `cchmax` characters to the pointer.
        unsafe {
            self.menu.GetCommandString(
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

impl MessageHandlers {
    fn of(menu: &IContextMenu) -> Self {
        Self {
            v3: menu.cast().ok(),
            v2: menu.cast().ok(),
        }
    }

    /// The shell's answer to a menu message, `None` when it doesn't handle it.
    fn handle(&self, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        if !is_menu_message(message) {
            return None;
        }
        if let Some(menu) = &self.v3 {
            let mut result = LRESULT(0);
            // SAFETY: the message's own arguments, and `result` is a valid local.
            unsafe { menu.HandleMenuMsg2(message, wparam, lparam, Some(&raw mut result)) }.ok()?;
            return Some(result);
        }
        // SAFETY: the message's own arguments.
        unsafe { self.v2.as_ref()?.HandleMenuMsg(message, wparam, lparam) }.ok()?;
        Some(LRESULT(0))
    }
}

/// The messages a window gets for a menu's submenus and owner-drawn items.
const fn is_menu_message(message: u32) -> bool {
    matches!(
        message,
        WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR
    )
}

thread_local! {
    /// The menu being shown on this thread, for the window subclass.
    static HANDLERS: RefCell<Option<MessageHandlers>> = const { RefCell::new(None) };
}

/// Passes the owner window's menu messages to the shell while it lives; see
/// [`ShellMenu::forward_owner_messages`].
#[must_use = "the messages are only passed on until this is dropped"]
pub struct Forwarding<'menu>(HWND, PhantomData<&'menu ShellMenu>);

impl Forwarding<'_> {
    fn start(owner: HWND, handlers: &MessageHandlers) -> Self {
        HANDLERS.set(Some(handlers.clone()));
        // SAFETY: `owner` is a live window of this thread, and the subclass is
        // removed on drop, before the handlers it reads go away.
        let installed =
            unsafe { SetWindowSubclass(owner, Some(forward_menu_messages), SUBCLASS_ID, 0) };
        if !installed.as_bool() {
            log::warn!("Could not forward the context menu's messages");
        }
        Self(owner, PhantomData)
    }
}

impl Drop for Forwarding<'_> {
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
    // Cloned: the shell can pump messages while handling one, which re-enters
    // this function.
    if is_menu_message(message)
        && let Some(handlers) = HANDLERS.with_borrow(Clone::clone)
        && let Some(result) = handlers.handle(message, wparam, lparam)
    {
        return result;
    }
    // SAFETY: passes on the arguments this subclass was called with.
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::{CreatePopupMenu, LAST_COMMAND, PopupMenu, ShellMenu, show};
    use std::path::PathBuf;
    use windows::Win32::UI::WindowsAndMessaging::{GetMenuItemCount, WM_COMMAND};

    fn fixture(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "dirfleet_winshell_{name}_{}.txt",
            std::process::id()
        ));
        std::fs::write(&path, "x").expect("write fixture");
        path
    }

    #[test]
    fn show_needs_an_owner_window() {
        let error = show(0, &[PathBuf::from(r"C:\")]).expect_err("no owner");
        assert_eq!(error.to_string(), "No window to show the menu from");
    }

    #[test]
    fn a_menu_needs_paths_that_exist() {
        let error = ShellMenu::new(0, &[])
            .err()
            .expect("an empty selection has no menu");
        assert_eq!(error.to_string(), "Nothing selected");

        let missing = std::env::temp_dir().join(format!(
            "dirfleet_winshell_missing_{}.txt",
            std::process::id()
        ));
        let error = ShellMenu::new(0, std::slice::from_ref(&missing))
            .err()
            .expect("a missing file has no menu");
        assert_eq!(
            error.to_string(),
            format!("{} no longer exists", missing.display())
        );
    }

    #[test]
    fn only_a_filled_menu_has_ids_to_invoke() {
        let path = fixture("ids");
        let shell_menu =
            ShellMenu::new(0, std::slice::from_ref(&path)).expect("shell menu for a file");
        assert!(shell_menu.invoke(1, (0, 0)).is_err(), "nothing filled yet");
        // Not a menu message: it is the window's to handle.
        assert_eq!(shell_menu.handle_message(WM_COMMAND, 0, 0), None);
        drop(shell_menu);
        let _ = std::fs::remove_file(path);
    }

    /// Exercises the real shell; ignored because the items depend on the host.
    #[test]
    #[ignore = "depends on the Windows shell and its extensions"]
    fn builds_a_menu_with_rename_for_a_file() {
        const FIRST_ID: u32 = 100;
        let path = fixture("context");
        let mut shell_menu =
            ShellMenu::new(0, std::slice::from_ref(&path)).expect("shell menu for a file");
        // SAFETY: no preconditions; the handle is owned by `PopupMenu`.
        let popup = PopupMenu(unsafe { CreatePopupMenu() }.expect("popup menu"));
        let used = shell_menu
            .fill(popup.0.0 as isize, FIRST_ID, LAST_COMMAND)
            .expect("fill the popup");
        // SAFETY: `popup` is a live menu.
        let count = unsafe { GetMenuItemCount(Some(popup.0)) };
        assert!(count > 0, "{count} items");
        assert!(used > 0, "{used} ids");
        let verbs: Vec<String> = (0..512)
            .filter_map(|offset| shell_menu.verb(offset))
            .collect();
        assert!(
            verbs.iter().any(|verb| verb.eq_ignore_ascii_case("rename")),
            "{verbs:?}"
        );
        assert!(
            shell_menu.invoke(FIRST_ID - 1, (0, 0)).is_err(),
            "an id below the shell's range"
        );
        drop((shell_menu, popup));
        let _ = std::fs::remove_file(path);
    }
}
