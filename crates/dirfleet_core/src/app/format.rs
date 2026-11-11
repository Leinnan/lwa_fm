//! Text shown for entries, the same in every shell.
use crate::columns::Column;
use crate::listing::{DirEntryMetaData, ElapsedTime, TimestampSeconds, kind_label};

const SIZE_UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

/// A file's size in decimal units; nothing for folders.
pub fn size(meta: &DirEntryMetaData) -> String {
    if !meta.entry_type.is_leaf() {
        return String::new();
    }
    bytes(meta.size)
}

/// A number of bytes in decimal units ("1.2 GB").
pub fn bytes(count: u64) -> String {
    let mut value = count as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < SIZE_UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{count} {}", SIZE_UNITS[0])
    } else {
        format!("{value:.1} {}", SIZE_UNITS[unit])
    }
}

/// The text of `column` for the entry `name`. Opens are counted by open
/// history, which the core doesn't have yet: empty.
pub fn cell(column: Column, name: &str, meta: &DirEntryMetaData) -> String {
    match column {
        Column::Name => name.to_owned(),
        Column::Modified => modified(meta),
        Column::Created => date(meta.created_at),
        Column::Accessed => date(meta.accessed_at),
        Column::Kind => kind_label(name, meta.entry_type),
        Column::Size => size(meta),
        Column::Folder | Column::OpenCount => String::new(),
    }
}

/// When the entry last changed, as [`date`] writes it.
pub fn modified(meta: &DirEntryMetaData) -> String {
    date(meta.modified_at)
}

/// How long ago `time` was, as egui's date columns say it ("3 days ago"),
/// when the time `now` (Unix seconds) is known; otherwise the [`date`].
/// Nothing when `time` is unknown.
pub fn when(time: TimestampSeconds, now: u64) -> String {
    if *time == 0 {
        return String::new();
    }
    if now == 0 {
        return date(time);
    }
    now.checked_sub(u64::from(*time))
        .map_or(ElapsedTime::None, ElapsedTime::from_seconds)
        .to_string()
}

/// "2026-11-08 14:03" in UTC, or nothing when unknown.
pub fn date(time: TimestampSeconds) -> String {
    let seconds = i64::from(*time);
    if seconds <= 0 {
        return String::new();
    }
    let days = seconds.div_euclid(86_400);
    let seconds = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        seconds / 3600,
        seconds % 3600 / 60
    )
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
const fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::listing::{EntryType, TimestampSeconds};
    use std::time::{Duration, UNIX_EPOCH};

    fn file(size: u64, modified: u64) -> DirEntryMetaData {
        let mut meta = DirEntryMetaData::from(EntryType::File);
        meta.size = size;
        meta.modified_at = TimestampSeconds::from(UNIX_EPOCH + Duration::from_secs(modified));
        meta
    }

    #[test]
    fn sizes_use_decimal_units() {
        assert_eq!(size(&file(999, 0)), "999 B");
        assert_eq!(size(&file(1_500_000, 0)), "1.5 MB");
        assert_eq!(size(&DirEntryMetaData::from(EntryType::Directory)), "");
    }

    #[test]
    fn dates_read_as_time_ago_once_the_time_is_known() {
        let at = TimestampSeconds::from_unix(1_762_610_580);
        assert_eq!(when(at, 0), "2025-11-08 14:03", "no clock yet");
        assert_eq!(when(at, 1_762_610_580 + 30), "Just now");
        assert_eq!(when(at, 1_762_610_580 + 3 * 86_400 + 5), "3 days ago");
        assert_eq!(when(at, 1_762_610_580 - 10), "Future");
        assert_eq!(when(TimestampSeconds::from_unix(0), 1_762_610_580), "");
    }

    #[test]
    fn dates_are_utc_calendar_dates() {
        assert_eq!(modified(&file(0, 1_762_610_580)), "2025-11-08 14:03");
        assert_eq!(modified(&file(0, 0)), "");
    }
}
