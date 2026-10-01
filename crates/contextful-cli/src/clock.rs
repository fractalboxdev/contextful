//! The system clock every subcommand reads when no fixed instant is given.

use contextful_core::ports::{Clock, FixedClock};
use contextful_core::time::Instant;

/// The system clock.
pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
        Instant::from_unix_nanos(i128::try_from(nanos).unwrap_or_default()).unwrap_or_else(|_| FixedClock(Instant::from_unix_secs(0).expect("the epoch is an instant")).now())
    }
}
