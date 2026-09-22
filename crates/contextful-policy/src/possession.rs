//! `authority.possession`.
//!
//! A credential's confirmation claim (`cnf.jkt`) holds the RFC 7638 thumbprint of the
//! client's Ed25519 public key. Each request carries a proof: a compact JWS whose
//! protected header embeds that public key as an OKP JWK and whose payload names the
//! request method (`htm`), target (`htu`), body digest (`bd`, base64url SHA-256), issue
//! instant (`iat`, Unix seconds) and nonce (`jti`). The signing input is
//! `base64url(header) "." base64url(payload)`, as in RFC 7515.

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use contextful_core::ports::Clock;
use contextful_core::time::Instant;
use contextful_core::AuthorityError;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};

/// Oldest proof issue instant a checkpoint admits, and nonce retention (`authority.verify.replay-window`).
pub const PROOF_REPLAY_WINDOW_SECS: u64 = 300;

/// Furthest a proof issue instant runs ahead of the checkpoint clock (`authority.verify.clock-skew`).
pub const PROOF_CLOCK_SKEW_SECS: u64 = 30;

/// Nonces one checkpoint retains (`authority.verify.nonce-cache`).
pub const NONCE_CACHE_ENTRIES: usize = 100_000;

/// The JWS `typ` of a request proof.
const PROOF_TYP: &str = "dpop+jwt";

/// The request a proof covers, as the checkpoint received it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProofRequest<'a> {
    pub method: &'a str,
    pub target: &'a str,
    pub body: &'a [u8],
}

/// Why a proof admits nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProofRefusal {
    /// `PossessionProofInvalid` or `PossessionProofReplayed`.
    #[error(transparent)]
    Refused(#[from] AuthorityError),
    /// The nonce cache is full: the proof cannot be recorded, so its replay could not
    /// be detected (`authority.verify.nonce-cache`).
    #[error("the nonce cache holds its bound of {NONCE_CACHE_ENTRIES} entries; the proof is not admitted")]
    NonceCacheFull,
}

impl ProofRefusal {
    /// The status a surface answers when the refusal fixes one: `503` for a full nonce
    /// cache. An authority refusal's status belongs to the surface.
    pub fn http_status(&self) -> Option<u16> {
        match self {
            ProofRefusal::NonceCacheFull => Some(503),
            ProofRefusal::Refused(_) => None,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Jwk {
    kty: String,
    crv: String,
    x: String,
}

#[derive(Serialize, Deserialize)]
struct Header {
    typ: String,
    alg: String,
    jwk: Jwk,
}

#[derive(Serialize, Deserialize)]
struct Claims {
    htm: String,
    htu: String,
    bd: String,
    iat: i64,
    jti: String,
}

/// The RFC 7638 thumbprint of an Ed25519 public key: base64url SHA-256 over the
/// canonical OKP JWK `{"crv":"Ed25519","kty":"OKP","x":"…"}`.
pub fn jwk_thumbprint(public_key: &[u8; 32]) -> String {
    let canonical = format!("{{\"crv\":\"Ed25519\",\"kty\":\"OKP\",\"x\":\"{}\"}}", B64.encode(public_key));
    B64.encode(Sha256::digest(canonical.as_bytes()))
}

/// The body digest a proof signs: base64url SHA-256 of the request body.
pub fn body_digest(body: &[u8]) -> String {
    B64.encode(Sha256::digest(body))
}

/// A client's proof for `request`, issued at `iat` under `nonce`.
pub fn sign_proof(key: &SigningKey, request: &ProofRequest<'_>, iat: Instant, nonce: &str) -> String {
    let header = Header {
        typ: PROOF_TYP.into(),
        alg: "EdDSA".into(),
        jwk: Jwk { kty: "OKP".into(), crv: "Ed25519".into(), x: B64.encode(key.verifying_key().as_bytes()) },
    };
    let claims = Claims {
        htm: request.method.into(),
        htu: request.target.into(),
        bd: body_digest(request.body),
        iat: iat.unix_secs(),
        jti: nonce.into(),
    };
    let input = format!(
        "{}.{}",
        B64.encode(serde_json::to_vec(&header).expect("a header serializes")),
        B64.encode(serde_json::to_vec(&claims).expect("claims serialize"))
    );
    let sig = key.sign(input.as_bytes());
    format!("{input}.{}", B64.encode(sig.to_bytes()))
}

fn invalid(why: impl std::fmt::Display) -> ProofRefusal {
    ProofRefusal::Refused(AuthorityError::PossessionProofInvalid(why.to_string()))
}

fn decode_json<T: for<'de> Deserialize<'de>>(part: &str, what: &str) -> Result<T, ProofRefusal> {
    let bytes = B64.decode(part).map_err(|e| invalid(format!("the proof {what} is not base64url: {e}")))?;
    serde_json::from_slice(&bytes).map_err(|e| invalid(format!("the proof {what} is malformed: {e}")))
}

/// Admit `proof` for `request` against the credential's confirmation thumbprint
/// `cnf_jkt`, recording its nonce in `nonces`.
///
/// Checks run signature first, then the issue instant, then the nonce; a refused proof
/// records nothing.
pub fn verify_proof(
    cnf_jkt: &str,
    proof: &str,
    request: &ProofRequest<'_>,
    clock: &impl Clock,
    nonces: &mut NonceCache,
) -> Result<(), ProofRefusal> {
    let parts: Vec<&str> = proof.split('.').collect();
    let [header_b64, claims_b64, sig_b64] = parts[..] else {
        return Err(invalid("a proof is three dot-separated parts"));
    };

    let header: Header = decode_json(header_b64, "header")?;
    if header.typ != PROOF_TYP || header.alg != "EdDSA" || header.jwk.kty != "OKP" || header.jwk.crv != "Ed25519" {
        return Err(invalid("a proof is an EdDSA JWS over an Ed25519 OKP key"));
    }
    let x: [u8; 32] = B64
        .decode(&header.jwk.x)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| invalid("the proof key is not a 32-byte Ed25519 public key"))?;
    if jwk_thumbprint(&x) != cnf_jkt {
        return Err(invalid("the proof key's thumbprint differs from the credential's confirmation thumbprint"));
    }
    let key = VerifyingKey::from_bytes(&x).map_err(|e| invalid(format!("the proof key is not a curve point: {e}")))?;
    let sig: [u8; 64] = B64
        .decode(sig_b64)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| invalid("the proof signature is not a 64-byte Ed25519 signature"))?;
    let signing_input = &proof[..header_b64.len() + 1 + claims_b64.len()];
    key.verify_strict(signing_input.as_bytes(), &Signature::from_bytes(&sig))
        .map_err(|_| invalid("the proof signature does not verify under the confirmed key"))?;

    let claims: Claims = decode_json(claims_b64, "payload")?;
    if claims.htm != request.method {
        return Err(invalid(format!("the proof covers method {}, the request is {}", claims.htm, request.method)));
    }
    if claims.htu != request.target {
        return Err(invalid(format!("the proof covers target {}, the request is {}", claims.htu, request.target)));
    }
    if claims.bd != body_digest(request.body) {
        return Err(invalid("the proof's body digest differs from the request body's"));
    }

    let iat = Instant::from_unix_secs(claims.iat).map_err(|e| invalid(format!("the proof issue instant: {e}")))?;
    let now = clock.now();
    if iat.secs_until(now) > PROOF_REPLAY_WINDOW_SECS {
        return Err(invalid(format!(
            "the proof was issued at {iat}, more than {PROOF_REPLAY_WINDOW_SECS} s before {now}"
        )));
    }
    if now.secs_until(iat) > PROOF_CLOCK_SKEW_SECS {
        return Err(invalid(format!("the proof was issued at {iat}, more than {PROOF_CLOCK_SKEW_SECS} s after {now}")));
    }

    nonces.record(&claims.jti, iat.plus_secs(PROOF_REPLAY_WINDOW_SECS), now)
}

/// A checkpoint's local nonce cache: each admitted proof's nonce, retained until its
/// proof leaves the replay window, under the [`NONCE_CACHE_ENTRIES`] bound.
#[derive(Debug, Clone)]
pub struct NonceCache {
    capacity: usize,
    expiry: HashMap<String, Instant>,
    by_expiry: BTreeSet<(Instant, String)>,
}

impl Default for NonceCache {
    fn default() -> Self {
        NonceCache::new()
    }
}

impl NonceCache {
    /// A cache holding up to [`NONCE_CACHE_ENTRIES`] nonces.
    pub fn new() -> NonceCache {
        NonceCache::with_capacity(NONCE_CACHE_ENTRIES)
    }

    /// A cache holding up to `capacity` nonces, clamped to [`NONCE_CACHE_ENTRIES`].
    pub fn with_capacity(capacity: usize) -> NonceCache {
        NonceCache { capacity: capacity.min(NONCE_CACHE_ENTRIES), expiry: HashMap::new(), by_expiry: BTreeSet::new() }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.expiry.len()
    }

    pub fn is_empty(&self) -> bool {
        self.expiry.is_empty()
    }

    /// Forget every nonce whose retention ends before `now`.
    pub fn prune(&mut self, now: Instant) {
        while let Some((until, _)) = self.by_expiry.first() {
            if *until >= now {
                break;
            }
            let (_, nonce) = self.by_expiry.pop_first().expect("a first entry exists");
            self.expiry.remove(&nonce);
        }
    }

    /// Record `nonce` until `until`, refusing a repeat inside its retention and any new
    /// nonce while the cache is full.
    fn record(&mut self, nonce: &str, until: Instant, now: Instant) -> Result<(), ProofRefusal> {
        self.prune(now);
        if self.expiry.contains_key(nonce) {
            return Err(ProofRefusal::Refused(AuthorityError::PossessionProofReplayed(format!(
                "nonce `{nonce}` repeats inside the {PROOF_REPLAY_WINDOW_SECS} s replay window"
            ))));
        }
        if self.expiry.len() >= self.capacity {
            return Err(ProofRefusal::NonceCacheFull);
        }
        self.expiry.insert(nonce.to_owned(), until);
        self.by_expiry.insert((until, nonce.to_owned()));
        Ok(())
    }
}
