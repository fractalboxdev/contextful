//! The per-connector resource bounds (`connector.package.linear-memory`,
//! `connector.package.call-deadline`, `connector.package.session-budget`) and the
//! failure-attribution bound (`connector.export.attribution-budget`).

use contextful_core::run::{Failure, FailureTag};
use std::time::Duration;

/// Linear memory a connector instance runs under unless raised: 256 MiB.
pub const DEFAULT_MEMORY_BYTES: u64 = 256 * 1024 * 1024;
/// Ceiling a per-connector override raises linear memory to: 2 GiB.
pub const MEMORY_CEILING_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Wall-clock deadline on one read call: 30 s.
pub const READ_DEADLINE: Duration = Duration::from_secs(30);
/// Wall-clock deadline on one discovery call: 60 s.
pub const DISCOVERY_DEADLINE: Duration = Duration::from_secs(60);
/// Epoch-interruption granularity arming the deadlines: 100 ms.
pub const EPOCH_TICK: Duration = Duration::from_millis(100);
/// Logging bytes one session accepts: 1 MiB.
pub const SESSION_LOG_BYTES: usize = 1024 * 1024;
/// Outbound requests one session holds open at once: 8.
pub const IN_FLIGHT: usize = 8;
/// Partition values one failure attribution returns per session: 512.
pub const ATTRIBUTION_ENTRIES: usize = 512;
/// Bytes in one attributed partition value: 512.
pub const ATTRIBUTION_VALUE_BYTES: usize = 512;

/// The bounds one session runs under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    pub memory_bytes: u64,
    pub read_deadline: Duration,
    pub discovery_deadline: Duration,
    pub log_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { memory_bytes: DEFAULT_MEMORY_BYTES, read_deadline: READ_DEADLINE, discovery_deadline: DISCOVERY_DEADLINE, log_bytes: SESSION_LOG_BYTES }
    }
}

impl Limits {
    /// The bounds with linear memory set per connector. An override past
    /// [`MEMORY_CEILING_BYTES`] is a configuration fault.
    pub fn with_memory(self, bytes: u64) -> Result<Limits, Failure> {
        if bytes > MEMORY_CEILING_BYTES {
            return Err(Failure::deterministic(
                FailureTag::Config,
                format!("a connector's linear memory is raised to at most {MEMORY_CEILING_BYTES} bytes, found {bytes}"),
            ));
        }
        Ok(Limits { memory_bytes: bytes, ..self })
    }
}
