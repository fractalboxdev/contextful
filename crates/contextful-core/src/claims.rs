//! The authority block's content, as the profile maps it (`spec/50-authority.md` Shapes).

use crate::grant::Grant;
use crate::identify::{Attestation, Member, NormalizedSubject, Subject};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The first block of a credential: issuer, audience, identifiers, timestamps, the
/// confirmation key, the subject with its attestations, the grants and the revocation
/// identity. Timestamps are Unix seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthorityBlock {
    pub iss: String,
    pub aud: String,
    pub jti: String,
    pub iat: i64,
    pub exp: i64,
    pub alg: String,
    /// Absent on a bearer credential bound to no holder key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cnf: Option<Confirmation>,
    pub sub: Subject,
    pub att: BTreeMap<Member, Attestation>,
    pub grants: Vec<Grant>,
    /// The one local project this signed authority block may open as owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_project: Option<String>,
    pub rev: Revocation,
}

impl AuthorityBlock {
    /// The block's subject, normalized once before any consumer reads it.
    pub fn subject(&self) -> NormalizedSubject {
        self.sub.clone().normalize()
    }
}

/// The confirmation claim: the thumbprint of the holder's public key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Confirmation {
    pub jkt: String,
}

/// The revocation identity: this derivation's denylist key and the scoped epoch it was
/// minted under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revocation {
    pub id: String,
    pub epoch: u64,
}
