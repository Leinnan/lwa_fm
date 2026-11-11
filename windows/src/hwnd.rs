//! The native window behind a reactor window: which one is active, and a
//! call when one becomes so. Reactor reports a window's size and placement
//! but not its activation, so the window is subclassed for that.
#![expect(unsafe_code, reason = "window subclassing")]

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_SHIFT};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, WM_ACTIVATE, WM_NCDESTROY};

/// Any number: the subclass is told apart by its procedure and this.
const SUBCLASS_ID: usize = 0xD1F1;
/// `WA_INACTIVE`: the low word of `WM_ACTIVATE`'s `wParam` when the window is left.
const INACTIVE: usize = 0;

/// The app's window that was active last, or 0 before any was.
static ACTIVE: AtomicIsize = AtomicIsize::new(0);

thread_local! {
    static WATCHERS: RefCell<HashMap<isize, Rc<dyn Fn()>>> = RefCell::new(HashMap::new());
}

/// The app's window that was active last: the owner for dialogs asked for
/// without a window. 0 when there is none.
pub fn active() -> isize {
    ACTIVE.load(Ordering::SeqCst)
}

/// Whether Ctrl and Shift are held, as of the message being handled. For the
/// UI thread, where a drop arrives without saying.
pub fn ctrl_and_shift() -> (bool, bool) {
    // SAFETY: reads this thread's key state; no pointers involved.
    let down = |key| unsafe { GetKeyState(i32::from(key)) } < 0;
    (down(VK_CONTROL.0), down(VK_SHIFT.0))
}

/// Stops the watching when dropped.
pub struct ActivationWatch(isize);

impl Drop for ActivationWatch {
    fn drop(&mut self) {
        WATCHERS.with(|watchers| watchers.borrow_mut().remove(&self.0));
        // SAFETY: removing a subclass that may already be gone only fails.
        unsafe {
            let _ = RemoveWindowSubclass(as_hwnd(self.0), Some(subclass), SUBCLASS_ID);
        }
        let _ = ACTIVE.compare_exchange(self.0, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
}

/// Calls `on_active` on the UI thread each time `window` becomes the active
/// window, and at once when it already is. Must be called on the window's thread.
pub fn watch_activation(window: isize, on_active: impl Fn() + 'static) -> ActivationWatch {
    let on_active: Rc<dyn Fn()> = Rc::new(on_active);
    WATCHERS.with(|watchers| {
        watchers.borrow_mut().insert(window, Rc::clone(&on_active));
    });
    // SAFETY: `window` is a live window of this thread; the procedure lives
    // for the whole process.
    let foreground = unsafe {
        let _ = SetWindowSubclass(as_hwnd(window), Some(subclass), SUBCLASS_ID, 0);
        GetForegroundWindow().0 as isize
    };
    if foreground == window {
        ACTIVE.store(window, Ordering::SeqCst);
        on_active();
    }
    ActivationWatch(window)
}

const fn as_hwnd(window: isize) -> HWND {
    HWND(window as *mut core::ffi::c_void)
}

unsafe extern "system" fn subclass(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    let handle = window.0 as isize;
    if message == WM_ACTIVATE && (wparam.0 & 0xFFFF) != INACTIVE {
        ACTIVE.store(handle, Ordering::SeqCst);
        let watcher = WATCHERS.with(|watchers| watchers.borrow().get(&handle).cloned());
        if let Some(watcher) = watcher {
            watcher();
        }
    } else if message == WM_NCDESTROY {
        WATCHERS.with(|watchers| watchers.borrow_mut().remove(&handle));
    }
    // SAFETY: passing on the message this procedure was called with.
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}
