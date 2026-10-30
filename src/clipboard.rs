//! Copy, cut and paste of files through the system clipboard.
//!
//! On Windows this uses the same clipboard formats as Explorer (`CF_HDROP`
//! plus `Preferred DropEffect`), so files can be copied between `DirFleet` and
//! Explorer in both directions. On macOS it uses file URLs on the general
//! pasteboard, as Finder does. Elsewhere an in-process clipboard is used, and
//! the paths are also put on the text clipboard as a `text/uri-list`.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Copy,
    Cut,
}

/// Puts `paths` on the clipboard for a later paste.
pub fn write_files(paths: &[PathBuf], mode: Mode) -> Result<(), String> {
    #[cfg(windows)]
    {
        windows::write_files(paths, mode)
    }
    #[cfg(target_os = "macos")]
    {
        macos::write_files(paths, mode)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        fallback::write_files(paths, mode);
        Ok(())
    }
}

/// The files on the clipboard, if it holds files.
pub fn read_files() -> Option<(Vec<PathBuf>, Mode)> {
    #[cfg(windows)]
    {
        windows::read_files()
    }
    #[cfg(target_os = "macos")]
    {
        macos::read_files()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        fallback::read_files()
    }
}

/// Changes whenever anything (in any application) writes to the clipboard.
pub fn sequence_number() -> u64 {
    #[cfg(windows)]
    {
        windows::sequence_number()
    }
    #[cfg(target_os = "macos")]
    {
        macos::sequence_number()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        fallback::sequence_number()
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod fallback {
    use super::Mode;
    use std::path::PathBuf;
    use std::sync::{Mutex, PoisonError};

    static FILES: Mutex<Option<(Vec<PathBuf>, Mode, u64)>> = Mutex::new(None);

    pub fn write_files(paths: &[PathBuf], mode: Mode) {
        let mut files = FILES.lock().unwrap_or_else(PoisonError::into_inner);
        let sequence = files.as_ref().map_or(1, |(_, _, seq)| seq + 1);
        *files = Some((paths.to_vec(), mode, sequence));
        // Best effort: other applications can at least paste the paths.
        let uris = paths
            .iter()
            .map(|path| format!("file://{}", path.display()))
            .collect::<Vec<_>>()
            .join("\r\n");
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let _ = clipboard.set_text(uris);
        }
    }

    pub fn read_files() -> Option<(Vec<PathBuf>, Mode)> {
        FILES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|(paths, mode, _)| (paths.clone(), *mode))
    }

    pub fn sequence_number() -> u64 {
        FILES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map_or(0, |(_, _, seq)| *seq)
    }
}

#[cfg(test)]
mod tests {
    use super::{Mode, read_files, sequence_number, write_files};
    use std::path::PathBuf;

    /// Uses the real system clipboard, so it only runs on request.
    #[test]
    #[ignore = "replaces the contents of the system clipboard"]
    fn files_round_trip_through_the_clipboard() {
        let paths = vec![
            std::env::temp_dir().join("a file.txt"),
            std::env::temp_dir().join("zażółć folder"),
        ];
        let before = sequence_number();
        write_files(&paths, Mode::Cut).expect("write");
        assert_ne!(sequence_number(), before);
        assert_eq!(read_files(), Some((paths.clone(), Mode::Cut)));
        write_files(&paths[..1], Mode::Copy).expect("write");
        assert_eq!(
            read_files(),
            Some((vec![PathBuf::from(&paths[0])], Mode::Copy))
        );
    }
}
