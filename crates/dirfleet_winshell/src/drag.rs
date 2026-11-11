//! Dragging files out of a window into other applications, e.g. a video onto
//! a player, with the shell's own data object and drag loop, the same
//! Explorer uses.
#![allow(unsafe_code)]

use crate::{
    com::{Com, Ole},
    error::{Error, ShellResult as _},
    raw::{ItemList, optional_hwnd},
};
use std::{ffi::c_void, path::PathBuf};
use windows::{
    Win32::{
        System::{
            Com::{IBindCtx, IDataObject},
            Ole::{DROPEFFECT_COPY, DROPEFFECT_LINK, IDropSource},
        },
        UI::Shell::{BHID_DataObject, SHCreateShellItemArrayFromIDLists, SHDoDragDrop},
    },
    core::Interface as _,
};

/// What came of a drag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DragOutcome {
    /// Dropped on something that copied the files.
    Copied,
    /// Dropped on something that made links to the files.
    Linked,
    /// Cancelled, or dropped where nothing takes files.
    NotTaken,
    /// The drag could not start, and why.
    Failed(String),
}

/// The shell's data object for a set of files: what Explorer puts on a drag
/// or on the clipboard. It stays on the thread that made it.
pub struct DataObject {
    object: IDataObject,
    /// The object is built from these; kept for as long as it is.
    _items: ItemList,
    /// Last, so the object above is released while COM is still up.
    _com: Com,
}

impl DataObject {
    /// The `IDataObject` pointer, for a shell on another windows-rs revision
    /// to wrap in its own type. Borrowed: it is good while `self` lives, and
    /// whoever keeps it longer adds a reference of their own.
    #[must_use]
    pub fn as_raw(&self) -> *mut c_void {
        self.object.as_raw()
    }

    /// Drags the files from the window `owner`, offering copy and link (never
    /// move, so the target can't take the files away). Returns when the drop
    /// is done or the drag is cancelled.
    ///
    /// The drag loop swallows the mouse button's release: a window that
    /// tracks the button itself has to be told it is up.
    #[must_use]
    pub fn drag(&self, owner: isize) -> DragOutcome {
        let _ole = Ole::enter();
        // SAFETY: the data object is live. With no drop source the shell
        // supplies its own, which also draws the drag image.
        let effect = unsafe {
            SHDoDragDrop(
                optional_hwnd(owner),
                &self.object,
                None::<&IDropSource>,
                DROPEFFECT_COPY | DROPEFFECT_LINK,
            )
        };
        match effect {
            Ok(effect) if effect.contains(DROPEFFECT_COPY) => DragOutcome::Copied,
            Ok(effect) if effect.contains(DROPEFFECT_LINK) => DragOutcome::Linked,
            Ok(_) => DragOutcome::NotTaken,
            Err(error) => DragOutcome::Failed(error.to_string()),
        }
    }
}

/// The shell's data object for `paths`. The ones that no longer exist are
/// left out.
///
/// # Errors
/// `paths` is empty, none of them exists any more, or the shell could not
/// build the object.
pub fn data_object(paths: &[PathBuf]) -> Result<DataObject, Error> {
    if paths.is_empty() {
        return Err(Error::new("Nothing to drag"));
    }
    let com = Com::enter();
    let items = ItemList::existing(paths);
    if items.0.is_empty() {
        return Err(Error::new("The files no longer exist"));
    }
    // SAFETY: the item ID lists are valid for as long as `items` lives.
    let object = unsafe {
        SHCreateShellItemArrayFromIDLists(&items.pointers()).and_then(|array| {
            array.BindToHandler::<_, IDataObject>(None::<&IBindCtx>, &BHID_DataObject)
        })
    }
    .shell()?;
    Ok(DataObject {
        object,
        _items: items,
        _com: com,
    })
}

/// Starts an OS drag of `paths` from the window `owner`, offering copy and
/// link (never move, so the target can't take the files away). Returns when
/// the drop is done.
///
/// The drag loop swallows the mouse button's release: a window that tracks
/// the button itself has to be told it is up.
#[must_use]
pub fn start_drag(owner: isize, paths: &[PathBuf]) -> DragOutcome {
    match data_object(paths) {
        Ok(data) => data.drag(owner),
        Err(error) => DragOutcome::Failed(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::{DragOutcome, data_object, start_drag};
    use std::path::PathBuf;

    #[test]
    fn a_drag_needs_files_that_exist() {
        assert_eq!(
            start_drag(0, &[]),
            DragOutcome::Failed("Nothing to drag".into())
        );
        let missing = std::env::temp_dir().join(format!(
            "dirfleet_winshell_missing_drag_{}.txt",
            std::process::id()
        ));
        assert_eq!(
            start_drag(0, &[missing]),
            DragOutcome::Failed("The files no longer exist".into())
        );
    }

    #[test]
    fn files_that_exist_get_a_data_object() {
        let path: PathBuf = std::env::temp_dir().join(format!(
            "dirfleet_winshell_data_object_{}.txt",
            std::process::id()
        ));
        std::fs::write(&path, "x").expect("write fixture");
        let data = data_object(std::slice::from_ref(&path)).expect("data object for a file");
        assert!(!data.as_raw().is_null());
        drop(data);
        let _ = std::fs::remove_file(path);
    }
}
