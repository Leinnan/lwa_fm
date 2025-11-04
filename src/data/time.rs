use std::{
    fmt::Display,
    ops::Deref,
    time::{Duration, SystemTime},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct TimestampSeconds(u64);

impl TimestampSeconds {
    #[inline]
    pub fn to_duration(self) -> Duration {
        self.into()
    }

    #[inline]
    pub fn system_time(self) -> SystemTime {
        std::time::UNIX_EPOCH + self.to_duration()
    }

    #[inline]
    pub fn elapsed(self) -> ElapsedTime {
        match self.system_time().elapsed() {
            Err(_) => ElapsedTime::None,
            Ok(duration) => ElapsedTime::from_seconds(duration.as_secs()),
        }
    }
}

impl Deref for TimestampSeconds {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<SystemTime> for TimestampSeconds {
    #[inline]
    fn from(value: SystemTime) -> Self {
        // Unix timestamp in seconds (valid until year 2262)
        let timestamp_seconds: u64 = value
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self(timestamp_seconds)
    }
}

impl Into<Duration> for TimestampSeconds {
    #[inline]
    fn into(self) -> Duration {
        Duration::from_secs(self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub enum ElapsedTime {
    #[default]
    None,
    Seconds(u64),
    Minutes(u64),
    Hours(u64),
    Days(u64),
    Years(u64),
}

impl ElapsedTime {
    #[inline]
    pub fn from_seconds(seconds: u64) -> Self {
        let days = seconds / 86400;
        if days > 0 {
            let years = days / 365;
            if years > 0 {
                Self::Years(years)
            } else {
                Self::Days(days)
            }
        } else if seconds > 3600 {
            Self::Hours(seconds / 3600)
        } else if seconds > 60 {
            Self::Minutes(seconds / 60)
        } else {
            Self::Seconds(seconds)
        }
    }
}

impl Display for ElapsedTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ElapsedTime::None => write!(f, "Future"),
            ElapsedTime::Seconds(_) => write!(f, "Just now"),
            ElapsedTime::Minutes(m) => write!(f, "{} min ago", m),
            ElapsedTime::Hours(h) => write!(f, "{} hours ago", h),
            ElapsedTime::Days(d) => write!(f, "{} days ago", d),
            ElapsedTime::Years(y) => write!(f, "{} years ago", y),
        }
    }
}
