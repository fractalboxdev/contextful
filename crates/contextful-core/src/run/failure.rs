//! `run.retry.failure-taxonomy`: the one tagged failure type crossing every port.

use serde::{Deserialize, Serialize};

/// The classification a caller branches on. A message never decides a branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum FailureTag {
    Transient,
    Permanent,
    SchemaIncompatible,
    AuthExpired,
    RateLimited,
    Config,
    Storage,
    UnknownConnector,
    SecretNotFound,
    Canceled,
}

impl FailureTag {
    pub const ALL: [FailureTag; 10] = [
        FailureTag::Transient,
        FailureTag::Permanent,
        FailureTag::SchemaIncompatible,
        FailureTag::AuthExpired,
        FailureTag::RateLimited,
        FailureTag::Config,
        FailureTag::Storage,
        FailureTag::UnknownConnector,
        FailureTag::SecretNotFound,
        FailureTag::Canceled,
    ];

    /// The tag's wire spelling.
    pub fn name(self) -> &'static str {
        match self {
            FailureTag::Transient => "Transient",
            FailureTag::Permanent => "Permanent",
            FailureTag::SchemaIncompatible => "SchemaIncompatible",
            FailureTag::AuthExpired => "AuthExpired",
            FailureTag::RateLimited => "RateLimited",
            FailureTag::Config => "Config",
            FailureTag::Storage => "Storage",
            FailureTag::UnknownConnector => "UnknownConnector",
            FailureTag::SecretNotFound => "SecretNotFound",
            FailureTag::Canceled => "Canceled",
        }
    }

    /// Decode a wire spelling; anything outside the ten is `None`.
    pub fn parse(s: &str) -> Option<FailureTag> {
        FailureTag::ALL.into_iter().find(|t| t.name() == s)
    }
}

impl std::fmt::Display for FailureTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// One classified failure: its tag, a message for people, and the two facts the retry
/// decision reads besides the tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub tag: FailureTag,
    pub message: String,
    /// A server-supplied `Retry-After`, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
    /// A refusal whose check is pure over static input (`run.retry.deterministic-verdict`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deterministic: bool,
    /// The declared resource bound a connector struck, [`LINEAR_MEMORY`] or
    /// [`CALL_DEADLINE`] (`run.retry.bound-hit-is-transient`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound: Option<String>,
    /// What defeated a read of one input, which decides whether a source dead-letters it
    /// (`run.land.input-fault`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fault: Option<Fault>,
}

/// What defeated a read of one input (`run.land.input-fault`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fault {
    /// The input's own bytes: corrupt, truncated, or past a size ceiling. A source dead-letters it.
    Input,
    /// The declaration or the build: a decoder compiled out, or a declared key no input can
    /// satisfy. The read refuses whole.
    Configuration,
}

/// The bound a guest strikes on its linear-memory cap (`connector.package.linear-memory`).
pub const LINEAR_MEMORY: &str = "linear-memory";
/// The bound a guest strikes past its call deadline (`connector.package.call-deadline`).
pub const CALL_DEADLINE: &str = "call-deadline";

impl Failure {
    pub fn new(tag: FailureTag, message: impl Into<String>) -> Failure {
        Failure { tag, message: message.into(), retry_after_secs: None, deterministic: false, bound: None, fault: None }
    }

    /// A refusal decided by a pure check over static input.
    pub fn deterministic(tag: FailureTag, message: impl Into<String>) -> Failure {
        Failure { deterministic: true, ..Failure::new(tag, message) }
    }

    /// A permanent refusal over one input's own bytes (`run.land.input-fault`).
    pub fn unreadable_input(message: impl Into<String>) -> Failure {
        Failure { fault: Some(Fault::Input), ..Failure::deterministic(FailureTag::Permanent, message) }
    }

    /// A refusal the declaration or the build causes, failing the whole read
    /// (`run.land.input-fault`).
    pub fn misconfigured(message: impl Into<String>) -> Failure {
        Failure { fault: Some(Fault::Configuration), ..Failure::deterministic(FailureTag::Config, message) }
    }

    /// Whether a source dead-letters this failure in place of the input's rows.
    pub fn dead_letters(&self) -> bool {
        self.fault == Some(Fault::Input)
    }

    pub fn canceled(message: impl Into<String>) -> Failure {
        Failure::new(FailureTag::Canceled, message)
    }

    pub fn with_retry_after(mut self, secs: u64) -> Failure {
        self.retry_after_secs = Some(secs);
        self
    }

    /// This failure as a hit on the declared resource bound `bound`.
    pub fn with_bound(mut self, bound: &str) -> Failure {
        self.bound = Some(bound.to_string());
        self
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.tag, self.message)
    }
}

impl std::error::Error for Failure {}
