use crate::helper::DetachedSpawn;
use anyhow::Context;
use std::{ffi::OsStr, iter::once, os::windows::ffi::OsStrExt};
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND},
        UI::Shell::{SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW, ShellExecuteExW},
    },
    core::PCWSTR,
};

pub fn display_in_explorer(path: impl AsRef<OsStr>) -> anyhow::Result<()> {
    std::process::Command::new("explorer.exe")
        .arg("/select,")
        .arg(&path)
        .spawn_detached()
        .context("Failed to open explorer")?;
    Ok(())
}

/// Opens the explorer properties for a given path
pub fn open_properties(path: impl AsRef<OsStr>) {
    // Convert the path to a wide string
    let wide_path: Vec<u16> = OsStr::new(&path).encode_wide().chain(once(0)).collect();
    // Convert the verb "properties" to a wide string
    let wide_verb: Vec<u16> = OsStr::new("properties")
        .encode_wide()
        .chain(once(0))
        .collect();
    // Set up the SHELLEXECUTEINFOW structure
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST,
        hwnd: HWND(std::ptr::null_mut()),
        lpVerb: PCWSTR::from_raw(wide_verb.as_ptr()),
        lpFile: PCWSTR::from_raw(wide_path.as_ptr()),
        lpParameters: PCWSTR::null(),
        lpDirectory: PCWSTR::null(),
        nShow: 0,
        hInstApp: HINSTANCE(std::ptr::null_mut()),
        ..Default::default()
    };

    // Open the properties window
    #[allow(unsafe_code)]
    unsafe {
        #[allow(clippy::borrow_as_ptr)]
        match ShellExecuteExW(&mut sei) {
            Ok(()) => {}
            Err(e) => eprintln!("Failed to open properties window: {e:?}"),
        }
    }
}

/// Thumbnail from the Windows shell (the same thumbnail cache Explorer uses),
/// scaled to fit `edge`. Returns straight-alpha RGBA. `None` when the shell
/// has no thumbnail provider for the file (callers then decode it themselves).
///
/// With `cached_only`, only thumbnails already in the OS cache are returned
/// (a cache hit costs a few ms; generating one can be slower than decoding
/// the image ourselves).
///
/// Must run on a thread where COM can be initialised; it is called from the
/// asset worker threads.
pub fn shell_thumbnail(
    path: &std::path::Path,
    edge: u32,
    cached_only: bool,
) -> Option<(u32, u32, Vec<u8>)> {
    use windows::Win32::{
        Foundation::SIZE,
        Graphics::Gdi::{
            BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS,
            DeleteDC, DeleteObject, GetDIBits, GetObjectW, HGDIOBJ,
        },
        System::Com::{COINIT_MULTITHREADED, CoInitializeEx},
        UI::Shell::{
            IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK,
            SIIGBF_INCACHEONLY, SIIGBF_THUMBNAILONLY,
        },
    };

    thread_local! {
        static COM_READY: bool = {
            #[allow(unsafe_code)]
            // SAFETY: initialising COM for the calling thread; never uninitialised
            // while the worker thread lives.
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).is_ok() }
        };
    }
    if !COM_READY.with(|ready| *ready) {
        return None;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(once(0)).collect();
    let side = i32::try_from(edge).ok()?;
    #[allow(unsafe_code)]
    // SAFETY: `wide` is NUL-terminated and outlives the call; every GDI handle
    // created here is released before returning, and the pixel buffer is sized
    // from the bitmap's own dimensions.
    unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(PCWSTR::from_raw(wide.as_ptr()), None).ok()?;
        let mut flags = SIIGBF_THUMBNAILONLY | SIIGBF_BIGGERSIZEOK;
        if cached_only {
            flags |= SIIGBF_INCACHEONLY;
        }
        let bitmap = factory.GetImage(SIZE { cx: side, cy: side }, flags).ok()?;
        let object = HGDIOBJ(bitmap.0);
        let mut info = BITMAP::default();
        let read = GetObjectW(
            object,
            i32::try_from(std::mem::size_of::<BITMAP>()).ok()?,
            Some((&raw mut info).cast()),
        );
        let (width, height) = (info.bmWidth, info.bmHeight.abs());
        if read == 0 || width <= 0 || height <= 0 {
            let _ = DeleteObject(object);
            return None;
        }
        let mut header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // Negative height: top-down rows.
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        let dc = CreateCompatibleDC(None);
        let lines = GetDIBits(
            dc,
            bitmap,
            0,
            height as u32,
            Some(pixels.as_mut_ptr().cast()),
            &raw mut header,
            DIB_RGB_COLORS,
        );
        let _ = DeleteDC(dc);
        let _ = DeleteObject(object);
        if lines != height {
            return None;
        }
        bgra_premultiplied_to_rgba(&mut pixels);
        Some((width as u32, height as u32, pixels))
    }
}

/// Shell bitmaps are BGRA with premultiplied alpha, and providers for opaque
/// formats often leave alpha at zero.
fn bgra_premultiplied_to_rgba(pixels: &mut [u8]) {
    let opaque = pixels.as_chunks::<4>().0.iter().all(|px| px[3] == 0);
    for px in pixels.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
        if opaque {
            px[3] = 255;
        } else if px[3] != 0 && px[3] != 255 {
            let alpha = u16::from(px[3]);
            for channel in &mut px[..3] {
                *channel = (u16::from(*channel) * 255 / alpha).min(255) as u8;
            }
        }
    }
}

/// Shows the Windows "Open with" dialog for a file.
pub fn open_with(path: impl AsRef<OsStr>) {
    use windows::Win32::UI::Shell::{
        OAIF_ALLOW_REGISTRATION, OAIF_EXEC, OAIF_REGISTER_EXT, OPENASINFO, SHOpenWithDialog,
    };
    let wide: Vec<u16> = OsStr::new(&path).encode_wide().chain(once(0)).collect();
    let info = OPENASINFO {
        pcszFile: windows::core::PCWSTR(wide.as_ptr()),
        pcszClass: windows::core::PCWSTR::null(),
        oaifInFlags: OAIF_ALLOW_REGISTRATION | OAIF_REGISTER_EXT | OAIF_EXEC,
    };
    // SAFETY: `info` points at a NUL-terminated path that outlives the call.
    #[allow(unsafe_code)]
    let result = unsafe { SHOpenWithDialog(None, &raw const info) };
    if let Err(error) = result {
        log::warn!("Open with failed: {error:?}");
    }
}

/// Asks for a folder with the standard Windows folder picker, starting in
/// `start`. Blocks until it closes; `None` when cancelled.
pub fn pick_folder(start: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    pick(start, None)
}

/// Asks for a program (an .exe) with the standard Windows open dialog,
/// starting in `start`. Blocks until it closes; `None` when cancelled.
pub fn pick_program(start: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    pick(start, Some(("Programs", "*.exe")))
}

/// The Windows open dialog: a folder picker without `filter`, otherwise a
/// file picker showing files matching `filter` (name, pattern).
fn pick(
    start: Option<&std::path::Path>,
    filter: Option<(&str, &str)>,
) -> Option<std::path::PathBuf> {
    use windows::Win32::{
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoTaskMemFree,
        },
        UI::Shell::{
            Common::COMDLG_FILTERSPEC, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog,
            IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
        },
    };
    let wide = |text: &OsStr| -> Vec<u16> { text.encode_wide().chain(once(0)).collect() };
    #[allow(unsafe_code)]
    // SAFETY: plain COM calls on the UI thread. Initialising COM again is
    // harmless (the window already did); every string passed in is
    // NUL-terminated and outlives its call, and the returned path is freed
    // with `CoTaskMemFree` as the API requires.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let mut options = dialog.GetOptions().ok()? | FOS_FORCEFILESYSTEM;
        // Kept alive until the dialog closes: the filter points into them.
        let filter_text =
            filter.map(|(name, pattern)| (wide(OsStr::new(name)), wide(OsStr::new(pattern))));
        match &filter_text {
            Some((name, pattern)) => {
                let spec = [COMDLG_FILTERSPEC {
                    pszName: PCWSTR(name.as_ptr()),
                    pszSpec: PCWSTR(pattern.as_ptr()),
                }];
                let _ = dialog.SetFileTypes(&spec);
            }
            None => options |= FOS_PICKFOLDERS,
        }
        dialog.SetOptions(options).ok()?;
        if let Some(start) = start {
            let path = wide(start.as_os_str());
            if let Ok(folder) =
                SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(path.as_ptr()), None)
            {
                let _ = dialog.SetFolder(&folder);
            }
        }
        dialog.Show(None).ok()?;
        let name = dialog
            .GetResult()
            .ok()?
            .GetDisplayName(SIGDN_FILESYSPATH)
            .ok()?;
        let path = name.to_string().ok();
        CoTaskMemFree(Some(name.0.cast()));
        path.map(std::path::PathBuf::from)
    }
}

/// A Windows Job Object that owns a child process and everything it starts.
/// Closing it (on drop, or when the app exits for any reason) kills them all,
/// so media tools never outlive a cancelled request or the app itself.
pub struct KillOnCloseJob(windows::Win32::Foundation::HANDLE);

// SAFETY: a job handle is a plain kernel handle; using it from another thread
// is allowed.
#[allow(unsafe_code)]
unsafe impl Send for KillOnCloseJob {}

impl KillOnCloseJob {
    /// Creates the job and puts `child` in it.
    #[allow(unsafe_code)]
    pub fn for_child(child: &std::process::Child) -> Option<Self> {
        use std::os::windows::io::AsRawHandle as _;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };
        // SAFETY: an unnamed job with default security; the handle is owned
        // by `Self` and closed in `Drop`.
        let job = Self(unsafe { CreateJobObjectW(None, PCWSTR::null()) }.ok()?);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `limits` is the struct this information class expects, and
        // the child handle stays valid while `child` is alive.
        unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                u32::try_from(std::mem::size_of_val(&limits)).ok()?,
            )
            .ok()?;
            AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle())).ok()?;
        }
        Some(job)
    }

    /// Kills every process in the job.
    #[allow(unsafe_code)]
    pub fn terminate(&self) {
        // SAFETY: `self.0` is a live job handle.
        let _ = unsafe { windows::Win32::System::JobObjects::TerminateJobObject(self.0, 1) };
    }
}

impl Drop for KillOnCloseJob {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: the handle is owned by `self` and closed exactly once.
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn premultiplied_bgra_becomes_straight_rgba() {
        let mut pixels = vec![10, 20, 30, 0, 64, 32, 16, 128];
        super::bgra_premultiplied_to_rgba(&mut pixels);
        assert_eq!(pixels, [30, 20, 10, 0, 31, 63, 127, 128]);
        let mut opaque = vec![1, 2, 3, 0];
        super::bgra_premultiplied_to_rgba(&mut opaque);
        assert_eq!(opaque, [3, 2, 1, 255]);
    }

    /// Exercises the real shell provider; ignored because it depends on the
    /// host's thumbnail handlers.
    #[test]
    #[ignore = "depends on the Windows shell thumbnail providers"]
    fn shell_thumbnail_reads_a_png() {
        let path = std::env::temp_dir().join(format!("lwa_shell_thumb_{}.png", std::process::id()));
        image::RgbaImage::from_pixel(400, 200, image::Rgba([200, 40, 10, 255]))
            .save(&path)
            .expect("write fixture");
        let (width, height, pixels) =
            super::shell_thumbnail(&path, 96, false).expect("shell thumbnail for PNG");
        assert!(
            super::shell_thumbnail(&path, 96, true).is_some(),
            "now cached by the shell"
        );
        assert!(width >= 96 && height > 0, "{width}x{height}");
        let center = ((height / 2 * width + width / 2) * 4) as usize;
        assert!(
            pixels[center] > 150 && pixels[center + 2] < 60,
            "{:?}",
            &pixels[center..center + 4]
        );
        let _ = std::fs::remove_file(path);
    }
}
