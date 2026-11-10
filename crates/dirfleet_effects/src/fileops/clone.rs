//! macOS copy helpers: APFS clones and the metadata (extended attributes,
//! ACLs, flags) a byte copy loses. Without them a copied app bundle fails
//! its code signature check.

use std::{ffi::CString, io, os::unix::ffi::OsStrExt, path::Path};

/// `CLONE_NOFOLLOW` from `<sys/clonefile.h>`: clone a symlink itself.
const CLONE_NOFOLLOW: u32 = 0x0001;

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
}

/// Clones `src` (a file or a whole tree) to the new path `dst`. Fails with
/// `CrossesDevices`/`Unsupported`-style errors off APFS or across volumes,
/// leaving nothing behind.
pub fn clone(src: &Path, dst: &Path) -> io::Result<()> {
    let (src, dst) = (c_path(src)?, c_path(dst)?);
    // SAFETY: both pointers are NUL-terminated strings that outlive the call.
    #[allow(unsafe_code)]
    let status = unsafe { libc::clonefile(src.as_ptr(), dst.as_ptr(), CLONE_NOFOLLOW) };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Copies permissions, flags, ACLs and extended attributes of `src` onto
/// `dst`, which already has its contents.
pub fn copy_metadata(src: &Path, dst: &Path) -> io::Result<()> {
    let (src, dst) = (c_path(src)?, c_path(dst)?);
    // SAFETY: both pointers are NUL-terminated strings that outlive the call;
    // a null state is allowed.
    #[allow(unsafe_code)]
    let status = unsafe {
        libc::copyfile(
            src.as_ptr(),
            dst.as_ptr(),
            std::ptr::null_mut(),
            libc::COPYFILE_METADATA | libc::COPYFILE_NOFOLLOW_SRC,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
