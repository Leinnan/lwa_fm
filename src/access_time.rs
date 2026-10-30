//! Keeps our own reads (thumbnails, `FFmpeg` probes) from counting as the user
//! opening a file, so sorting by last access stays meaningful.
#![cfg_attr(windows, allow(unsafe_code))]

use std::path::Path;

/// Restores the file's last access time when dropped, unless the file was
/// written meanwhile. A no-op where the time cannot be read or set.
pub struct PreserveAccessTime {
    #[cfg(windows)]
    inner: Option<windows_impl::Saved>,
}

impl PreserveAccessTime {
    #[must_use]
    pub fn new(path: &Path) -> Self {
        #[cfg(windows)]
        {
            Self {
                inner: windows_impl::Saved::capture(path),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = path;
            Self {}
        }
    }
}

#[cfg(windows)]
impl Drop for PreserveAccessTime {
    fn drop(&mut self) {
        if let Some(saved) = self.inner.take() {
            saved.restore();
        }
    }
}

#[cfg(windows)]
mod windows_impl {
    use std::{
        fs::File,
        os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
        path::Path,
    };

    use windows::Win32::{
        Foundation::{FILETIME, HANDLE},
        Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, GetFileTime, SetFileTime,
        },
    };

    pub struct Saved {
        file: File,
        accessed: FILETIME,
        written: FILETIME,
    }

    impl Saved {
        pub fn capture(path: &Path) -> Option<Self> {
            // Attribute access only: no data is read, so opening this handle
            // neither bumps the access time nor hydrates cloud placeholders.
            let file = std::fs::OpenOptions::new()
                .access_mode((FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES).0)
                .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
                .open(path)
                .ok()?;
            let (accessed, written) = times(&file)?;
            Some(Self {
                file,
                accessed,
                written,
            })
        }

        pub fn restore(self) {
            // A write in the meantime is a real change; leave its times alone.
            let Some((accessed, written)) = times(&self.file) else {
                return;
            };
            if same(written, self.written) && !same(accessed, self.accessed) {
                let handle = HANDLE(self.file.as_raw_handle());
                // SAFETY: `handle` belongs to `self.file`, open for the call.
                let _ = unsafe { SetFileTime(handle, None, Some(&raw const self.accessed), None) };
            }
        }
    }

    fn times(file: &File) -> Option<(FILETIME, FILETIME)> {
        let handle = HANDLE(file.as_raw_handle());
        let mut accessed = FILETIME::default();
        let mut written = FILETIME::default();
        // SAFETY: `handle` is open and both out pointers are valid locals.
        unsafe {
            GetFileTime(
                handle,
                None,
                Some(&raw mut accessed),
                Some(&raw mut written),
            )
        }
        .ok()?;
        Some((accessed, written))
    }

    const fn same(a: FILETIME, b: FILETIME) -> bool {
        a.dwLowDateTime == b.dwLowDateTime && a.dwHighDateTime == b.dwHighDateTime
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::PreserveAccessTime;
    use std::time::{Duration, SystemTime};

    #[test]
    fn reads_under_the_guard_keep_the_access_time() {
        let dir = std::env::temp_dir().join(format!("lwa_atime_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("a.txt");
        std::fs::write(&path, b"hello").expect("write");
        let past = SystemTime::now() - Duration::from_hours(30 * 24);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .and_then(|file| file.set_times(std::fs::FileTimes::new().set_accessed(past)))
            .expect("set atime");
        let before = std::fs::metadata(&path).and_then(|m| m.accessed()).expect("atime");
        {
            let _guard = PreserveAccessTime::new(&path);
            let _ = std::fs::read(&path).expect("read");
        }
        let after = std::fs::metadata(&path).and_then(|m| m.accessed()).expect("atime");
        assert_eq!(before, after);
        let _ = std::fs::remove_dir_all(dir);
    }
}
