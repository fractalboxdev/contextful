//! The signed artifact a tenant purge returns (`disclosure.receipt`): a salted tenant
//! pseudonym, the coverage its version declares, and a digest that names it once.

use crate::control_receipt::normalized_public_key;
use crate::issue::{sign_through, SignerKey};
use contextful_core::disclosure::erase::ErasureError;
use contextful_core::ports::SigningPort;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// The one receipt layout this engine signs.
pub const RECEIPT_VERSION: u32 = 1;
const DOMAIN: &[u8] = b"contextful-tenant-receipt-v1\n";

/// What a receipt claims was rewritten, and what it names as outside the claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    pub covered: Vec<String>,
    pub excluded: Vec<String>,
}

impl Coverage {
    /// The coverage a receipt version declares; version 1 covers the canonical store and
    /// excludes replicas and the replication bucket.
    pub fn of_version(version: u32) -> Option<Coverage> {
        (version == 1).then(|| Coverage {
            covered: vec!["canonical-store".into()],
            excluded: vec!["replicas".into(), "replication-bucket".into()],
        })
    }

    fn within(&self, version: u32) -> Result<(), ErasureError> {
        let declared = Coverage::of_version(version)
            .ok_or_else(|| ErasureError::ReceiptClaimWidened(format!("receipt version {version} declares no coverage")))?;
        if let Some(scope) = self.covered.iter().find(|scope| !declared.covered.contains(scope)) {
            return Err(ErasureError::ReceiptClaimWidened(format!("receipt version {version} does not cover `{scope}`")));
        }
        if let Some(scope) = declared.excluded.iter().find(|scope| !self.excluded.contains(scope)) {
            return Err(ErasureError::ReceiptClaimWidened(format!("receipt version {version} excludes `{scope}`, which the block omits")));
        }
        Ok(())
    }
}

/// The purge outcome a receipt signs, before its salt and tenant pseudonym join it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptClaim {
    pub request_id: String,
    pub transaction_id: String,
    pub store_id: String,
    pub affected_counts: BTreeMap<String, usize>,
    pub coverage: Coverage,
    pub issued_at: String,
    /// The latest earlier purge of the same tenant, when the audit chain records one.
    pub prior_request_id: Option<String>,
}

/// A signed tenant purge receipt; its JSON form is the artifact a purge returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TenantReceipt {
    pub receipt_version: u32,
    pub request_id: String,
    pub transaction_id: String,
    pub store_id: String,
    pub salt: String,
    pub tenant_hash: String,
    pub affected_counts: BTreeMap<String, usize>,
    pub coverage: Coverage,
    pub issued_at: String,
    /// Absent on a tenant's first receipt, so a receipt signed before the field verifies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_request_id: Option<String>,
    pub signer: String,
    pub signature: String,
}

impl TenantReceipt {
    /// `sha256:` and the hex SHA-256 over the salt bytes followed by the identifier bytes.
    pub fn tenant_hash(salt: &[u8], tenant: &str) -> String {
        let mut digest = Sha256::new();
        digest.update(salt);
        digest.update(tenant.as_bytes());
        format!("sha256:{}", hex::encode(digest.finalize()))
    }

    /// Refuse a claim before any rewrite runs: a coverage block outside its version, or
    /// an identifier equal to a field the claim already fixes, refuses as signing would.
    pub fn screen(claim: &ReceiptClaim, tenant: &str, signer: &dyn SigningPort) -> Result<(), ErasureError> {
        Self::draft(claim.clone(), &[0u8; 32], tenant, signer).map(|_| ())
    }

    /// Sign a fresh receipt under a new random 32-byte salt. A coverage block outside its
    /// version, or an identifier in cleartext, refuses before the port signs.
    pub fn sign(claim: ReceiptClaim, tenant: &str, signer: &dyn SigningPort) -> Result<Self, ErasureError> {
        let mut salt = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        let mut receipt = Self::draft(claim, &salt, tenant, signer)?;
        let signature = sign_through(signer, &receipt.message())
            .map_err(|error| ErasureError::ErasureTransactionIncomplete(format!("the receipt did not sign: {error}")))?;
        receipt.signature = hex::encode(signature);
        Ok(receipt)
    }

    fn draft(claim: ReceiptClaim, salt: &[u8; 32], tenant: &str, signer: &dyn SigningPort) -> Result<Self, ErasureError> {
        claim.coverage.within(RECEIPT_VERSION)?;
        let receipt = TenantReceipt {
            receipt_version: RECEIPT_VERSION,
            request_id: claim.request_id,
            transaction_id: claim.transaction_id,
            store_id: claim.store_id,
            salt: hex::encode(salt),
            tenant_hash: Self::tenant_hash(salt, tenant),
            affected_counts: claim.affected_counts,
            coverage: claim.coverage,
            issued_at: claim.issued_at,
            prior_request_id: claim.prior_request_id,
            signer: SignerKey::of(signer).to_string(),
            signature: String::new(),
        };
        if receipt.carries(tenant) {
            return Err(ErasureError::ReceiptIdentifierLeak);
        }
        Ok(receipt)
    }

    /// Whether a field value or an affected table name equals the identifier. Every field
    /// is engine-formatted, so a value merely containing the identifier carries nothing.
    fn carries(&self, tenant: &str) -> bool {
        [&self.request_id, &self.transaction_id, &self.store_id, &self.salt, &self.tenant_hash, &self.issued_at, &self.signer]
            .into_iter()
            .chain(self.prior_request_id.iter())
            .chain(self.affected_counts.keys())
            .chain(self.coverage.covered.iter())
            .chain(self.coverage.excluded.iter())
            .any(|value| value == tenant)
    }

    /// The domain-separated canonical JSON of every field but the signature.
    pub fn message(&self) -> Vec<u8> {
        let mut unsigned = self.clone();
        unsigned.signature = String::new();
        let mut message = DOMAIN.to_vec();
        message.extend(serde_json_canonicalizer::to_vec(&unsigned).expect("a receipt canonicalizes"));
        message
    }

    /// Verify offline: the declared version and coverage, the pseudonym recomputed from
    /// the salt and the identifier, the signature, and a locally pinned signer.
    pub fn verify(&self, tenant: &str, trusted: &[SignerKey]) -> Result<(), String> {
        self.coverage.within(self.receipt_version).map_err(|error| error.to_string())?;
        let salt = hex::decode(&self.salt).map_err(|error| format!("receipt salt: {error}"))?;
        if salt.len() != 32 {
            return Err("receipt salt is not 32 bytes".into());
        }
        if Self::tenant_hash(&salt, tenant) != self.tenant_hash {
            return Err("the tenant hash does not recompute for this identifier".into());
        }
        let key: SignerKey = self.signer.parse().map_err(|error| format!("receipt signer: {error}"))?;
        let signature = hex::decode(&self.signature).map_err(|error| format!("receipt signature: {error}"))?;
        if !key.verifies(&self.message(), &signature) {
            return Err("receipt signature does not verify".into());
        }
        let signer = normalized_public_key(&key).expect("a parsed receipt signer names a valid key");
        if !trusted.iter().any(|pin| pin.algorithm == key.algorithm && normalized_public_key(pin).as_deref() == Some(signer.as_slice())) {
            return Err("receipt signer is not locally pinned".into());
        }
        Ok(())
    }

    /// The hex SHA-256 over the receipt's canonical JSON, signature included.
    pub fn digest(&self) -> String {
        hex::encode(Sha256::digest(serde_json_canonicalizer::to_vec(self).expect("a receipt canonicalizes")))
    }

    /// Refuse an artifact whose digest an earlier receipt already recorded.
    pub fn check_fresh(&self, issued: &[String]) -> Result<(), ErasureError> {
        let digest = self.digest();
        if issued.contains(&digest) {
            return Err(ErasureError::ReceiptReplayed);
        }
        Ok(())
    }
}
