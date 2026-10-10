//! `run.suspend`: an awakeable's row, its deadline, and the single-valued resolution
//! against an injected instant. The core reads no wall clock.

use super::journal::{sha256_hex, Stored};
use super::RunError;
use crate::time::Instant;
use serde::{Deserialize, Serialize};

/// An awakeable's state. `resolved` and `timed_out` never change again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwakeableState {
    Pending,
    Resolved,
    TimedOut,
}

/// One registry row, persisted through an awakeable store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Awakeable {
    pub token: String,
    /// The execution and step awaiting it.
    pub execution_id: String,
    pub step_label: String,
    pub created_at: Instant,
    pub ttl_secs: u64,
    pub deadline: Instant,
    pub state: AwakeableState,
    /// The recorded payload's digest, compared on a repeat resolution.
    #[serde(default)]
    pub payload_sha256: Option<String>,
    /// Where the recorded payload lives; a blob above the inline cutoff.
    #[serde(default)]
    pub payload: Option<Stored>,
    /// The verified caller subject bound at mint; a bound token resolves only for a
    /// credential of that subject, and an unbound token is its whole authority
    /// (`run.suspend.caller-binding`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<String>,
}

/// Decode a caller-supplied instant: RFC 3339 in UTC, ending `Z`.
pub fn parse_zulu(s: &str) -> Result<Instant, RunError> {
    if !s.ends_with('Z') {
        return Err(RunError::Invalid(format!("`{s}` is not an RFC 3339 instant ending `Z`")));
    }
    Instant::parse(s).map_err(|e| RunError::Invalid(e.to_string()))
}

impl Awakeable {
    /// A pending row whose deadline is the creation instant plus the time-to-live
    /// (`run.suspend.deadline`).
    pub fn mint(token: &str, execution_id: &str, step_label: &str, created_at: &str, ttl_secs: u64) -> Result<Awakeable, RunError> {
        let created = parse_zulu(created_at)?;
        Ok(Awakeable {
            token: token.to_string(),
            execution_id: execution_id.to_string(),
            step_label: step_label.to_string(),
            created_at: created,
            ttl_secs,
            deadline: created.plus_secs(ttl_secs),
            state: AwakeableState::Pending,
            payload_sha256: None,
            payload: None,
            caller: None,
        })
    }

    /// The row bound to the verified caller `subject` (`run.suspend.caller-binding`).
    pub fn bound_to(self, subject: &str) -> Awakeable {
        Awakeable { caller: Some(subject.to_string()), ..self }
    }

    /// Whether `caller`, the subject a verified credential names or `None` for a caller
    /// presenting none, reaches this row. Any other caller answers as
    /// [`unknown`], disclosing nothing (`run.suspend.caller-binding`).
    pub fn answers(&self, caller: Option<&str>) -> bool {
        self.caller.as_deref().is_none_or(|bound| caller == Some(bound))
    }

    /// The state at `now`: a pending row past its deadline reads `timed_out`, and the
    /// transition sticks under any later instant (`run.suspend.timeout-is-sticky`).
    pub fn evaluate(&mut self, now: Instant) -> AwakeableState {
        if self.state == AwakeableState::Pending && now >= self.deadline {
            self.state = AwakeableState::TimedOut;
        }
        self.state
    }

    /// Resolve with `payload` at `now`. The first resolution records it; the identical
    /// payload again returns [`Resolution::Recorded`]; a different one refuses and
    /// leaves the recorded value untouched; a resolution past the deadline refuses.
    pub fn resolve(&mut self, payload: &[u8], now: Instant) -> Result<Resolution, RunError> {
        let digest = sha256_hex(payload);
        match self.evaluate(now) {
            AwakeableState::TimedOut => Err(RunError::AwakeableTimedOut(format!(
                "awakeable `{}` passed its deadline {} before this resolution at {now}",
                self.token, self.deadline
            ))),
            AwakeableState::Resolved if self.payload_sha256.as_deref() == Some(digest.as_str()) => Ok(Resolution::Recorded),
            AwakeableState::Resolved => Err(RunError::AwakeableAlreadyResolved(format!(
                "awakeable `{}` resolved earlier with another payload; the recorded value stands",
                self.token
            ))),
            AwakeableState::Pending => {
                self.state = AwakeableState::Resolved;
                self.payload_sha256 = Some(digest);
                self.payload = Some(Stored::place(payload));
                Ok(Resolution::First)
            }
        }
    }
}

/// How a successful resolution landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// This call recorded the payload.
    First,
    /// The identical payload was recorded before; the recorded value answers.
    Recorded,
}

/// A registry lookup that found no row (`run.suspend.unknown-token`).
pub fn unknown(token: &str) -> RunError {
    RunError::AwakeableUnknown(format!("no awakeable holds token `{token}`"))
}
