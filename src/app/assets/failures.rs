//! Remembering failed thumbnails and previews, and when to try them again.

use super::process::ErrorKind;
use std::time::{Duration, Instant};

/// First retry after this long, doubling with each further failure.
const RETRY_BASE: Duration = Duration::from_secs(30);
/// The doubling stops here: 30 s × 2⁸ ≈ 2 h between attempts.
const RETRY_SHIFT_CAP: u32 = 8;
/// After this many failures, broken media is not retried at all.
pub(super) const RETRY_MAX_TRIES: u32 = 3;

#[derive(Debug, Clone)]
pub(super) struct FailureRecord {
    pub last_attempt: Instant,
    pub tries: u32,
    pub reason: String,
    pub kind: ErrorKind,
}

/// What to do about an item that failed before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Retry {
    /// Broken or unsupported media: don't try again.
    Never,
    /// Wait this much longer before trying again.
    After(Duration),
    /// The wait is over.
    Now,
}

impl FailureRecord {
    /// Invalid or unsupported media and fixed limits are given up on after
    /// [`RETRY_MAX_TRIES`]; missing tools and timeouts can recover, so they
    /// keep retrying with exponential backoff.
    pub fn retry(&self) -> Retry {
        let permanent = matches!(
            self.kind,
            ErrorKind::InvalidMedia | ErrorKind::Unsupported | ErrorKind::Limit
        );
        if permanent && self.tries >= RETRY_MAX_TRIES {
            return Retry::Never;
        }
        let shift = self.tries.saturating_sub(1).min(RETRY_SHIFT_CAP);
        let backoff = RETRY_BASE * (1 << shift);
        let elapsed = self.last_attempt.elapsed();
        if elapsed < backoff {
            Retry::After(backoff.saturating_sub(elapsed))
        } else {
            Retry::Now
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FailureRecord, RETRY_BASE, RETRY_MAX_TRIES, Retry};
    use crate::app::assets::process::ErrorKind;
    use std::time::{Duration, Instant};

    fn record(tries: u32, kind: ErrorKind, ago: Duration) -> FailureRecord {
        FailureRecord {
            last_attempt: Instant::now().checked_sub(ago).expect("past instant"),
            tries,
            reason: String::new(),
            kind,
        }
    }

    #[test]
    fn backoff_doubles_and_broken_media_gives_up() {
        let fresh = record(1, ErrorKind::Timeout, Duration::ZERO);
        assert!(matches!(fresh.retry(), Retry::After(wait) if wait <= RETRY_BASE));
        let second = record(2, ErrorKind::Timeout, RETRY_BASE + Duration::from_secs(1));
        assert!(
            matches!(second.retry(), Retry::After(_)),
            "second failure waits 60 s"
        );
        let waited = record(1, ErrorKind::Timeout, RETRY_BASE + Duration::from_secs(1));
        assert_eq!(waited.retry(), Retry::Now);
        let broken = record(
            RETRY_MAX_TRIES,
            ErrorKind::InvalidMedia,
            Duration::from_hours(1),
        );
        assert_eq!(broken.retry(), Retry::Never);
        let missing_tool = record(
            RETRY_MAX_TRIES,
            ErrorKind::Unavailable,
            Duration::from_hours(1),
        );
        assert_eq!(missing_tool.retry(), Retry::Now);
    }
}
