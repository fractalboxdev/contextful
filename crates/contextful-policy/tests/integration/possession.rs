//! `authority.verify`: the request possession proof and the checkpoint nonce cache.

use contextful_core::claims::Confirmation;
use contextful_core::ports::FixedClock;
use contextful_core::time::Instant;
use contextful_core::AuthorityError;
use contextful_policy::possession::{
    body_digest, jwk_thumbprint, sign_proof, verify_proof, NonceCache, ProofRefusal, ProofRequest,
    NONCE_CACHE_ENTRIES, PROOF_CLOCK_SKEW_SECS, PROOF_REPLAY_WINDOW_SECS,
};
use ed25519_dalek::SigningKey;

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

fn client(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn cnf(key: &SigningKey) -> Confirmation {
    Confirmation { jkt: jwk_thumbprint(key.verifying_key().as_bytes()) }
}

const NOW: &str = "2030-01-01T00:00:00Z";

fn request() -> ProofRequest<'static> {
    ProofRequest { method: "POST", target: "https://store.example/v1/query", body: b"{\"sql\":\"select 1\"}" }
}

fn invalid(r: Result<(), ProofRefusal>) -> String {
    match r {
        Err(ProofRefusal::Refused(AuthorityError::PossessionProofInvalid(m))) => m,
        other => panic!("expected PossessionProofInvalid, got {other:?}"),
    }
}

/// A credential's confirmation claim holds a client public-key thumbprint. Each request carries a proof signed by the matching private key over method, target, body digest, issue instant and nonce.
// spec: authority.verify.possession-binding@ca4a9e8d
#[test]
fn a_proof_binds_method_target_body_instant_and_nonce_to_the_confirmation_thumbprint() {
    let key = client(7);
    // RFC 7638 thumbprint of an Ed25519 OKP key: 43 base64url characters, no padding.
    let jkt = cnf(&key).jkt;
    assert_eq!(jkt.len(), 43);
    assert!(!jkt.contains('='));
    assert_ne!(jkt, cnf(&client(8)).jkt);
    // RFC 8037 appendix A.3's key and thumbprint.
    let rfc_x: [u8; 32] = [
        0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7, 0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64, 0x07, 0x3a, 0x0e, 0xe1,
        0x72, 0xf3, 0xda, 0xa6, 0x23, 0x25, 0xaf, 0x02, 0x1a, 0x68, 0xf7, 0x07, 0x51, 0x1a,
    ];
    assert_eq!(jwk_thumbprint(&rfc_x), "kPrK_qmxVWaYVA9wwBF6Iuo3vVzz7TxHCTwXBygrS4k");
    assert_eq!(body_digest(b""), "47DEQpj8HBSa-_TImW-5JCeuQeRkm5NMpJWZG3hSuFU");

    let clock = FixedClock(at(NOW));
    let mut nonces = NonceCache::new();
    let proof = sign_proof(&key, &request(), at(NOW), "n-1");
    assert_eq!(verify_proof(&cnf(&key).jkt, &proof, &request(), &clock, &mut nonces), Ok(()));

    // Each signed element is bound: changing any one of them fails the proof.
    let other_method = ProofRequest { method: "GET", ..request() };
    let other_target = ProofRequest { target: "https://store.example/v1/admin", ..request() };
    let other_body = ProofRequest { body: b"{\"sql\":\"drop table t\"}", ..request() };
    for (i, req) in [other_method, other_target, other_body].iter().enumerate() {
        let proof = sign_proof(&key, &request(), at(NOW), &format!("m-{i}"));
        invalid(verify_proof(&cnf(&key).jkt, &proof, req, &clock, &mut nonces));
    }

    // The issue instant and nonce sit under the signature: editing the payload fails it.
    let proof = sign_proof(&key, &request(), at(NOW), "n-2");
    let mut parts: Vec<String> = proof.split('.').map(str::to_owned).collect();
    let forged = sign_proof(&key, &request(), at(NOW), "n-3");
    parts[1] = forged.split('.').nth(1).unwrap().to_owned();
    invalid(verify_proof(&cnf(&key).jkt, &parts.join("."), &request(), &clock, &mut nonces));
}

/// A request whose proof does not verify against the confirmation thumbprint raises `PossessionProofInvalid`.
// spec: authority.verify.possession-invalid@834b1ead
#[test]
fn a_proof_from_another_key_or_malformed_raises_possession_proof_invalid() {
    let holder = client(1);
    let thief = client(2);
    let clock = FixedClock(at(NOW));
    let mut nonces = NonceCache::new();

    // A validly signed proof from a key other than the confirmed one.
    let proof = sign_proof(&thief, &request(), at(NOW), "t-1");
    let msg = invalid(verify_proof(&cnf(&holder).jkt, &proof, &request(), &clock, &mut nonces));
    assert!(msg.contains("thumbprint"), "{msg}");

    // A proof carrying the holder's public key but signed by another.
    let genuine = sign_proof(&holder, &request(), at(NOW), "t-2");
    let stolen = sign_proof(&thief, &request(), at(NOW), "t-2");
    let spliced = format!(
        "{}.{}",
        genuine.rsplit_once('.').unwrap().0,
        stolen.rsplit_once('.').unwrap().1
    );
    invalid(verify_proof(&cnf(&holder).jkt, &spliced, &request(), &clock, &mut nonces));

    for garbage in ["", "a.b", "a.b.c", "!!.!!.!!", "a.b.c.d"] {
        invalid(verify_proof(&cnf(&holder).jkt, garbage, &request(), &clock, &mut nonces));
    }

    // The refusal is an authority error whose display names the identifier.
    let err = verify_proof(&cnf(&holder).jkt, &proof, &request(), &clock, &mut nonces).unwrap_err();
    assert!(err.to_string().starts_with("PossessionProofInvalid"), "{err}");
    assert_eq!(err.http_status(), None);
    // A refused proof records nothing: the holder's own later proof with the same nonce admits.
    let own = sign_proof(&holder, &request(), at(NOW), "t-1");
    assert_eq!(verify_proof(&cnf(&holder).jkt, &own, &request(), &clock, &mut nonces), Ok(()));
}

/// A proof issued more than 300 s before the checkpoint clock is refused as {{authority.verify.possession-invalid}}; the nonce cache retains each nonce for that window.
// spec: authority.verify.replay-window@65727cfc
#[test]
fn a_proof_older_than_the_replay_window_refuses_and_nonces_live_for_that_window() {
    assert_eq!(PROOF_REPLAY_WINDOW_SECS, 300);
    let key = client(3);
    let jkt = cnf(&key).jkt;
    let issued = at(NOW);
    let mut nonces = NonceCache::new();

    // Exactly 300 s old admits; 301 s old refuses.
    let edge = sign_proof(&key, &request(), issued, "w-1");
    assert_eq!(verify_proof(&jkt, &edge, &request(), &FixedClock(issued.plus_secs(300)), &mut nonces), Ok(()));
    let stale = sign_proof(&key, &request(), issued, "w-2");
    let msg = invalid(verify_proof(&jkt, &stale, &request(), &FixedClock(issued.plus_secs(301)), &mut nonces));
    assert!(msg.contains("300"), "{msg}");

    // A nonce is retained for the window after its proof's issue instant, then forgotten.
    let mut nonces = NonceCache::new();
    let proof = sign_proof(&key, &request(), issued, "w-3");
    assert_eq!(verify_proof(&jkt, &proof, &request(), &FixedClock(issued), &mut nonces), Ok(()));
    assert_eq!(nonces.len(), 1);
    nonces.prune(issued.plus_secs(PROOF_REPLAY_WINDOW_SECS));
    assert_eq!(nonces.len(), 1, "retained through the whole window");
    nonces.prune(issued.plus_secs(PROOF_REPLAY_WINDOW_SECS + 1));
    assert!(nonces.is_empty(), "forgotten once the proof itself is stale");
}

/// A proof issued more than 30 s after the checkpoint clock is refused as {{authority.verify.possession-invalid}}.
// spec: authority.verify.clock-skew@b41c8d56
#[test]
fn a_proof_issued_more_than_thirty_seconds_ahead_refuses() {
    assert_eq!(PROOF_CLOCK_SKEW_SECS, 30);
    let key = client(4);
    let jkt = cnf(&key).jkt;
    let clock = FixedClock(at(NOW));
    let mut nonces = NonceCache::new();

    let ahead_30 = sign_proof(&key, &request(), at(NOW).plus_secs(30), "s-1");
    assert_eq!(verify_proof(&jkt, &ahead_30, &request(), &clock, &mut nonces), Ok(()));
    let ahead_31 = sign_proof(&key, &request(), at(NOW).plus_secs(31), "s-2");
    let msg = invalid(verify_proof(&jkt, &ahead_31, &request(), &clock, &mut nonces));
    assert!(msg.contains("30"), "{msg}");
}

/// A proof whose nonce repeats inside the replay window raises `PossessionProofReplayed`.
// spec: authority.verify.replayed-nonce@dd870b10
#[test]
fn a_nonce_repeating_inside_the_window_raises_possession_proof_replayed() {
    let key = client(5);
    let jkt = cnf(&key).jkt;
    let issued = at(NOW);
    let mut nonces = NonceCache::new();

    let proof = sign_proof(&key, &request(), issued, "r-1");
    assert_eq!(verify_proof(&jkt, &proof, &request(), &FixedClock(issued), &mut nonces), Ok(()));

    // The captured request re-sent, and a fresh proof reusing the nonce, both refuse.
    let later = FixedClock(issued.plus_secs(120));
    match verify_proof(&jkt, &proof, &request(), &later, &mut nonces) {
        Err(ProofRefusal::Refused(AuthorityError::PossessionProofReplayed(m))) => assert!(m.contains("r-1"), "{m}"),
        other => panic!("expected PossessionProofReplayed, got {other:?}"),
    }
    let reused = sign_proof(&key, &request(), issued.plus_secs(120), "r-1");
    let err = verify_proof(&jkt, &reused, &request(), &later, &mut nonces).unwrap_err();
    assert!(err.to_string().starts_with("PossessionProofReplayed"), "{err}");

    // A distinct nonce admits.
    let fresh = sign_proof(&key, &request(), issued.plus_secs(120), "r-2");
    assert_eq!(verify_proof(&jkt, &fresh, &request(), &later, &mut nonces), Ok(()));
}

/// A checkpoint's nonce cache holds at most 100000 entries; a proof arriving while it is full answers `503` and admits nothing.
// spec: authority.verify.nonce-cache@7f58576e
#[test]
fn a_full_nonce_cache_answers_503_and_admits_nothing() {
    assert_eq!(NONCE_CACHE_ENTRIES, 100_000);
    assert_eq!(NonceCache::new().capacity(), NONCE_CACHE_ENTRIES);
    // A smaller cache is expressible; a larger one clamps to the bound.
    assert_eq!(NonceCache::with_capacity(NONCE_CACHE_ENTRIES + 1).capacity(), NONCE_CACHE_ENTRIES);

    let key = client(6);
    let jkt = cnf(&key).jkt;
    let issued = at(NOW);
    let clock = FixedClock(issued);
    let mut nonces = NonceCache::with_capacity(3);
    for i in 0..3 {
        let proof = sign_proof(&key, &request(), issued, &format!("c-{i}"));
        assert_eq!(verify_proof(&jkt, &proof, &request(), &clock, &mut nonces), Ok(()));
    }
    assert_eq!(nonces.len(), 3);

    // A valid proof arriving while the cache is full is refused and not recorded.
    let proof = sign_proof(&key, &request(), issued, "c-3");
    let err = verify_proof(&jkt, &proof, &request(), &clock, &mut nonces).unwrap_err();
    assert_eq!(err, ProofRefusal::NonceCacheFull);
    assert_eq!(err.http_status(), Some(503));
    assert_eq!(nonces.len(), 3);

    // Once the window passes for the recorded nonces, room returns.
    let later = FixedClock(issued.plus_secs(PROOF_REPLAY_WINDOW_SECS + 1));
    let proof = sign_proof(&key, &request(), issued.plus_secs(PROOF_REPLAY_WINDOW_SECS + 1), "c-3");
    assert_eq!(verify_proof(&jkt, &proof, &request(), &later, &mut nonces), Ok(()));
    assert_eq!(nonces.len(), 1);
}
