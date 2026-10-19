//! `run.retry`: the step schedule, the one retry layer, and the pure decision the runner
//! consults after each closed attempt.

use super::failure::{Failure, FailureTag};
use super::journal::EntryKey;
use super::RunError;
use crate::time::Instant;
use serde::{Deserialize, Serialize};

/// First delay of the default schedule: 100 ms (`run.retry.default-policy`).
pub const DEFAULT_BASE_MS: u64 = 100;
/// Growth factor of the default schedule.
pub const DEFAULT_FACTOR: u32 = 2;
/// Total attempts of the default schedule, the first included: 5 attempts.
pub const DEFAULT_ATTEMPTS: u32 = 5;
/// Clamp on one computed delay: 30 s.
pub const DEFAULT_MAX_DELAY_MS: u64 = 30_000;
/// Seeded additive jitter stays below 250 ms.
pub const DEFAULT_JITTER_CEILING_MS: u64 = 250;
/// Total attempts of the no-retry schedule: 1 attempt (`run.retry.single-attempt`).
pub const SINGLE_ATTEMPT: u32 = 1;
/// Longest server-requested delay a step sleeps before closing: 300 s (`run.retry.retry-after`).
pub const RETRY_AFTER_CEILING_SECS: u64 = 300;

/// The two tags a schedule retries (`run.retry.retryable-classes`).
pub const RETRYABLE: [FailureTag; 2] = [FailureTag::Transient, FailureTag::RateLimited];

/// How the delay grows between attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Backoff {
    Fixed { delay_ms: u64 },
    Exponential { base_ms: u64, factor: u32 },
}

/// A step's retry schedule (`run.retry.schedule`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub backoff: Backoff,
    /// Total attempts, the first included.
    pub attempts: u32,
    pub max_delay_ms: u64,
    pub jitter_ceiling_ms: u64,
    /// The retryable tags this schedule retries; a subset of [`RETRYABLE`].
    pub retry_on: Vec<FailureTag>,
}

impl Default for Schedule {
    /// Exponential from 100 ms with factor 2, 5 attempts, each delay clamped at 30 s,
    /// plus seeded jitter below 250 ms.
    fn default() -> Schedule {
        Schedule {
            backoff: Backoff::Exponential { base_ms: DEFAULT_BASE_MS, factor: DEFAULT_FACTOR },
            attempts: DEFAULT_ATTEMPTS,
            max_delay_ms: DEFAULT_MAX_DELAY_MS,
            jitter_ceiling_ms: DEFAULT_JITTER_CEILING_MS,
            retry_on: RETRYABLE.to_vec(),
        }
    }
}

impl Schedule {
    /// The no-retry schedule.
    pub fn single_attempt() -> Schedule {
        Schedule { attempts: SINGLE_ATTEMPT, ..Schedule::default() }
    }
}

/// A schedule as a plan declares it; every field absent takes the default's value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ScheduleSpec {
    /// `fixed` or `exponential`.
    #[serde(default)]
    pub backoff: Option<String>,
    #[serde(default)]
    pub base_ms: Option<u64>,
    #[serde(default)]
    pub factor: Option<u32>,
    #[serde(default)]
    pub attempts: Option<u32>,
    #[serde(default)]
    pub max_delay_ms: Option<u64>,
    #[serde(default)]
    pub jitter_ms: Option<u64>,
    #[serde(default)]
    pub retry_on: Option<Vec<String>>,
}

impl ScheduleSpec {
    /// Compile the declared schedule. A `retry_on` naming a tag outside the retryable
    /// pair refuses here, at plan compile (`run.retry.unretryable-tag`).
    pub fn compile(&self) -> Result<Schedule, RunError> {
        let d = Schedule::default();
        let retry_on = match &self.retry_on {
            None => d.retry_on.clone(),
            Some(names) => {
                let mut tags = Vec::new();
                for n in names {
                    match FailureTag::parse(n).filter(|t| RETRYABLE.contains(t)) {
                        Some(t) => tags.push(t),
                        None => {
                            return Err(RunError::RunRetryTagUnretryable(format!(
                                "`retry_on` names `{n}`; only `Transient` and `RateLimited` retry"
                            )))
                        }
                    }
                }
                tags
            }
        };
        let base_ms = self.base_ms.unwrap_or(DEFAULT_BASE_MS);
        let backoff = match self.backoff.as_deref() {
            None | Some("exponential") => Backoff::Exponential { base_ms, factor: self.factor.unwrap_or(DEFAULT_FACTOR) },
            Some("fixed") => Backoff::Fixed { delay_ms: base_ms },
            Some(other) => return Err(RunError::Invalid(format!("backoff `{other}` is neither `fixed` nor `exponential`"))),
        };
        let attempts = self.attempts.unwrap_or(d.attempts);
        if attempts == 0 {
            return Err(RunError::Invalid("a schedule makes at least 1 attempt".into()));
        }
        Ok(Schedule {
            backoff,
            attempts,
            max_delay_ms: self.max_delay_ms.unwrap_or(d.max_delay_ms),
            jitter_ceiling_ms: self.jitter_ms.unwrap_or(d.jitter_ceiling_ms),
            retry_on,
        })
    }
}

/// What the runner does after an attempt closes on a failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Sleep this long, then make the next attempt.
    Retry { delay_ms: u64 },
    /// The failure stands as the step's answer: `StepFailed`. `consumed_attempt` is
    /// false for a deterministic refusal, which spends no budget.
    Fail { error: RunError, consumed_attempt: bool },
    /// The server asked for a delay past the ceiling: the attempt closes waiting until its
    /// next-attempt instant rather than sleeping (`run.retry.not-before`).
    Reschedule { error: RunError, retry_after_secs: u64 },
}

/// The label prefix of a step's next-attempt journal entry.
pub const NOT_BEFORE_PREFIX: &str = "not-before:";

/// A step's next attempt: the instant before which no attempt runs and the attempts its
/// schedule closed, recorded in the execution's journal before any wait
/// (`run.retry.not-before`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotBefore {
    pub at: Instant,
    pub attempts: u32,
}

impl NotBefore {
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a next-attempt entry serializes")
    }

    pub fn decode(bytes: &[u8]) -> Result<NotBefore, Failure> {
        serde_json::from_slice(bytes).map_err(|e| Failure::new(FailureTag::Storage, format!("a next-attempt entry does not decode: {e}")))
    }

    /// The entry `delay_ms` after `now`, closing `attempts` attempts.
    pub fn after(now: Instant, delay_ms: u64, attempts: u32) -> NotBefore {
        let nanos = now.unix_nanos().saturating_add(i128::from(delay_ms) * 1_000_000);
        let at = Instant::from_unix_nanos(nanos).unwrap_or_else(|_| now.plus_secs(delay_ms / 1000));
        NotBefore { at, attempts }
    }
}

/// The journal key of the next-attempt entry that closed attempt `attempt` of `step`.
pub fn not_before_key(step: &EntryKey, attempt: u32) -> EntryKey {
    let mut input = step.input_hash.as_bytes().to_vec();
    input.extend_from_slice(&attempt.to_be_bytes());
    EntryKey::new(&step.execution_id, &format!("{NOT_BEFORE_PREFIX}{}", step.step_label), &input)
}

/// What a step does before attempting at `now` under a next-attempt instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// The instant has passed: attempt now.
    Due,
    /// Sleep this long in process, then attempt.
    Sleep { nanos: u64 },
    /// Close the attempt waiting until the instant.
    Wait,
}

/// The hold before an attempt: a remaining wait up to [`RETRY_AFTER_CEILING_SECS`] sleeps
/// unless the caller parks every wait, and a longer one waits (`run.retry.retry-after`).
pub fn hold(at: Instant, now: Instant, park_every_wait: bool) -> Hold {
    let remaining = at.unix_nanos() - now.unix_nanos();
    if remaining <= 0 {
        return Hold::Due;
    }
    if park_every_wait || remaining > i128::from(RETRY_AFTER_CEILING_SECS) * 1_000_000_000 {
        return Hold::Wait;
    }
    Hold::Sleep { nanos: u64::try_from(remaining).unwrap_or(u64::MAX) }
}

fn step_failed(label: &str, failure: &Failure) -> RunError {
    RunError::StepFailed { label: label.to_string(), failure: failure.clone() }
}

/// One mix of a seed and an attempt into a uniform 64-bit value (splitmix64).
fn mix(seed: u64, attempt: u32) -> u64 {
    let mut z = seed.wrapping_add(u64::from(attempt).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The computed delay before attempt `attempt + 1`: the backoff clamped at the schedule's
/// maximum, plus jitter below the ceiling derived from `seed`.
pub fn delay_ms(schedule: &Schedule, attempt: u32, seed: u64) -> u64 {
    let raw = match schedule.backoff {
        Backoff::Fixed { delay_ms } => delay_ms,
        Backoff::Exponential { base_ms, factor } => {
            let exp = attempt.saturating_sub(1);
            u64::from(factor).checked_pow(exp).and_then(|m| base_ms.checked_mul(m)).unwrap_or(u64::MAX)
        }
    };
    let jitter = if schedule.jitter_ceiling_ms == 0 { 0 } else { mix(seed, attempt) % schedule.jitter_ceiling_ms };
    raw.min(schedule.max_delay_ms) + jitter
}

/// The retry decision: a pure function of the schedule, the one-based attempt that just
/// closed, the classified failure and an injected seed (`run.retry.decision-is-pure`).
pub fn decide(schedule: &Schedule, label: &str, attempt: u32, failure: &Failure, seed: u64) -> Decision {
    if failure.deterministic {
        return Decision::Fail { error: step_failed(label, failure), consumed_attempt: false };
    }
    if !schedule.retry_on.contains(&failure.tag) {
        return Decision::Fail { error: step_failed(label, failure), consumed_attempt: true };
    }
    if attempt >= schedule.attempts {
        return Decision::Fail { error: step_failed(label, failure), consumed_attempt: true };
    }
    match failure.retry_after_secs {
        Some(secs) if secs > RETRY_AFTER_CEILING_SECS => {
            let closed = Failure { tag: FailureTag::RateLimited, ..failure.clone() };
            Decision::Reschedule { error: step_failed(label, &closed), retry_after_secs: secs }
        }
        Some(secs) => Decision::Retry { delay_ms: secs * 1000 },
        None => Decision::Retry { delay_ms: delay_ms(schedule, attempt, seed) },
    }
}
