//! Taking part in COM and OLE for the length of a call.
#![allow(unsafe_code)]

use std::marker::PhantomData;
use windows::Win32::System::{
    Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
    Ole::{OleInitialize, OleUninitialize},
};

/// Keeps a guard on the thread that made it: what it did is undone there.
type ThreadBound = PhantomData<*const ()>;

/// COM on this thread for as long as it lives. On a thread that already has
/// it (a UI thread) this only adds a reference; on the multithreaded apartment
/// it does nothing, and the calls that need an apartment of their own fail.
///
/// Declare it before the COM objects it serves, so they are released first.
pub struct Com(bool, ThreadBound);

impl Com {
    pub fn enter() -> Self {
        // SAFETY: no preconditions; undone on drop when it took effect.
        let entered = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        Self(entered, PhantomData)
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the `CoInitializeEx` in `enter`, on its thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// OLE (what drag and drop needs on top of COM) on this thread for as long as
/// it lives. A thread with a window that takes drops already has it.
pub struct Ole(bool, ThreadBound);

impl Ole {
    pub fn enter() -> Self {
        // SAFETY: no preconditions; undone on drop when it took effect.
        let entered = unsafe { OleInitialize(None) }.is_ok();
        Self(entered, PhantomData)
    }
}

impl Drop for Ole {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the `OleInitialize` in `enter`, on its thread.
            unsafe { OleUninitialize() };
        }
    }
}
