//! `authority.exchange`: verify a signed external assertion under operator-injected
//! material and hand its claims to the domain policy, which earns the mint plan.
//!
//! Verification checks the signature alone. Issuer, audience, expiry and the mapped
//! claims are judged by [`ExchangePolicy::mint_request`] at the mint clock's instant, so
//! the host's wall clock never decides a lapse. No function here opens a connection: a
//! key set is the injected document, and a header's `jku` or `x5u` is never followed.
//!
//! [`redeem`] is the transport-neutral exchange, request body bytes in and credential out,
//! and [`answer`] its HTTP form, status and JSON body out. The command line calls
//! `redeem`; a served face mounts `answer` at `POST /auth/exchange` through the binary,
//! the one package linking this module (`topology.package.exchange-optional`).

use contextful_core::exchange::{ExchangePolicy, VerifyingMaterial};
use contextful_core::issue::{IssuancePolicy, MintContext, MintPlan, MintRequest, PolicyError};
use contextful_core::time::Instant;
use contextful_core::AuthorityError;
use crate::issue::{mint, MintClaims};
use crate::possession::{proof_thumbprint, ProofRefusal, ProofRequest, ProofVerifier};
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

    /// The whole exchange through to the signed credential: the plan's mint under the
    /// signing port the context carries, bound to `holder`'s key thumbprint when one is
    /// given (`authority.exchange.holder-binding`) and a bearer otherwise
    /// (`authority.exchange.bearer-mint`).
    pub fn credential(
        &self,
        assertion: &str,
        holder: Option<&str>,
        issuance: &IssuancePolicy,
        ctx: &MintContext<'_>,
    ) -> Result<String, AuthorityError> {
        let plan = self.plan(assertion, issuance, ctx)?;
        mint(&plan, &MintClaims { confirmation: holder.map(str::to_owned), ..MintClaims::default() }, ctx.signer)
    }
}

/// The path a served face mounts the exchange at, and the target a holder proof covers.
pub const EXCHANGE_PATH: &str = "/auth/exchange";

/// Largest exchange request body the handler parses: 64 KiB (`authority.exchange.body-ceiling`).
pub const EXCHANGE_BODY_BYTES: usize = 64 * 1024;

/// One exchange answer as a served face writes it: an HTTP status and a JSON body.
#[derive(Debug, Clone, PartialEq)]
pub struct ExchangeAnswer {
    pub status: u16,
    pub body: Value,
}

impl ExchangeAnswer {
    /// The refusal body: `{"error": {"http", "identifier", "message"}}`. A signing fault
    /// withholds its detail, which describes the face's keys (`authority.exchange.signing-fault`).
    fn refused(error: &AuthorityError) -> ExchangeAnswer {
        let status = status(error);
        let text = error.to_string();
        // A refusal's `Display` begins with its identifier.
        let (identifier, _) = text.split_once(':').unwrap_or((text.as_str(), ""));
        let message = if status == 500 {
            "the face cannot sign a credential; its operator repairs the issuer configuration".to_string()
        } else {
            text.clone()
        };
        let mut detail = Map::new();
        detail.insert("http".into(), status.into());
        detail.insert("identifier".into(), identifier.into());
        detail.insert("message".into(), message.into());
        let mut body = Map::new();
        body.insert("error".into(), Value::Object(detail));
        ExchangeAnswer { status, body: Value::Object(body) }
    }
}

/// The HTTP status an exchange refusal answers (`authority.exchange.status-map`,
/// `authority.exchange.signing-fault`).
pub fn status(error: &AuthorityError) -> u16 {
    match error {
        AuthorityError::ExchangeUnconfigured(_) => 404,
        AuthorityError::ExchangeRequestMalformed(_) => 400,
        AuthorityError::ExchangeBodyTooLarge(_) => 413,
        AuthorityError::ExchangeMaterialMissing(_) => 503,
        AuthorityError::ExchangeAssertionInvalid(_) => 401,
        AuthorityError::ExchangeHolderProofInvalid(_) => 400,
        AuthorityError::IssuerKeyMissing(_)
        | AuthorityError::IssuerKeyUnresolvable(_)
        | AuthorityError::SignatureAlgorithmMismatch(_)
        | AuthorityError::ReplicaCannotIssue(_) => 500,
        _ => 403,
    }
}

/// The request body (`authority.exchange.wire`).
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ExchangeRequest {
    jwt: String,
}

/// The exchange from request body to signed credential, shared by the command line and
/// every served face: `exchange` is the project's configured exchange, `None` when it
/// declares no policy; `body` is `{"jwt": "<assertion>"}` of at most
/// [`EXCHANGE_BODY_BYTES`], refused unparsed past it; `dpop` is the request's holder
/// proof over `POST` [`EXCHANGE_PATH`] and `body`, admitted under `proofs`.
pub fn redeem(
    exchange: Option<&Exchange>,
    body: &[u8],
    dpop: Option<&str>,
    proofs: &dyn ProofVerifier,
    issuance: &IssuancePolicy,
    ctx: &MintContext<'_>,
) -> Result<String, ProofRefusal> {
    let Some(exchange) = exchange else {
        return Err(AuthorityError::ExchangeUnconfigured(format!("no exchange policy at {}", ExchangePolicy::PATH)).into());
    };
    if body.len() > EXCHANGE_BODY_BYTES {
        return Err(AuthorityError::ExchangeBodyTooLarge(format!(
            "the request body holds {} B, past {EXCHANGE_BODY_BYTES} B",
            body.len()
        ))
        .into());
    }
    let request: ExchangeRequest = serde_json::from_slice(body).map_err(|e| {
        AuthorityError::ExchangeRequestMalformed(format!("the request body is not {{\"jwt\": \"<assertion>\"}}: {e}"))
    })?;
    let holder = dpop.map(|proof| holder_key(proof, body, proofs)).transpose()?;
    Ok(exchange.credential(&request.jwt, holder.as_deref(), issuance, ctx)?)
}

/// The thumbprint of the key `proof` embeds, once the proof verifies under it over the
/// exchange request (`authority.exchange.holder-proof-invalid`). A full nonce cache
/// stays [`ProofRefusal::NonceCacheFull`].
fn holder_key(proof: &str, body: &[u8], proofs: &dyn ProofVerifier) -> Result<String, ProofRefusal> {
    let covered = ProofRequest { method: "POST", target: EXCHANGE_PATH, body };
    let checked = proof_thumbprint(proof).and_then(|jkt| proofs.verify_request(&jkt, proof, &covered).map(|()| jkt));
    checked.map_err(|refusal| match refusal {
        ProofRefusal::Refused(e) => ProofRefusal::Refused(AuthorityError::ExchangeHolderProofInvalid(e.to_string())),
        full @ ProofRefusal::NonceCacheFull => full,
    })
}

/// The exchange handler a served face mounts at `POST` [`EXCHANGE_PATH`]: [`redeem`]
/// with its result as a status and JSON body, `{"token": "<credential>"}` on a mint. A
/// full nonce cache answers 503 (`authority.verify.nonce-cache`).
pub fn answer(
    exchange: Option<&Exchange>,
    body: &[u8],
    dpop: Option<&str>,
    proofs: &dyn ProofVerifier,
    issuance: &IssuancePolicy,
    ctx: &MintContext<'_>,
) -> ExchangeAnswer {
    match redeem(exchange, body, dpop, proofs, issuance, ctx) {
        Ok(token) => {
            let mut body = Map::new();
            body.insert("token".into(), token.into());
            ExchangeAnswer { status: 200, body: Value::Object(body) }
        }
        Err(ProofRefusal::Refused(e)) => ExchangeAnswer::refused(&e),
        Err(ProofRefusal::NonceCacheFull) => {
            let mut detail = Map::new();
            detail.insert("http".into(), 503.into());
            detail.insert("message".into(), ProofRefusal::NonceCacheFull.to_string().into());
            let mut body = Map::new();
            body.insert("error".into(), Value::Object(detail));
            ExchangeAnswer { status: 503, body: Value::Object(body) }
        }
    }
}

fn invalid(reason: String) -> AuthorityError {
    AuthorityError::ExchangeAssertionInvalid(reason)
}

fn unusable(reason: String) -> AuthorityError {
    AuthorityError::ExchangeMaterialMissing(reason)
}
