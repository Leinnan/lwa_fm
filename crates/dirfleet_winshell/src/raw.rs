//! The plain values this crate's API takes, as the Win32 calls want them.
#![allow(unsafe_code)]

use crate::error::Error;
use std::{
    ffi::{OsStr, c_void},
    iter::once,
    os::windows::ffi::OsStrExt,
    path::PathBuf,
};
use windows::{
    Win32::{
        Foundation::HWND,
        UI::Shell::{Common::ITEMIDLIST, ILCreateFromPathW, ILFree},
    },
    core::PCWSTR,
};

/// `text` as NUL-terminated UTF-16.
pub fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(once(0)).collect()
}

/// An owner window as it crosses this crate's API; 0 is the null handle.
pub const fn hwnd(owner: isize) -> HWND {
    HWND(owner as *mut c_void)
}

/// As [`hwnd`], for the calls that take "no owner" as `None`.
pub const fn optional_hwnd(owner: isize) -> Option<HWND> {
    if owner == 0 { None } else { Some(hwnd(owner)) }
}

/// Item ID lists for a set of paths, freed on drop.
pub struct ItemList(pub Vec<*mut ITEMIDLIST>);

impl ItemList {
    /// A list for every path.
    ///
    /// # Errors
    /// Names the first path that is gone.
    pub fn all(paths: &[PathBuf]) -> Result<Self, Error> {
        let mut items = Self(Vec::with_capacity(paths.len()));
        for path in paths {
            let item = create(path);
            if item.is_null() {
                return Err(Error::new(format!("{} no longer exists", path.display())));
            }
            items.0.push(item);
        }
        Ok(items)
    }

    /// Lists for the paths that still exist; the others are left out.
    pub fn existing(paths: &[PathBuf]) -> Self {
        Self(
            paths
                .iter()
                .map(create)
                .filter(|item| !item.is_null())
                .collect(),
        )
    }

    /// The lists as the shell's calls take them. They point into `self`.
    pub fn pointers(&self) -> Vec<*const ITEMIDLIST> {
        self.0.iter().map(|item| item.cast_const()).collect()
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

/// The item ID list of `path`, null when there is nothing at it.
fn create(path: &PathBuf) -> *mut ITEMIDLIST {
    let wide = wide(path);
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    unsafe { ILCreateFromPathW(PCWSTR(wide.as_ptr())) }
}
