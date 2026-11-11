//! Windows specifics: the Mark of the Web, positional reads, local time and
//! process liveness. Unix permissions and extended attributes have no
//! counterpart here, so the functions for them do nothing.

use std::fs;
use std::io::{self, Read};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION, FILETIME, SYSTEMTIME, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_HIDDEN, FILE_FLAG_BACKUP_SEMANTICS, FILE_WRITE_ATTRIBUTES, SetFileAttributesW,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};
use windows_sys::Win32::System::Time::{SystemTimeToFileTime, TzSpecificLocalTimeToSystemTime};

/// Zones from this one up mark a file as coming from somewhere untrusted:
/// 3 is the internet, 4 a restricted site.
const INTERNET_ZONE: u32 = 3;

/// The stream of `path` that holds its Mark of the Web.
fn zone_stream(path: &Path) -> PathBuf {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    stream.into()
}

/// The Mark of the Web to put on files extracted from `path`, when browsers
/// or mail clients marked it as downloaded. Only the zone is passed on: the
/// address the archive came from stays with the archive.
pub(crate) fn quarantine(path: &Path) -> Option<Vec<u8>> {
    // A relative `a:Zone.Identifier` would name a file on drive A.
    let path = std::path::absolute(path).ok()?;
    let mut mark = Vec::new();
    fs::File::open(zone_stream(&path))
        .ok()?
        .take(4096)
        .read_to_end(&mut mark)
        .ok()?;
    let zone: u32 = String::from_utf8_lossy(&mark)
        .lines()
        .find_map(|line| line.trim().strip_prefix("ZoneId="))?
        .trim()
        .parse()
        .ok()?;
    (zone >= INTERNET_ZONE).then(|| format!("[ZoneTransfer]\r\nZoneId={zone}\r\n").into_bytes())
}

/// Folders and links carry no mark on Windows.
pub(crate) fn set_quarantine(_path: &Path, _value: &[u8]) {}

/// Writes the mark onto the file at `path`, which must be absolute. Best
/// effort: a volume without streams, such as FAT, can't take it.
pub(crate) fn set_file_quarantine(_file: &fs::File, path: &Path, value: &[u8]) {
    let _ = fs::write(zone_stream(path), value);
}

pub(crate) fn set_xattr(_path: &Path, _name: &str, _value: &[u8]) {}

pub(crate) fn set_file_mode(_file: &fs::File, _mode: u32) {}

pub(crate) fn set_dir_mode(_path: &Path, _mode: u32) {}

/// Nothing is restored read-only here, so there is nothing to open up.
pub(crate) fn make_writable(_dir: &Path) -> io::Result<()> {
    Ok(())
}

/// Names that differ only in case are the same file on Windows volumes.
pub(crate) fn case_sensitive(_dir: &Path) -> bool {
    false
}

/// Whether a process with `pid` is running.
pub(crate) fn process_alive(pid: i32) -> bool {
    // SAFETY: the call takes no pointers.
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid as u32,
        )
    };
    if handle.is_null() {
        // A process of another user exists but can't be opened.
        return io::Error::last_os_error().raw_os_error() == Some(ERROR_ACCESS_DENIED as i32);
    }
    // SAFETY: `handle` is an open process handle; it is closed once, here.
    unsafe {
        // A handle can outlive its process, which is then signaled.
        let running = WaitForSingleObject(handle, 0) == WAIT_TIMEOUT;
        CloseHandle(handle);
        running
    }
}

/// Reads from `file` at `offset`. The read moves the file's position, which
/// nothing relies on.
pub(crate) fn read_at(file: &fs::File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    file.seek_read(buf, offset)
}

/// A folder is only opened for its attributes when asked for exactly that.
pub(crate) fn set_dir_mtime(path: &Path, mtime: SystemTime) -> io::Result<()> {
    fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?
        .set_times(fs::FileTimes::new().set_modified(mtime))
}

/// A stored link target or member name as a path.
pub(crate) fn raw_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

/// Hides the working folder, as its leading dot does elsewhere. Best effort.
pub(crate) fn hide(path: &Path) {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    unsafe {
        SetFileAttributesW(wide.as_ptr(), FILE_ATTRIBUTE_HIDDEN);
    }
}

/// Virus scanners and the search indexer open new files for a moment, which
/// makes moving or deleting them fail; a short wait gets past that.
fn retrying<T>(mut attempt: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut wait = Duration::from_millis(5);
    loop {
        match attempt() {
            Err(e)
                if wait < Duration::from_millis(400)
                    && matches!(
                        e.raw_os_error().map(|code| code as u32),
                        Some(ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION)
                    ) =>
            {
                std::thread::sleep(wait);
                wait *= 2;
            }
            result => return result,
        }
    }
}

pub(crate) fn rename(from: &Path, to: &Path) -> io::Result<()> {
    retrying(|| fs::rename(from, to))
}

pub(crate) fn remove_file(path: &Path) -> io::Result<()> {
    retrying(|| fs::remove_file(path))
}

/// A local date and time, as archives from DOS and Windows store them, as
/// an instant. A date that doesn't exist gives `None`.
pub(crate) fn local_time(
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
) -> Option<SystemTime> {
    let local = SYSTEMTIME {
        wYear: u16::try_from(year).ok()?,
        wMonth: month.into(),
        wDayOfWeek: 0,
        wDay: day.into(),
        wHour: hour.into(),
        wMinute: minute.into(),
        // DOS times count seconds in twos, up to a 60 that no minute has.
        wSecond: second.min(59).into(),
        wMilliseconds: 0,
    };
    let mut utc = SYSTEMTIME::default();
    let mut ticks = FILETIME::default();
    // SAFETY: all three point at initialized values that outlive the calls;
    // a null time zone means the system's current one.
    let converted = unsafe {
        TzSpecificLocalTimeToSystemTime(std::ptr::null(), &local, &mut utc) != 0
            && SystemTimeToFileTime(&utc, &mut ticks) != 0
    };
    if !converted {
        return None;
    }
    // 100-nanosecond steps since 1601.
    let ticks = u64::from(ticks.dwHighDateTime) << 32 | u64::from(ticks.dwLowDateTime);
    let seconds = (ticks / 10_000_000).checked_sub(11_644_473_600)?;
    Some(UNIX_EPOCH + Duration::from_secs(seconds))
}

/// A UTC date and time as an instant.
pub(crate) fn utc_time(
    year: i32,
    month: i32,
    day: i32,
    hour: i32,
    minute: i32,
    second: i32,
) -> Option<SystemTime> {
    if !(1..=12).contains(&month) {
        return None;
    }
    // Days since 1970 in the proleptic Gregorian calendar, counting years
    // from March so that a leap day is the last day of its year.
    let year = i64::from(if month <= 2 { year - 1 } else { year });
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let day_of_year = (153 * i64::from((month + 9) % 12) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds =
        days * 86_400 + i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second);
    u64::try_from(seconds)
        .ok()
        .map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seconds(time: Option<SystemTime>) -> Option<u64> {
        time.map(|t| t.duration_since(UNIX_EPOCH).unwrap().as_secs())
    }

    #[test]
    fn utc_dates() {
        assert_eq!(seconds(utc_time(1970, 1, 1, 0, 0, 0)), Some(0));
        assert_eq!(seconds(utc_time(2000, 2, 29, 0, 0, 0)), Some(951_782_400));
        assert_eq!(
            seconds(utc_time(2017, 3, 8, 19, 56, 42)),
            Some(1_489_003_002)
        );
        assert_eq!(seconds(utc_time(1969, 12, 31, 23, 59, 59)), None);
        assert_eq!(seconds(utc_time(2020, 13, 1, 0, 0, 0)), None);
    }

    #[test]
    fn local_dates_round_trip_through_the_system_zone() {
        let noon = local_time(2024, 1, 15, 12, 0, 0).unwrap();
        let utc = utc_time(2024, 1, 15, 12, 0, 0).unwrap();
        // No zone is further than 14 hours from UTC.
        let apart = noon
            .duration_since(utc)
            .unwrap_or_else(|e| e.duration())
            .as_secs();
        assert!(apart <= 14 * 3600 && apart.is_multiple_of(900), "{apart}");
        assert_eq!(local_time(2024, 2, 30, 0, 0, 0), None);
        assert!(local_time(2024, 1, 15, 12, 0, 60).is_some());
    }

    #[test]
    fn own_process_is_alive() {
        assert!(process_alive(std::process::id() as i32));
        // Process ids are multiples of four.
        assert!(!process_alive(0x7fff_fff1));
    }

    #[test]
    fn mark_of_the_web_passes_on_the_zone_only() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.zip");
        fs::write(&archive, b"x").unwrap();
        assert_eq!(quarantine(&archive), None);
        fs::write(
            zone_stream(&archive),
            "[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=https://example.com/a.zip\r\n",
        )
        .unwrap();
        let mark = quarantine(&archive).unwrap();
        assert_eq!(mark, b"[ZoneTransfer]\r\nZoneId=3\r\n");

        let out = dir.path().join("out.txt");
        let file = fs::File::create(&out).unwrap();
        set_file_quarantine(&file, &out, &mark);
        drop(file);
        assert_eq!(fs::read(zone_stream(&out)).unwrap(), mark);

        // A file from the local intranet isn't marked on.
        fs::write(zone_stream(&archive), "[ZoneTransfer]\r\nZoneId=1\r\n").unwrap();
        assert_eq!(quarantine(&archive), None);
    }
}
