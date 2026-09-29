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

/// One scope's current epoch as the key-set ledger persists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpochEntry {
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal_class: Option<String>,
    pub epoch: u64,
}

/// One issuer key version's life as the key-set ledger persists it: the instant it began
/// signing, and the instant it began retiring under a grace window, or retired at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyVersion {
    /// The public key in static-pin form, `<algorithm>/<hex>`.
    pub key: String,
    pub since: Instant,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retiring_since: Option<Instant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<Instant>,
}

/// The project's key-set ledger, persisted beside the issuer seed at
/// [`KeySetLedger::PATH`]: each issuer key version's life and each scope's current
/// revocation epoch (`authority.revoke.epoch-store`). A mint stamps the epoch it reads
/// here; a checkpoint reads the file with no service call.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeySetLedger {
    #[serde(default, rename = "key", skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<KeyVersion>,
    #[serde(default, rename = "epoch", skip_serializing_if = "Vec::is_empty")]
    pub epochs: Vec<EpochEntry>,
}

impl KeySetLedger {
    /// Where a project persists its ledger, beside `.contextful/issuer.seed`.
    pub const PATH: &'static str = ".contextful/keyset.toml";

    /// Decode a ledger file; malformed text names the file.
    pub fn parse(text: &str, origin: &str) -> Result<KeySetLedger, String> {
        toml::from_str(text).map_err(|e| format!("{origin}: {}", e.message()))
    }

    /// The ledger file's text.
    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("a key-set ledger serializes")
    }

    /// The current epoch of every recorded scope.
    pub fn current_epochs(&self) -> Epochs {
        let mut epochs = Epochs::default();
        for e in &self.epochs {
            let scope = EpochScope { project: e.project.clone(), tenant: e.tenant.clone(), principal_class: e.principal_class.clone() };
            let slot = epochs.current.entry(scope).or_insert(0);
            *slot = (*slot).max(e.epoch);
        }
        epochs
    }

    /// Advance `scope`'s epoch, returning the new value.
    pub fn bump(&mut self, scope: EpochScope) -> u64 {
        let found = self
            .epochs
            .iter_mut()
            .find(|e| e.project == scope.project && e.tenant == scope.tenant && e.principal_class == scope.principal_class);
        match found {
            Some(e) => {
                e.epoch += 1;
                e.epoch
            }
            None => {
                self.epochs.push(EpochEntry { project: scope.project, tenant: scope.tenant, principal_class: scope.principal_class, epoch: 1 });
                1
            }
        }
    }

    /// The recorded version of `key`, if any.
    pub fn version(&self, key: &str) -> Option<&KeyVersion> {
        self.keys.iter().find(|k| k.key == key)
    }

    /// Record `key` as signing from `since`.
    pub fn record(&mut self, key: &str, since: Instant) {
        if self.version(key).is_none() {
            self.keys.push(KeyVersion { key: key.to_string(), since, retiring_since: None, grace_secs: None, retired_at: None });
        }
    }

    /// Begin retiring `key` at `at` under `rotation`'s grace window.
    pub fn retire_after_grace(&mut self, key: &str, at: Instant, rotation: &RotationPolicy) {
        self.record(key, at);
        if let Some(k) = self.keys.iter_mut().find(|k| k.key == key) {
            k.retiring_since = Some(at);
            k.grace_secs = Some(rotation.grace_secs);
        }
    }

    /// Retire `key` at once (`authority.revoke.immediate-retire`).
    pub fn retire_now(&mut self, key: &str, at: Instant) {
        self.record(key, at);
        if let Some(k) = self.keys.iter_mut().find(|k| k.key == key) {
            k.retired_at = Some(at);
        }
    }

    /// Whether a checkpoint still verifies under `key` at `now`: an unrecorded key does,
    /// a retired one does not, and a retiring one does until its grace window lapses.
    pub fn verifies(&self, key: &str, now: Instant) -> bool {
        match self.version(key) {
            None => true,
            Some(k) if k.retired_at.is_some_and(|at| now >= at) => false,
            Some(k) => match (k.retiring_since, k.grace_secs) {
                (Some(since), Some(grace)) => RotationPolicy::default().with_grace(grace).retiring_key_verifies(since, now),
                _ => true,
            },
        }
    }
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
