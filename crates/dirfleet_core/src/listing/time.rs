use std::{
    fmt::Display,
    ops::Deref,
    time::{Duration, SystemTime},
};

use bincode::{Decode, Encode};
use serde::{Deserialize, Serialize};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Default,
    Deserialize,
    Serialize,
    Decode,
    Encode,
    Hash,
)]
pub struct TimestampSeconds(u32);

impl TimestampSeconds {
    /// The time `seconds` after the Unix epoch.
    #[inline]
    #[must_use]
    pub const fn from_unix(seconds: u32) -> Self {
        Self(seconds)
    }

    #[inline]
    #[must_use]
    pub fn to_duration(self) -> Duration {
        self.into()
    }

    #[inline]
    #[must_use]
    pub fn system_time(self) -> SystemTime {
        std::time::UNIX_EPOCH + self.to_duration()
    }

    /// The time from this moment until `now`: the core never reads the clock.
    #[inline]
    #[must_use]
    pub fn elapsed_at(self, now: SystemTime) -> ElapsedTime {
        now.duration_since(self.system_time())
            .map_or(ElapsedTime::None, |duration| {
                ElapsedTime::from_seconds(duration.as_secs())
            })
    }
}

impl Deref for TimestampSeconds {
    type Target = u32;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<SystemTime> for TimestampSeconds {
    #[inline]
    fn from(value: SystemTime) -> Self {
        // Unix timestamp in seconds (valid until year 2262)
        let timestamp_seconds = value
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;
        Self(timestamp_seconds)
    }
}

impl From<TimestampSeconds> for Duration {
    #[inline]
    fn from(value: TimestampSeconds) -> Self {
        Self::from_secs(u64::from(value.0))
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Default,
    Hash,
    Deserialize,
    Serialize,
    Decode,
    Encode,
)]
pub enum ElapsedTime {
    #[default]
    None,
    Seconds(u32),
    Minutes(u32),
    Hours(u32),
    Days(u32),
    Years(u32),
}

impl ElapsedTime {
    #[inline]
    #[must_use]
    pub const fn from_seconds(seconds: u64) -> Self {
        let days = seconds / 86400;
        if days > 0 {
            let years = days / 365;
            if years > 0 {
                Self::Years(years as u32)
            } else {
                Self::Days(days as u32)
            }
        } else if seconds > 3600 {
            Self::Hours(seconds as u32 / 3600)
        } else if seconds > 60 {
            Self::Minutes(seconds as u32 / 60)
        } else {
            Self::Seconds(seconds as u32)
        }
    }
}

impl Display for ElapsedTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ago = |f: &mut std::fmt::Formatter<'_>, count: u32, unit: &str| {
            let plural = if count == 1 { "" } else { "s" };
            write!(f, "{count} {unit}{plural} ago")
        };
        match *self {
            Self::None => write!(f, "Future"),
            Self::Seconds(_) => write!(f, "Just now"),
            Self::Minutes(m) => write!(f, "{m} min ago"),
            Self::Hours(h) => ago(f, h, "hour"),
            // Day counts stop being useful after a couple of weeks.
            Self::Days(d) if d >= 60 => ago(f, d / 30, "month"),
            Self::Days(d) if d >= 14 => ago(f, d / 7, "week"),
            Self::Days(d) => ago(f, d, "day"),
            Self::Years(y) => ago(f, y, "year"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ElapsedTime;

    #[test]
    fn elapsed_time_reads_naturally() {
        assert_eq!(ElapsedTime::Days(1).to_string(), "1 day ago");
        assert_eq!(ElapsedTime::Days(3).to_string(), "3 days ago");
        assert_eq!(ElapsedTime::Days(20).to_string(), "2 weeks ago");
        assert_eq!(ElapsedTime::Days(200).to_string(), "6 months ago");
        assert_eq!(ElapsedTime::Hours(1).to_string(), "1 hour ago");
        assert_eq!(ElapsedTime::Years(1).to_string(), "1 year ago");
    }
}
