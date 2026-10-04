//! An issuer-signed, versioned receipt binds an applied control snapshot to its
//! project and predecessor (`surface.apply.receipt-message`). Its canonical JSON
//! digest identifies that receipt in the successor (`surface.apply.receipt-digest`).

use crate::issue::{sign_through, SignerKey};
use contextful_core::issue::SignatureAlgorithm;
use contextful_core::ports::SigningPort;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The receipt layout a synced apply signs and a replica verifies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlReceipt {
    pub format: u32,
    pub project: String,
    pub version: u64,
    pub parent: Option<String>,
    pub snapshot_sha256: String,
    pub signer: String,
    pub signature: String,
}

impl ControlReceipt {
    /// The domain-separated, unambiguous bytes signed by the issuer.
    pub fn message(&self) -> Vec<u8> {
        format!(
            "contextful-control-v1\n{}\n{}\n{}\n{}\n{}\n",
            self.project,
            self.version,
            self.parent.as_deref().unwrap_or("-"),
            self.snapshot_sha256,
            self.signer
        )
        .into_bytes()
    }

    /// Sign one immutable snapshot after the caller has admitted admin authority.
    pub fn sign(project: &str, version: u64, parent: Option<&str>, snapshot: &[u8], signer: &dyn SigningPort) -> Result<Self, String> {
        validate_fields(project, version, parent)?;
        let mut receipt = ControlReceipt {
            format: 1,
            project: project.to_string(),
            version,
            parent: parent.map(str::to_string),
            snapshot_sha256: hex::encode(Sha256::digest(snapshot)),
            signer: SignerKey::of(signer).to_string(),
            signature: String::new(),
        };
        receipt.signature = hex::encode(sign_through(signer, &receipt.message()).map_err(|e| e.to_string())?);
        Ok(receipt)
    }

    /// Verify the project, exact snapshot bytes, trusted issuer pin and signed fields.
    pub fn verify(&self, project: &str, snapshot: &[u8], trusted: &[SignerKey]) -> Result<(), String> {
        if self.format != 1 {
            return Err(format!("control receipt format {} is unsupported", self.format));
        }
        validate_fields(&self.project, self.version, self.parent.as_deref())?;
        if self.project != project {
            return Err(format!("control receipt project `{}` differs from `{project}`", self.project));
        }
        if self.snapshot_sha256 != hex::encode(Sha256::digest(snapshot)) {
            return Err("control snapshot digest differs from the receipt".into());
        }
        let key: SignerKey = self.signer.parse().map_err(|e| format!("control receipt signer: {e}"))?;
        let signature = hex::decode(&self.signature).map_err(|e| format!("control receipt signature: {e}"))?;
        if !key.verifies(&self.message(), &signature) {
            return Err("control receipt signature does not verify".into());
        }
        let signer_key = normalized_public_key(&key).expect("a parsed receipt signer names a valid key");
        if !trusted.iter().any(|pin| {
            pin.algorithm == key.algorithm && normalized_public_key(pin).as_deref() == Some(signer_key.as_slice())
        }) {
            return Err("control receipt signer is not locally pinned".into());
        }
        Ok(())
    }

    /// The digest a successor names as its parent, over canonical JSON bytes.
    pub fn digest(&self) -> String {
        hex::encode(Sha256::digest(serde_json_canonicalizer::to_vec(self).expect("a control receipt canonicalizes")))
    }
}

fn normalized_public_key(key: &SignerKey) -> Option<Vec<u8>> {
    match key.algorithm {
        SignatureAlgorithm::Ed25519 => Some(key.public_key.clone()),
        SignatureAlgorithm::Es256 => {
            use p256::elliptic_curve::sec1::ToEncodedPoint;
            p256::PublicKey::from_sec1_bytes(&key.public_key)
                .ok()
                .map(|point| point.to_encoded_point(true).as_bytes().to_vec())
        }
    }
}

fn validate_fields(project: &str, version: u64, parent: Option<&str>) -> Result<(), String> {
    if project.is_empty() || project.contains(['\n', '\r']) {
        return Err("control receipt project is empty or contains a line break".into());
    }
    if version == 0 || (version == 1) != parent.is_none() {
        return Err("control receipt version and predecessor disagree".into());
    }
    if parent.is_some_and(|digest| digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit())) {
        return Err("control receipt predecessor is not a SHA-256 digest".into());
    }
    Ok(())
}
