//! `authority.keyset`.
//!
//! A checkpoint verifies against issuer public keys from one of two sources, chosen per
//! configured key: static pins, or a project-published key route. The route's transport
//! is an adapter behind [`KeySetFetcher`]; this crate holds no HTTP client.
//!
//! A pin is `[<version>=]<algorithm>/<hex>`, the key in the credential library's text
//! form (`ed25519/…` or `secp256r1/…`), or a bare 64-hex Ed25519 key read as
//! `ed25519/<hex>`; an unnamed pin's version is its normalized key text. A
//! published document is `{"keys":[{"version":"…","key":"<algorithm>/<hex>"}]}` and
//! lists the current and non-retired versions.

use crate::possession::{verify_proof, NonceCache, ProofRefusal, ProofRequest};
use biscuit_auth::{Algorithm, PublicKey};
use contextful_core::issue::SignatureAlgorithm;
use contextful_core::ports::Clock;
use contextful_core::time::Instant;
use contextful_core::AuthorityError;
use serde::Deserialize;
use std::sync::{Arc, Mutex};

/// Lifetime of a fetched published key set (`authority.verify.key-set-refresh`).
pub const KEY_SET_REFRESH_INTERVAL_SECS: u64 = 300;

/// Age past which a last-known-good key set stops serving (`authority.verify.key-set-stale`).
pub const KEY_SET_STALE_CEILING_SECS: u64 = 60 * 60;

/// Least spacing between two fetches forced by a signature failure, and between a failed
/// fetch and its retry (`authority.verify.key-set-refresh`: failure-forced refreshes are
/// throttled).
pub const KEY_SET_FORCED_REFRESH_MIN_SECS: u64 = 30;

/// One issuer public key and the version it publishes under.
#[derive(Debug, Clone, PartialEq)]
pub struct IssuerKey {
    pub version: String,
    pub public_key: PublicKey,
}

impl IssuerKey {
    /// The key's signature scheme, authoritative for the credentials it verifies.
    pub fn algorithm(&self) -> SignatureAlgorithm {
        match self.public_key {
            PublicKey::Ed25519(_) => SignatureAlgorithm::Ed25519,
            PublicKey::P256(_) => SignatureAlgorithm::Es256,
        }
    }
}

/// The key forms a pin accepts, named in every refusal of an unparseable one.
const ACCEPTED_FORMS: &str = "a pin is `ed25519/<hex>`, `secp256r1/<hex>` or a bare 64-hex Ed25519 key";

/// A bare Ed25519 key's length in hex digits: 32 bytes.
const BARE_ED25519_HEX_LEN: usize = 64;

/// `text` as `<algorithm>/<hex>`: a bare 64-hex key reads as `ed25519/<hex>`
/// (`authority.verify.pin-source`); any other text returns as given.
fn normalize_key(text: &str) -> String {
    if text.len() == BARE_ED25519_HEX_LEN && text.bytes().all(|b| b.is_ascii_hexdigit()) {
        format!("ed25519/{text}")
    } else {
        text.to_owned()
    }
}

/// Parse `<algorithm>/<hex>` into a public key.
fn parse_key(text: &str) -> Result<PublicKey, String> {
    let (alg, hex) = text.split_once('/').ok_or_else(|| format!("`{text}` names no key scheme; {ACCEPTED_FORMS}"))?;
    let algorithm = match alg {
        "ed25519" => Algorithm::Ed25519,
        "secp256r1" => Algorithm::Secp256r1,
        _ => return Err(format!("`{text}`: unknown key scheme `{alg}`; {ACCEPTED_FORMS}")),
    };
    PublicKey::from_bytes_hex(hex, algorithm).map_err(|e| format!("`{text}`: {e}"))
}

/// The issuer keys a checkpoint verifies against, in source order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KeySet {
    keys: Vec<IssuerKey>,
}

impl KeySet {
    /// A set of `keys`, refusing an empty set or a repeated version.
    fn new(keys: Vec<IssuerKey>) -> Result<KeySet, String> {
        if keys.is_empty() {
            return Err("the set holds no key".into());
        }
        for (i, k) in keys.iter().enumerate() {
            if keys[..i].iter().any(|o| o.version == k.version) {
                return Err(format!("version `{}` appears twice", k.version));
            }
        }
        Ok(KeySet { keys })
    }

    pub fn get(&self, version: &str) -> Option<&IssuerKey> {
        self.keys.iter().find(|k| k.version == version)
    }

    pub fn keys(&self) -> impl Iterator<Item = &IssuerKey> {
        self.keys.iter()
    }

    pub fn versions(&self) -> impl Iterator<Item = &str> {
        self.keys.iter().map(|k| k.version.as_str())
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// Where a checkpoint's issuer keys come from.
pub trait KeySource: Send + Sync {
    /// The set to verify against now.
    fn keys(&self) -> Result<Arc<KeySet>, AuthorityError>;
    /// The set after a signature failure: a source that can refresh does so once,
    /// subject to its throttle.
    fn keys_after_signature_failure(&self) -> Result<Arc<KeySet>, AuthorityError>;
}

/// Comma-separated static pins.
#[derive(Debug, Clone)]
pub struct StaticPins(Arc<KeySet>);

impl StaticPins {
    /// Parse a pin list; an empty list or any malformed pin raises `KeySetUnavailable`.
    pub fn parse(list: &str) -> Result<StaticPins, AuthorityError> {
        let malformed = |why: String| AuthorityError::KeySetUnavailable(format!("static pins: {why}"));
        let keys = list
            .split(',')
            .map(|entry| {
                let entry = entry.trim();
                if entry.is_empty() {
                    return Err(malformed("an empty pin".into()));
                }
                let (version, key) = match entry.split_once('=') {
                    Some((v, k)) if !v.trim().is_empty() => (Some(v.trim()), normalize_key(k.trim())),
                    Some(_) => return Err(malformed(format!("`{entry}` names an empty version"))),
                    None => (None, normalize_key(entry)),
                };
                let public_key = parse_key(&key).map_err(malformed)?;
                Ok(IssuerKey { version: version.map_or_else(|| key.clone(), str::to_owned), public_key })
            })
            .collect::<Result<Vec<_>, _>>()?;
        KeySet::new(keys).map(|s| StaticPins(Arc::new(s))).map_err(malformed)
    }

    /// The pins whose key text, `<algorithm>/<hex>`, `keep` admits; a retired key drops
    /// from the set (`authority.revoke.immediate-retire`), and none left raises
    /// `KeySetUnavailable`.
    pub fn retaining(&self, keep: impl Fn(&str) -> bool) -> Result<StaticPins, AuthorityError> {
        let keys = self.0.keys().filter(|k| keep(&k.public_key.to_string())).cloned().collect();
        KeySet::new(keys)
            .map(|s| StaticPins(Arc::new(s)))
            .map_err(|why| AuthorityError::KeySetUnavailable(format!("static pins: every pinned key is retired: {why}")))
    }
}

impl KeySource for StaticPins {
    fn keys(&self) -> Result<Arc<KeySet>, AuthorityError> {
        Ok(self.0.clone())
    }

    fn keys_after_signature_failure(&self) -> Result<Arc<KeySet>, AuthorityError> {
        Ok(self.0.clone())
    }
}

/// The transport to a project-published, unauthenticated key route. An adapter
/// implements it; it returns the response body or why none arrived.
pub trait KeySetFetcher: Send + Sync {
    fn fetch(&self) -> Result<Vec<u8>, String>;
}

impl<T: KeySetFetcher + ?Sized> KeySetFetcher for Arc<T> {
    fn fetch(&self) -> Result<Vec<u8>, String> {
        (**self).fetch()
    }
}

#[derive(Deserialize)]
struct PublishedDocument {
    keys: Vec<PublishedEntry>,
}

#[derive(Deserialize)]
struct PublishedEntry {
    version: String,
    key: String,
}

fn parse_document(body: &[u8]) -> Result<KeySet, String> {
    let doc: PublishedDocument =
        serde_json::from_slice(body).map_err(|e| format!("the key route answered a malformed document: {e}"))?;
    let keys = doc
        .keys
        .into_iter()
        .map(|e| parse_key(&e.key).map(|public_key| IssuerKey { version: e.version, public_key }))
        .collect::<Result<Vec<_>, _>>()?;
    KeySet::new(keys).map_err(|e| format!("the key route's document: {e}"))
}

#[derive(Default)]
struct PublishedState {
    /// The last successfully fetched set and when it arrived.
    good: Option<(Arc<KeySet>, Instant)>,
    /// When the last fetch failed, while no success has followed it.
    failed_at: Option<Instant>,
    /// When a signature failure last forced a fetch.
    forced_at: Option<Instant>,
}

/// A published key set: fetched through `F`, refreshed every
/// [`KEY_SET_REFRESH_INTERVAL_SECS`], single-flight, and once more on a signature
/// failure at most every [`KEY_SET_FORCED_REFRESH_MIN_SECS`].
///
/// Single-flight: the state lock is held across a fetch, so concurrent callers finding
/// the set expired wait for the one fetch in flight and read its result.
pub struct PublishedKeySet<F, C> {
    fetcher: F,
    clock: C,
    state: Mutex<PublishedState>,
}

impl<F: KeySetFetcher, C: Clock> PublishedKeySet<F, C> {
    pub fn new(fetcher: F, clock: C) -> Self {
        PublishedKeySet { fetcher, clock, state: Mutex::new(PublishedState::default()) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PublishedState> {
        // A panic mid-fetch leaves the state consistent: every field is written whole.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn throttled(since: Option<Instant>, now: Instant) -> bool {
        since.is_some_and(|t| t.secs_until(now) < KEY_SET_FORCED_REFRESH_MIN_SECS)
    }

    /// Fetch now; on failure fall back to the last-known-good set.
    fn refresh(&self, state: &mut PublishedState, now: Instant) -> Result<Arc<KeySet>, AuthorityError> {
        match self.fetcher.fetch().and_then(|body| parse_document(&body)) {
            Ok(set) => {
                let set = Arc::new(set);
                state.good = Some((set.clone(), now));
                state.failed_at = None;
                Ok(set)
            }
            Err(why) => {
                state.failed_at = Some(now);
                Self::last_known_good(state, now, &why)
            }
        }
    }

    /// The last-known-good set while its age stays under [`KEY_SET_STALE_CEILING_SECS`].
    fn last_known_good(state: &PublishedState, now: Instant, why: &str) -> Result<Arc<KeySet>, AuthorityError> {
        match &state.good {
            Some((set, at)) if at.secs_until(now) < KEY_SET_STALE_CEILING_SECS => Ok(set.clone()),
            Some((_, at)) => Err(AuthorityError::KeySetStale(format!(
                "the last successful fetch was at {at}, {KEY_SET_STALE_CEILING_SECS} s or more before {now}: {why}"
            ))),
            None => Err(AuthorityError::KeySetUnavailable(format!("no published key set obtained: {why}"))),
        }
    }
}

impl<F: KeySetFetcher, C: Clock + Send + Sync> KeySource for PublishedKeySet<F, C> {
    fn keys(&self) -> Result<Arc<KeySet>, AuthorityError> {
        let mut state = self.lock();
        let now = self.clock.now();
        if let Some((set, at)) = &state.good {
            if at.secs_until(now) < KEY_SET_REFRESH_INTERVAL_SECS {
                return Ok(set.clone());
            }
        }
        if Self::throttled(state.failed_at, now) {
            return Self::last_known_good(&state, now, "the last fetch failed; retry is throttled");
        }
        self.refresh(&mut state, now)
    }

    fn keys_after_signature_failure(&self) -> Result<Arc<KeySet>, AuthorityError> {
        let mut state = self.lock();
        let now = self.clock.now();
        if Self::throttled(state.forced_at, now) || Self::throttled(state.failed_at, now) {
            drop(state);
            return self.keys();
        }
        state.forced_at = Some(now);
        self.refresh(&mut state, now)
    }
}

/// A checkpoint's verifying state: an issuer key source, a clock and the local nonce
/// cache. It holds public key material only — no signing key, no issuer, identity
/// provider or policy-service client — so admission calls nothing outside the process;
/// a published source calls its key route on refresh alone.
pub struct KeyCheckpoint<C> {
    source: Box<dyn KeySource>,
    clock: C,
    nonces: Mutex<NonceCache>,
}

impl<C: Clock> KeyCheckpoint<C> {
    /// Obtain the key set before accepting any request; a checkpoint without one does
    /// not start (`KeySetUnavailable`).
    pub fn start(source: Box<dyn KeySource>, clock: C) -> Result<KeyCheckpoint<C>, AuthorityError> {
        source.keys()?;
        Ok(KeyCheckpoint { source, clock, nonces: Mutex::new(NonceCache::new()) })
    }

    /// The set to verify against now.
    pub fn keys(&self) -> Result<Arc<KeySet>, AuthorityError> {
        self.source.keys()
    }

    /// Run a signature check against the current set; on `SignatureInvalid` run it once
    /// more against the set after one (throttled) refresh.
    pub fn verify_with<T>(&self, check: impl Fn(&KeySet) -> Result<T, AuthorityError>) -> Result<T, AuthorityError> {
        match check(&*self.source.keys()?) {
            Err(AuthorityError::SignatureInvalid(_)) => check(&*self.source.keys_after_signature_failure()?),
            other => other,
        }
    }

    /// Admit a request's possession proof against the credential's confirmation
    /// thumbprint, under this checkpoint's clock and nonce cache.
    pub fn verify_request(&self, cnf_jkt: &str, proof: &str, request: &ProofRequest<'_>) -> Result<(), ProofRefusal> {
        let mut nonces = self.nonces.lock().unwrap_or_else(|e| e.into_inner());
        verify_proof(cnf_jkt, proof, request, &self.clock, &mut nonces)
    }
}

impl<C: Clock> crate::possession::ProofVerifier for KeyCheckpoint<C> {
    fn verify_request(&self, cnf_jkt: &str, proof: &str, request: &ProofRequest<'_>) -> Result<(), ProofRefusal> {
        KeyCheckpoint::verify_request(self, cnf_jkt, proof, request)
    }
}
