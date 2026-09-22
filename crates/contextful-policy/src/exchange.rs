//! `authority.exchange`: verify a signed external assertion under operator-injected
//! material and hand its claims to the domain policy, which earns the mint plan.
//!
//! Verification checks the signature alone. Issuer, audience, expiry and the mapped
//! claims are judged by [`ExchangePolicy::mint_request`] at the mint clock's instant, so
//! the host's wall clock never decides a lapse. No function here opens a connection: a
//! key set is the injected document, and a header's `jku` or `x5u` is never followed.

use contextful_core::exchange::{ExchangePolicy, VerifyingMaterial};
use contextful_core::issue::{IssuancePolicy, MintContext, MintPlan, MintRequest, PolicyError};
use contextful_core::time::Instant;
use contextful_core::AuthorityError;
use jsonwebtoken::jwk::{AlgorithmParameters, JwkSet};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde_json::{Map, Value};

/// The injected bytes at [`ExchangePolicy::VERIFY_KEY_PATH`] as verifying material: a PEM
/// block is an RS256 public key, a JSON document is a key set, anything else is a shared
/// secret. Surrounding whitespace is trimmed; bytes that are all whitespace are none.
pub fn material_from_bytes(bytes: &[u8]) -> Option<VerifyingMaterial> {
    let trimmed = bytes.trim_ascii();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        if trimmed.starts_with(b"-----BEGIN") {
            return Some(VerifyingMaterial::Rs256PublicKeyPem(text.to_string()));
        }
        if trimmed.starts_with(b"{") {
            return Some(VerifyingMaterial::KeySet(text.to_string()));
        }
    }
    Some(VerifyingMaterial::SharedSecret(trimmed.to_vec()))
}

/// Verify an assertion's signature under the injected material and return its claims.
/// A shared secret verifies HS256, a PEM key RS256, and a key set the key its `kid`
/// selects under that key's declared algorithm (RS256 when it declares none).
pub fn verify_assertion(
    material: Option<&VerifyingMaterial>,
    assertion: &str,
) -> Result<Map<String, Value>, AuthorityError> {
    let material = material.filter(|m| !m.is_empty()).ok_or_else(|| {
        AuthorityError::ExchangeMaterialMissing(format!("no verifying material at {}", ExchangePolicy::VERIFY_KEY_PATH))
    })?;
    let (key, algorithm) = match material {
        VerifyingMaterial::SharedSecret(secret) => (DecodingKey::from_secret(secret), Algorithm::HS256),
        VerifyingMaterial::Rs256PublicKeyPem(pem) => {
            let key = DecodingKey::from_rsa_pem(pem.as_bytes())
                .map_err(|e| unusable(format!("the RS256 public key holds no usable key: {e}")))?;
            (key, Algorithm::RS256)
        }
        VerifyingMaterial::KeySet(document) => key_set_entry(document, assertion)?,
    };

    // Signature and algorithm only; every claim check belongs to the domain policy.
    let mut validation = Validation::new(algorithm);
    validation.required_spec_claims.clear();
    validation.validate_exp = false;
    validation.validate_nbf = false;
    validation.validate_aud = false;
    validation.leeway = 0;
    decode::<Map<String, Value>>(assertion, &key, &validation)
        .map(|data| data.claims)
        .map_err(|e| invalid(format!("signature does not verify: {e}")))
}

/// The key a key-set document holds under the assertion's `kid`, and its algorithm.
fn key_set_entry(document: &str, assertion: &str) -> Result<(DecodingKey, Algorithm), AuthorityError> {
    let set: JwkSet =
        serde_json::from_str(document).map_err(|e| unusable(format!("the key set is no key-set document: {e}")))?;
    if set.keys.is_empty() || set.keys.iter().any(|k| matches!(k.algorithm, AlgorithmParameters::Other(_))) {
        return Err(unusable("the key set holds a key of no usable type".into()));
    }
    let header = decode_header(assertion).map_err(|e| invalid(format!("header does not decode: {e}")))?;
    let kid = header.kid.ok_or_else(|| invalid("names no `kid` to select from the key set".into()))?;
    let jwk = set.find(&kid).ok_or_else(|| invalid(format!("`kid` `{kid}` names no key in the key set")))?;
    let algorithm = match jwk.common.key_algorithm {
        None => Algorithm::RS256,
        Some(declared) => declared
            .to_string()
            .parse::<Algorithm>()
            .map_err(|_| unusable(format!("key `{kid}` declares the unsupported algorithm {declared}")))?,
    };
    let key = DecodingKey::from_jwk(jwk).map_err(|e| unusable(format!("key `{kid}` is unusable: {e}")))?;
    Ok((key, algorithm))
}

/// A project's configured exchange: the declared policy and the injected material.
#[derive(Debug, Clone)]
pub struct Exchange {
    pub policy: ExchangePolicy,
    pub material: Option<VerifyingMaterial>,
}

impl Exchange {
    /// Build from the text at [`ExchangePolicy::PATH`] and the bytes at
    /// [`ExchangePolicy::VERIFY_KEY_PATH`]; empty bytes configure no material.
    pub fn from_config(policy_text: &str, material: &[u8]) -> Result<Exchange, PolicyError> {
        Ok(Exchange { policy: ExchangePolicy::parse(policy_text)?, material: material_from_bytes(material) })
    }

    /// The mint request a verified assertion earns at `now`.
    pub fn request(&self, assertion: &str, now: Instant) -> Result<MintRequest, AuthorityError> {
        let claims = verify_assertion(self.material.as_ref(), assertion)?;
        self.policy.mint_request(self.material.as_ref(), &claims, now)
    }

    /// The whole exchange: the verified assertion's mint plan, checked against the
    /// persisted issuance policy, ready for the credential mint. Its subject is the
    /// assertion's mapped reader and no other.
    pub fn plan(
        &self,
        assertion: &str,
        issuance: &IssuancePolicy,
        ctx: &MintContext<'_>,
    ) -> Result<MintPlan, AuthorityError> {
        let claims = verify_assertion(self.material.as_ref(), assertion)?;
        self.policy.mint(self.material.as_ref(), &claims, issuance, ctx)
    }
}

fn invalid(reason: String) -> AuthorityError {
    AuthorityError::ExchangeAssertionInvalid(reason)
}

fn unusable(reason: String) -> AuthorityError {
    AuthorityError::ExchangeMaterialMissing(reason)
}
