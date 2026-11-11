//! The way back to the UI thread from any other thread.
//!
//! Reactor's application messages can only be sent from the UI thread, and the
//! core's effect handlers answer from their own. A message-only window on the
//! UI thread bridges the two: [`wake`] posts to it, and its window procedure
//! runs the installed callback there. Wake-ups are coalesced, so a burst of
//! renders costs one message.
//!
//! The same window runs work [`defer`]red on the UI thread: anything with a
//! message loop of its own (a file dialog, a popup menu, a drag) must start
//! from here and never from inside a reactor callback, which it would stall.
#![expect(unsafe_code, reason = "a message-only window and its window procedure")]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, HWND_MESSAGE, PostMessageW, RegisterClassW, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_APP, WNDCLASSW,
};
use windows::core::w;

/// The message-only window, or 0 before [`install`].
static WINDOW: AtomicIsize = AtomicIsize::new(0);
/// A wake-up is posted and not yet delivered.
static PENDING: AtomicBool = AtomicBool::new(false);

thread_local! {
    static ON_WAKE: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
    static DEFERRED: RefCell<VecDeque<Box<dyn FnOnce()>>> = const { RefCell::new(VecDeque::new()) };
}

/// The message that runs deferred work.
const WM_RUN_DEFERRED: u32 = WM_APP + 1;

/// Runs `work` on the UI thread once the current callback has returned.
/// Only for the UI thread; work deferred before [`install`] waits for it.
pub fn defer(work: impl FnOnce() + 'static) {
    DEFERRED.with(|deferred| deferred.borrow_mut().push_back(Box::new(work)));
    let window = WINDOW.load(Ordering::SeqCst);
    if window != 0 {
        // SAFETY: posting to this thread's own window.
        unsafe {
            let _ = PostMessageW(
                Some(HWND(window as *mut core::ffi::c_void)),
                WM_RUN_DEFERRED,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

/// Makes the calling thread the one [`wake`] wakes; `on_wake` runs on it.
/// Returns whether the window could be made.
pub fn install(on_wake: impl Fn() + 'static) -> bool {
    ON_WAKE.with(|slot| *slot.borrow_mut() = Some(Rc::new(on_wake)));
    if WINDOW.load(Ordering::SeqCst) != 0 {
        return true;
    }
    let class_name = w!("DirFleetWake");
    // SAFETY: plain Win32 calls with valid arguments; the class and the window
    // live for the rest of the process, on this thread.
    let window = unsafe {
        let Ok(module) = GetModuleHandleW(None) else {
            return false;
        };
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: module.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        if RegisterClassW(&raw const class) == 0 {
            return false;
        }
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            class_name,
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(module.into()),
            None,
        )
    };
    let Ok(window) = window else {
        return false;
    };
    WINDOW.store(window.0 as isize, Ordering::SeqCst);
    true
}

/// Asks the UI thread to run its callback soon. Safe from any thread; calls
/// made before the callback ran are folded into one.
pub fn wake() {
    let window = WINDOW.load(Ordering::SeqCst);
    if window == 0 || PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    // SAFETY: posting to a window handle is allowed from any thread; a stale
    // handle only makes the call fail.
    let posted = unsafe {
        PostMessageW(
            Some(HWND(window as *mut core::ffi::c_void)),
            WM_APP,
            WPARAM(0),
            LPARAM(0),
        )
    };
    if posted.is_err() {
        PENDING.store(false, Ordering::SeqCst);
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_APP {
        // Cleared first: a wake-up arriving while the callback runs posts again.
        PENDING.store(false, Ordering::SeqCst);
        let on_wake = ON_WAKE.with(|slot| slot.borrow().clone());
        if let Some(on_wake) = on_wake {
            on_wake();
        }
        return LRESULT(0);
    }
    if message == WM_RUN_DEFERRED {
        // One piece per message: work that pumps messages lets the next run.
        let work = DEFERRED.with(|deferred| deferred.borrow_mut().pop_front());
        if let Some(work) = work {
            work();
        }
        return LRESULT(0);
    }
    // SAFETY: forwarding the arguments this procedure was called with.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}
