//! How far local time is from UTC at given moments, for the dates a batch
//! rename puts in names: a photo taken in July is named by July's clock,
//! whatever the season is now.
use chrono::{Local, Offset, TimeZone};
use dirfleet_core::app::LocalOffsets;

/// Local time minus UTC at each of the request's times, in seconds; 0 for
/// a time the system can't place.
pub fn offsets(request: &LocalOffsets) -> Vec<i32> {
    request
        .times
        .iter()
        .map(|&time| {
            i64::try_from(time)
                .ok()
                .and_then(|time| Local.timestamp_opt(time, 0).single())
                .map_or(0, |at| at.offset().fix().local_minus_utc())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_time_gets_an_offset_a_clock_could_have() {
        let times = vec![0, 1_709_251_199, 1_762_610_580, u64::MAX];
        let offsets = offsets(&LocalOffsets {
            times: times.clone(),
        });
        assert_eq!(offsets.len(), times.len());
        // No place is further than fourteen hours from UTC.
        assert!(offsets.iter().all(|offset| offset.abs() <= 14 * 3600));
        assert_eq!(offsets[3], 0, "a time out of range");
        // The offset is the one the system's own calendar uses.
        let at = Local
            .timestamp_opt(1_762_610_580, 0)
            .single()
            .expect("a time");
        assert_eq!(offsets[2], at.offset().fix().local_minus_utc());
    }
}
