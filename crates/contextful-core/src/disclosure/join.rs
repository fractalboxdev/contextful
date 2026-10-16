//! The cross-owner hashed join (`disclosure.set-mode.hashed-join`): each owner lands its
//! join key as an HMAC-SHA256 digest under the pair's pepper at write time, a join compares
//! digests alone, and only per-group counts of distinct matched keys leave, under the
//! suppression floor. Every join runs under a fresh pepper: the [`PepperLedger`] records
//! each pepper's fingerprint and each pair's last version, and a pepper seen before raises
//! `DisclosureStaticPepper`. A join matches only rows carrying its pepper's version
//! (`disclosure.set-mode.pepper-version`).

use super::suppress::{suppress, GroupStats, Sentinel, SuppressPolicy};
use super::DisclosureError;
use crate::surface::worker::{hex, hmac_sha256};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// The two owners a pepper serves, in sorted order, so either side names the same pair.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OwnerPair {
    first: String,
    second: String,
}

impl OwnerPair {
    pub fn new(a: &str, b: &str) -> OwnerPair {
        let (first, second) = if a <= b { (a, b) } else { (b, a) };
        OwnerPair { first: first.to_string(), second: second.to_string() }
    }

    /// The pair's ledger name, `<first>+<second>`.
    pub fn name(&self) -> String {
        format!("{}+{}", self.first, self.second)
    }
}

/// One join's pepper: a 32-byte secret escrowed for the pair under a version that
/// advances with every join.
#[derive(Clone)]
pub struct PairPepper {
    pair: OwnerPair,
    version: u64,
    secret: [u8; 32],
}

impl std::fmt::Debug for PairPepper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairPepper").field("pair", &self.pair).field("version", &self.version).finish_non_exhaustive()
    }
}

/// A join key as an owner lands it: the keyed digest and the pepper version it was hashed under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinKey {
    pub digest: String,
    pub version: u64,
}

/// One row of the owner whose groups the release counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinRow {
    pub key: JoinKey,
    pub group: String,
}

impl PairPepper {
    pub fn new(pair: OwnerPair, version: u64, secret: [u8; 32]) -> PairPepper {
        PairPepper { pair, version, secret }
    }

    pub fn pair(&self) -> &OwnerPair {
        &self.pair
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// The write-time digest of `key`, bound to the pair and the version.
    pub fn hash(&self, key: &str) -> JoinKey {
        let message = format!("contextful.disclosure.join\0{}\0{}\0{}\0{key}", self.pair.first, self.pair.second, self.version);
        JoinKey { digest: hex(&hmac_sha256(&self.secret, message.as_bytes())), version: self.version }
    }

    /// The SHA-256 fingerprint the ledger records in place of the secret.
    pub fn fingerprint(&self) -> String {
        hex(&Sha256::new().chain_update(b"contextful.disclosure.pepper\0").chain_update(self.secret).finalize())
    }
}

/// Every pepper a join has run under, by fingerprint, and each pair's last joined version,
/// keyed by the pair's first then second owner so no two pairs share a history.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PepperLedger {
    fingerprints: BTreeSet<String>,
    versions: BTreeMap<String, BTreeMap<String, u64>>,
}

impl PepperLedger {
    /// Admit `pepper` for one join: its fingerprint unseen across every pair, and its version
    /// past the pair's last join. Either failing raises `DisclosureStaticPepper`.
    pub fn admit(&mut self, pepper: &PairPepper) -> Result<(), DisclosureError> {
        let pair = pepper.pair.name();
        if self.fingerprints.contains(&pepper.fingerprint()) {
            return Err(DisclosureError::StaticPepper(format!("pair `{pair}` presents a pepper an earlier join ran under")));
        }
        let (first, second) = (&pepper.pair.first, &pepper.pair.second);
        if let Some(last) = self.versions.get(first).and_then(|m| m.get(second)).filter(|last| pepper.version <= **last) {
            return Err(DisclosureError::StaticPepper(format!(
                "pair `{pair}` presents pepper version {} at or before its last joined version {last}",
                pepper.version
            )));
        }
        self.fingerprints.insert(pepper.fingerprint());
        self.versions.entry(first.clone()).or_default().insert(second.clone(), pepper.version);
        Ok(())
    }
}

/// What a join releases: per-group counts of distinct matched keys at or over the floor,
/// in group order, and the one sentinel when any group is withheld.
#[derive(Debug, Clone, PartialEq)]
pub struct JoinRelease {
    pub groups: Vec<(String, u64)>,
    pub sentinel: Option<Sentinel>,
    pub version: u64,
}

/// Join `left`'s grouped rows to `right`'s keys under `pepper`. The floor validates first,
/// so a refused floor spends no pepper; then the ledger admits the pepper; then rows not
/// carrying the pepper's version drop out of the match.
pub fn hashed_join(
    pepper: &PairPepper,
    ledger: &mut PepperLedger,
    left: &[JoinRow],
    right: &[JoinKey],
    min_group_size: u64,
) -> Result<JoinRelease, DisclosureError> {
    let policy = SuppressPolicy::new(Some(min_group_size), None)?;
    ledger.admit(pepper)?;
    let current = |k: &JoinKey| k.version == pepper.version;
    let theirs: BTreeSet<&str> = right.iter().filter(|k| current(k)).map(|k| k.digest.as_str()).collect();
    let mut matched: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for row in left.iter().filter(|r| current(&r.key) && theirs.contains(r.key.digest.as_str())) {
        matched.entry(row.group.as_str()).or_default().insert(row.key.digest.as_str());
    }
    let groups = matched
        .into_iter()
        .map(|(group, keys)| {
            let count = keys.len() as u64;
            ((group.to_string(), count), GroupStats { noised_contributors: count, contributor_masses: None })
        })
        .collect();
    let out = suppress(&policy, groups);
    Ok(JoinRelease { groups: out.published, sentinel: out.sentinel, version: pepper.version })
}
