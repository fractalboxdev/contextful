//! `connector.rotate`: the write half a credential store contributes to turnover, a
//! blind versioned put conditioned on the version a run hydrated.

use super::lease::{RETIRE_CAP_SECS, RETIRE_SHARE_PERCENT};
use super::reference::{Hydrated, SecretName};
use crate::time::Instant;

/// When a credential obtained at `obtained` and expiring at `expires_at` refreshes: ahead
/// of expiry by the margin `connector.lease.retirement-margin` computes, 10 percent of
/// the window at most 30 s (`connector.rotate.oauth-credential`).
pub fn refresh_at(obtained: Instant, expires_at: Instant) -> Instant {
    let window = obtained.secs_until(expires_at);
    expires_at.minus_secs((window * RETIRE_SHARE_PERCENT / 100).min(RETIRE_CAP_SECS))
}

/// An opaque version a store assigns to a stored value.
pub type Version = String;

/// Why a store refused or failed a put.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PutError {
    /// The store refused the write (`connector.rotate.write-back-denied`).
    Denied(String),
    /// Another writer stored this version first (`connector.rotate.conditional-put`).
    Conflict(Version),
    /// The write did not complete.
    Failed(String),
}

/// A store's write half (`connector.rotate.division-of-labor`). An adapter without one —
/// the environment, the keychain, the lease provider — never receives a write-back.
pub trait VersionedStore: Send + Sync {
    /// The adapter's name, as attribution records it.
    fn adapter(&self) -> &str;
    /// Store `value` for `name` when the current version is `if_version`, with an opaque
    /// expiry hint beside it (`connector.rotate.expiry-hint`).
    fn put(&self, name: &SecretName, value: &Hydrated, if_version: Option<&str>, expires_hint: Option<Instant>) -> Result<Version, PutError>;
}
