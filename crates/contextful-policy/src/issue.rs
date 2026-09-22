//! `authority.issue`, credential side: the seed-file signing adapter and the mint that
//! encodes a checked [`MintPlan`] as a signed authority block.
//!
//! A seed file holds the issuer private key in the library's text form
//! (`ed25519-private/<hex>` or `secp256r1-private/<hex>`); its public half prints as
//! `ed25519/<hex>` or `secp256r1/<hex>`, the static-pin form a checkpoint reads.

use crate::profile::authority_facts;
use biscuit_auth::{Algorithm, BiscuitBuilder, KeyPair, PrivateKey};
use contextful_core::claims::{AuthorityBlock, Confirmation, Revocation};
use contextful_core::issue::{MintPlan, SignatureAlgorithm};
use contextful_core::ports::SigningPort;
use contextful_core::AuthorityError;
use rand::RngCore;
use std::path::Path;

/// The command that writes a seed file, printed wherever an issuer key is missing.
pub const KEYGEN_COMMAND: &str = "contextful token keygen --out .contextful/issuer.seed";

/// An issuer key held in-process, loaded from a seed file or generated.
pub struct SeedSigner {
    key: KeyPair,
}

impl std::fmt::Debug for SeedSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeedSigner").field("public_key", &self.public_key_text()).finish_non_exhaustive()
    }
}

impl SeedSigner {
    /// A fresh key under `algorithm`.
    pub fn generate(algorithm: SignatureAlgorithm) -> SeedSigner {
        let algorithm = match algorithm {
            SignatureAlgorithm::Ed25519 => Algorithm::Ed25519,
            SignatureAlgorithm::Es256 => Algorithm::Secp256r1,
        };
        SeedSigner { key: KeyPair::new_with_algorithm(algorithm) }
    }

    /// Decode a seed file's text; anything else raises `IssuerKeyUnresolvable`.
    pub fn from_seed(text: &str) -> Result<SeedSigner, AuthorityError> {
        let private: PrivateKey = text
            .trim()
            .parse()
            .map_err(|e| AuthorityError::IssuerKeyUnresolvable(format!("the seed holds no issuer key: {e}")))?;
        Ok(SeedSigner { key: KeyPair::from(&private) })
    }

    /// Resolve a configured issuer key reference: none configured raises `IssuerKeyMissing`
    /// naming the command that writes one; a reference resolving to no material raises
    /// `IssuerKeyUnresolvable`. Nothing is written: no local key is fabricated.
    pub fn resolve(reference: Option<&Path>) -> Result<SeedSigner, AuthorityError> {
        let Some(path) = reference else {
            return Err(AuthorityError::IssuerKeyMissing(format!("no issuer key is configured; run `{KEYGEN_COMMAND}`")));
        };
        let text = std::fs::read_to_string(path).map_err(|e| {
            AuthorityError::IssuerKeyUnresolvable(format!("issuer key `{}` resolves to no material: {e}", path.display()))
        })?;
        SeedSigner::from_seed(&text)
            .map_err(|e| AuthorityError::IssuerKeyUnresolvable(format!("issuer key `{}`: {e}", path.display())))
    }

    /// The seed file's text.
    pub fn seed(&self) -> String {
        self.key.private().to_prefixed_string()
    }

    /// The public key in static-pin form, `<algorithm>/<hex>`.
    pub fn public_key_text(&self) -> String {
        self.key.public().to_string()
    }

    /// The key the library signs blocks with.
    pub fn key_pair(&self) -> &KeyPair {
        &self.key
    }
}

impl SigningPort for SeedSigner {
    fn algorithm(&self) -> SignatureAlgorithm {
        match self.key.public() {
            biscuit_auth::PublicKey::Ed25519(_) => SignatureAlgorithm::Ed25519,
            biscuit_auth::PublicKey::P256(_) => SignatureAlgorithm::Es256,
        }
    }

    fn public_key(&self) -> Vec<u8> {
        self.key.public().to_bytes()
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, AuthorityError> {
        self.key
            .sign(message)
            .map(|s| s.to_bytes().to_vec())
            .map_err(|e| AuthorityError::IssuerKeyUnresolvable(format!("the issuer key signs nothing: {e}")))
    }
}

/// The claims a mint adds beyond the plan: the holder key binding and the scoped epoch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MintClaims {
    /// The holder key thumbprint the credential binds (`authority.verify.possession-binding`).
    pub confirmation: Option<String>,
    /// The scoped revocation epoch the credential is minted under (`authority.revoke.epoch`).
    pub epoch: u64,
}

/// The authority block a plan mints: a fresh credential identifier, the subject
/// normalized with its attestations, and the revocation identity `rev://<jti>`.
pub fn authority_block(plan: &MintPlan, claims: &MintClaims) -> AuthorityBlock {
    let mut jti = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut jti);
    let jti = hex::encode(jti);
    let subject = plan.subject.clone().normalize();
    AuthorityBlock {
        iss: plan.issuer.clone().unwrap_or_else(|| plan.audience.clone()),
        aud: plan.audience.clone(),
        rev: Revocation { id: format!("rev://{jti}"), epoch: claims.epoch },
        jti,
        iat: plan.issued_at.unix_secs(),
        exp: plan.expires_at.unix_secs(),
        alg: plan.algorithm.to_string(),
        cnf: claims.confirmation.clone().map(|jkt| Confirmation { jkt }),
        att: subject.attestations(),
        sub: subject.to_subject(),
        grants: plan.grants.clone(),
    }
}

/// Mint a checked plan as a credential, signed by `signer`. The plan's scheme must be
/// the signing key's (`authority.issue.algorithm-mismatch`).
pub fn mint(plan: &MintPlan, claims: &MintClaims, signer: &SeedSigner) -> Result<String, AuthorityError> {
    let pinned = signer.algorithm();
    if plan.algorithm != pinned {
        return Err(AuthorityError::SignatureAlgorithmMismatch(format!(
            "the plan names {}; the signing key is {pinned}",
            plan.algorithm
        )));
    }
    let block = authority_block(plan, claims);
    let mut builder = BiscuitBuilder::new();
    for f in authority_facts(&block)? {
        builder = builder.fact(f).map_err(|e| AuthorityError::ProfileElementUnrecognized(e.to_string()))?;
    }
    let token = builder
        .build(signer.key_pair())
        .map_err(|e| AuthorityError::IssuerKeyUnresolvable(format!("the issuer key signs nothing: {e}")))?;
    token.to_base64().map_err(|e| AuthorityError::IssuerKeyUnresolvable(format!("the credential encodes to nothing: {e}")))
}
