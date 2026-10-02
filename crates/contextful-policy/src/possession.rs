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

/// The method a proof presented over a local transport covers; its target is the verb's
/// command path and its body is empty (`authority.verify.local-proof-request`).
pub const LOCAL_PROOF_METHOD: &str = "CLI";

/// The request a local verb's proof covers: [`LOCAL_PROOF_METHOD`], the command path
/// `verb` as target, and an empty body.
pub fn local_request(verb: &str) -> ProofRequest<'_> {
    ProofRequest { method: LOCAL_PROOF_METHOD, target: verb, body: b"" }
}

/// The text prefix of a holder seed, the form `token keygen` writes an Ed25519 seed in.
const HOLDER_SEED_PREFIX: &str = "ed25519-private/";

/// A holder's Ed25519 signing key, read from a seed file: `ed25519-private/<64 hex>` or
/// bare 64 hex (`authority.verify.local-proof-channel`).
pub struct HolderKey(SigningKey);

impl std::fmt::Debug for HolderKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("HolderKey").field(&self.thumbprint()).finish()
    }
}

impl HolderKey {
    /// A fresh holder key.
    pub fn generate() -> HolderKey {
        HolderKey(SigningKey::from_bytes(&rand::random::<[u8; 32]>()))
    }

    /// The key a seed text names. A malformed seed yields no proof, so it refuses as
    /// `PossessionProofInvalid`.
    pub fn from_seed(text: &str) -> Result<HolderKey, AuthorityError> {
        let text = text.trim();
        let hex_text = text.strip_prefix(HOLDER_SEED_PREFIX).unwrap_or(text);
        let seed: [u8; 32] = hex::decode(hex_text).ok().and_then(|b| b.try_into().ok()).ok_or_else(|| {
            AuthorityError::PossessionProofInvalid(format!(
                "a holder seed is `{HOLDER_SEED_PREFIX}<64 hex>` or 64 hex characters naming an Ed25519 seed"
            ))
        })?;
        Ok(HolderKey(SigningKey::from_bytes(&seed)))
    }

    /// The seed text [`HolderKey::from_seed`] reads back.
    pub fn seed(&self) -> String {
        format!("{HOLDER_SEED_PREFIX}{}", hex::encode(self.0.to_bytes()))
    }

    /// The RFC 7638 thumbprint a credential binds as its confirmation claim.
    pub fn thumbprint(&self) -> String {
        jwk_thumbprint(self.0.verifying_key().as_bytes())
    }

    /// A proof for `request` issued at `iat` under a fresh random nonce.
    pub fn prove(&self, request: &ProofRequest<'_>, iat: Instant) -> String {
        sign_proof(&self.0, request, iat, &B64.encode(rand::random::<[u8; 16]>()))
    }
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

/// The RFC 7638 thumbprint of the key a proof's header embeds; the proof is not verified.
pub fn proof_thumbprint(proof: &str) -> Result<String, ProofRefusal> {
    let header_b64 = proof.split('.').next().unwrap_or_default();
    let header: Header = decode_json(header_b64, "header")?;
    let x: [u8; 32] = B64
        .decode(&header.jwk.x)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| invalid("the proof key is not a 32-byte Ed25519 public key"))?;
    Ok(jwk_thumbprint(&x))
}

/// Admits a request proof against a confirmation thumbprint under one clock and nonce cache.
pub trait ProofVerifier {
    fn verify_request(&self, cnf_jkt: &str, proof: &str, request: &ProofRequest<'_>) -> Result<(), ProofRefusal>;
}

/// A [`ProofVerifier`] owning its clock and nonce cache: the command line's one-shot
/// verifier, and a served face's where no key checkpoint holds the cache.
pub struct ProofChecker<C> {
    clock: C,
    nonces: std::sync::Mutex<NonceCache>,
}

impl<C: Clock> ProofChecker<C> {
    pub fn new(clock: C) -> ProofChecker<C> {
        ProofChecker { clock, nonces: std::sync::Mutex::new(NonceCache::new()) }
    }
}

impl<C: Clock> ProofVerifier for ProofChecker<C> {
    fn verify_request(&self, cnf_jkt: &str, proof: &str, request: &ProofRequest<'_>) -> Result<(), ProofRefusal> {
        let mut nonces = self.nonces.lock().unwrap_or_else(|e| e.into_inner());
        verify_proof(cnf_jkt, proof, request, &self.clock, &mut nonces)
    }
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

    /// A full-bound cache holding `entries`, each a nonce and the instant its retention
    /// ends, less those ending before `now`: the cache a process rebuilds from a store that
    /// outlives it (`authority.verify.local-nonce-store`).
    pub fn restored(entries: impl IntoIterator<Item = (String, Instant)>, now: Instant) -> NonceCache {
        let mut cache = NonceCache::new();
        for (nonce, until) in entries {
            if until >= now && !cache.expiry.contains_key(&nonce) {
                cache.by_expiry.insert((until, nonce.clone()));
                cache.expiry.insert(nonce, until);
            }
        }
        cache
    }

    /// Each retained nonce and the instant its retention ends.
    pub fn entries(&self) -> impl Iterator<Item = (&str, Instant)> {
        self.by_expiry.iter().map(|(until, nonce)| (nonce.as_str(), *until))
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
