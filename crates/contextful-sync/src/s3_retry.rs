//! S3 request retry policy and acquisition deadline.

use contextful_core::store::object::ObjectError;
use std::time::{Duration, Instant};

/// Wall-clock budget of one request, transfer included.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// Bounds on an attempt's wait for its answer once sent. A request
/// whose repetition changes nothing waits 2 s, then 4 s, then 10 s for each attempt's answer,
/// so 3 attempts held open unanswered end within 31 s; a conditional put waits 10 s once
/// (`store.endpoint.transport`).
pub(crate) const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);
const ANSWER_WINDOWS: [Duration; 3] = [
    Duration::from_secs(2),
    Duration::from_secs(4),
    ANSWER_TIMEOUT,
];

/// Total attempts of a request whose repetition changes nothing (`store.endpoint.transport`).
const TRANSPORT_ATTEMPTS: u32 = 3;

/// Total attempts of a request the backend answers throttled, and the backoff before each
/// resend: a uniform draw below a ceiling doubling from 100 ms to 5 s (`store.endpoint.throttled`).
const THROTTLED_ATTEMPTS: u32 = 5;
const BACKOFF_BASE: Duration = Duration::from_millis(100);
const BACKOFF_CAP: Duration = Duration::from_secs(5);

/// Which failures a request resends after (`store.endpoint.throttled`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resend {
    /// A request whose repetition changes nothing: a transport loss, a `429`, a `500` or a `503`.
    Repeatable,
    /// A conditional put, which may have applied unanswered: a `429` or a `503` alone, which
    /// answer a request the backend never applied.
    Conditional,
}

impl Resend {
    fn throttled(self, status: u16) -> bool {
        matches!(status, 429 | 503) || (status == 500 && self == Resend::Repeatable)
    }
}

/// Delay before throttled resend `n` (1-based): a uniform draw below `BACKOFF_BASE * 2^(n-1)`,
/// capped at `BACKOFF_CAP`, so clients throttled together spread their resends.
fn backoff(n: u32) -> Duration {
    let ceiling = BACKOFF_BASE
        .saturating_mul(1 << n.saturating_sub(1).min(16))
        .min(BACKOFF_CAP);
    let mut draw = [0u8; 8];
    let fraction = getrandom::fill(&mut draw)
        .map_or(1.0, |()| u64::from_le_bytes(draw) as f64 / u64::MAX as f64);
    ceiling.mul_f64(fraction)
}

pub(crate) fn resend_until<T>(
    what: &str,
    mode: Resend,
    deadline: Option<Instant>,
    send: impl Fn(Duration) -> Result<(u16, T), ureq::Error>,
) -> Result<(u16, T), ObjectError> {
    let (mut lost, mut throttled) = (0u32, 0u32);
    loop {
        let remaining = request_budget(what, deadline)?;
        let window = match mode {
            Resend::Repeatable => ANSWER_WINDOWS[(lost as usize).min(ANSWER_WINDOWS.len() - 1)],
            Resend::Conditional => ANSWER_TIMEOUT,
        };
        let answer = send(window.min(remaining));
        let delay = match answer {
            Ok((status, answer))
                if mode.throttled(status) && throttled + 1 < THROTTLED_ATTEMPTS =>
            {
                let _ = answer;
                throttled += 1;
                backoff(throttled)
            }
            Ok(answer) => return Ok(answer),
            Err(_) if mode == Resend::Repeatable && lost + 1 < TRANSPORT_ATTEMPTS => {
                lost += 1;
                Duration::from_millis(50 * u64::from(lost))
            }
            Err(e) => return Err(ObjectError::Unavailable(format!("{what}: {e}"))),
        };
        std::thread::sleep(delay.min(request_budget(what, deadline)?));
    }
}

pub(crate) fn request_budget(
    what: &str,
    deadline: Option<Instant>,
) -> Result<Duration, ObjectError> {
    let remaining = deadline.map_or(REQUEST_TIMEOUT, |deadline| {
        REQUEST_TIMEOUT.min(deadline.saturating_duration_since(Instant::now()))
    });
    if remaining.is_zero() {
        return Err(ObjectError::Unavailable(format!(
            "{what}: acquisition deadline"
        )));
    }
    Ok(remaining)
}
