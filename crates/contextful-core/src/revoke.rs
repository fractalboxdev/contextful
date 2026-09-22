//! Withdrawal ahead of expiry: the denylist, the scoped revocation epoch, rotation
//! grace validated against issuance, and credential-format withdrawal.

use crate::issue::{IssuancePolicy, ISSUANCE_LIFETIME_CEILING_SECS, ISSUER_KEY_ROTATION_CADENCE_SECS};
use crate::time::Instant;
use crate::AuthorityError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Denied revocation identifiers, each with the key version its credential verifies
/// under (`authority.revoke.denylist`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Denylist {
    entries: BTreeMap<String, String>,
}

impl Denylist {
    /// Deny `id`, a credential verifying under `key_version`.
    pub fn deny(&mut self, id: &str, key_version: &str) {
        self.entries.insert(id.to_string(), key_version.to_string());
    }

    pub fn contains(&self, id: &str) -> bool {
        self.entries.contains_key(id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drop every entry whose key version is not live, returning the dropped identifiers:
    /// no checkpoint verifies such a credential, so the entry guards nothing.
    pub fn age_out(&mut self, live_key_versions: &BTreeSet<String>) -> Vec<String> {
        let dropped: Vec<String> =
            self.entries.iter().filter(|(_, kv)| !live_key_versions.contains(*kv)).map(|(id, _)| id.clone()).collect();
        for id in &dropped {
            self.entries.remove(id);
        }
        dropped
    }
}

/// The slice one revocation epoch covers (`authority.revoke.epoch`). An absent tenant or
/// principal class covers every tenant or class in the project.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EpochScope {
    pub project: String,
    pub tenant: Option<String>,
    pub principal_class: Option<String>,
}

impl EpochScope {
    fn covers(&self, c: &RevocationClaims) -> bool {
        self.project == c.project
            && self.tenant.as_ref().is_none_or(|t| c.tenant.as_ref() == Some(t))
            && self.principal_class.as_ref().is_none_or(|k| *k == c.principal_class)
    }
}

/// The current epoch of every scope that has been bumped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Epochs {
    current: BTreeMap<EpochScope, u64>,
}

impl Epochs {
    /// Advance `scope`'s epoch, returning the new value.
    pub fn bump(&mut self, scope: EpochScope) -> u64 {
        let epoch = self.current.entry(scope).or_insert(0);
        *epoch += 1;
        *epoch
    }

    /// The highest current epoch among the scopes covering a credential.
    pub fn current_for(&self, c: &RevocationClaims) -> u64 {
        self.current.iter().filter(|(s, _)| s.covers(c)).map(|(_, e)| *e).max().unwrap_or(0)
    }
}

/// The revocation-relevant view of a credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevocationClaims {
    pub project: String,
    pub tenant: Option<String>,
    pub principal_class: String,
    /// One identifier per derivation, root first (`authority.revoke.revocation-id`).
    pub revocation_ids: Vec<String>,
    pub epoch: u64,
}

/// Refuse a credential any of whose derivations is denied, or whose epoch is below the
/// current scoped epoch, with `AuthorityRevoked` (`authority.revoke.revoked`).
pub fn check_revoked(c: &RevocationClaims, denylist: &Denylist, epochs: &Epochs) -> Result<(), AuthorityError> {
    if let Some(id) = c.revocation_ids.iter().find(|id| denylist.contains(id)) {
        return Err(AuthorityError::AuthorityRevoked(format!("`{id}` is on the denylist")));
    }
    let current = epochs.current_for(c);
    if c.epoch < current {
        return Err(AuthorityError::AuthorityRevoked(format!(
            "epoch {} is below the current scoped epoch {current}",
            c.epoch
        )));
    }
    Ok(())
}

/// Key rotation: a cadence, and a grace window during which the retiring key version
/// still verifies (`authority.revoke.rotation-grace`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotationPolicy {
    pub cadence_secs: u64,
    pub grace_secs: u64,
}

impl Default for RotationPolicy {
    /// The 90 d cadence, with a grace window of the 24 h issuance bound, which no legal
    /// issuance policy exceeds.
    fn default() -> Self {
        RotationPolicy { cadence_secs: ISSUER_KEY_ROTATION_CADENCE_SECS, grace_secs: ISSUANCE_LIFETIME_CEILING_SECS }
    }
}

impl RotationPolicy {
    /// The same cadence with an overridden grace window.
    pub fn with_grace(self, grace_secs: u64) -> Self {
        RotationPolicy { grace_secs, ..self }
    }

    /// Refuse a grace window shorter than the issuance policy's effective ceiling at `at`
    /// with `RotationGraceTooShort` (`authority.revoke.short-grace`).
    pub fn validate(&self, issuance: &IssuancePolicy, at: Instant) -> Result<(), AuthorityError> {
        let ceiling = issuance.effective_ceiling_secs(at);
        if self.grace_secs < ceiling {
            return Err(AuthorityError::RotationGraceTooShort(format!(
                "grace window {} s is shorter than the effective issuance ceiling {ceiling} s",
                self.grace_secs
            )));
        }
        Ok(())
    }

    /// Whether a key version retiring since `retiring_since` still verifies at `now`.
    pub fn retiring_key_verifies(&self, retiring_since: Instant, now: Instant) -> bool {
        now < retiring_since.plus_secs(self.grace_secs)
    }
}

/// Withdrawn credential formats and their cutover instants (`authority.revoke.format-withdrawn`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatWithdrawals {
    cutovers: BTreeMap<String, Instant>,
}

impl FormatWithdrawals {
    /// Withdraw `format` from `cutover` on.
    pub fn withdraw(&mut self, format: &str, cutover: Instant) {
        self.cutovers.insert(format.to_string(), cutover);
    }

    /// Restore acceptance of `format`: the one path back.
    pub fn restore(&mut self, format: &str) {
        self.cutovers.remove(format);
    }

    /// Refuse a credential in `format` at `at`, from its cutover on.
    pub fn check(&self, format: &str, at: Instant) -> Result<(), AuthorityError> {
        match self.cutovers.get(format) {
            Some(cutover) if at >= *cutover => Err(AuthorityError::CredentialFormatWithdrawn(format!(
                "format `{format}` is withdrawn since {cutover}"
            ))),
            _ => Ok(()),
        }
    }
}
