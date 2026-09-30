//! The decision module's credential case: a network checkpoint's admission of one
//! credential against pinned keys, decided from one case text.

use crate::support::*;
use contextful_core::decide::{Decision, CASE_MALFORMED};
use contextful_core::grant::Action;
use contextful_core::issue::{IssuancePolicy, Lifetime, MintContext, MintRequest, NodeRole};
use contextful_core::ports::FixedClock;
use contextful_policy::decide::decide;
use contextful_policy::issue::{mint, mint_seeded, MintClaims, SeedSigner};
use contextful_policy::verify::introspect;
use serde_json::{json, Value};

const MINTED_SECS: i64 = 1_893_456_000;
const DURING_SECS: i64 = MINTED_SECS + 300;

fn decided(case: &Value) -> Decision {
    decide(case.to_string().as_bytes())
}

fn refused(error: &str) -> Decision {
    Decision::refused(error, None)
}

fn verdict(v: &str) -> Decision {
    Decision { verdict: v.into(), error: None, dimension: None, zone: None }
}

fn case(credential: &str, signer: &SeedSigner) -> Value {
    json!({"op": "verify", "credential": credential, "keys": [signer.public_key_text()], "audience": AUD, "at": DURING_SECS})
}

/// A credential living `lifetime` seconds from [`MINTED`].
fn lasting(signer: &SeedSigner, lifetime: u64) -> String {
    let policy = IssuancePolicy::parse(&format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 86400\n")).unwrap();
    let mut req = MintRequest::custody(dana(), vec![grant(&[Action::Read], &["research/*"])]);
    req.lifetime = Lifetime::Requested(lifetime);
    let clock = FixedClock(at(MINTED));
    let plan = policy.check(&req, &MintContext { node: NodeRole::Primary, signer, clock: &clock }).unwrap();
    mint(&plan, &MintClaims::default(), signer).unwrap()
}

#[test]
fn a_verification_case_admits_a_bearer_signed_by_a_pinned_key_for_the_declared_audience() {
    let signer = issuer();
    let credential = minted(&signer);
    assert_eq!(decided(&case(&credential, &signer)), verdict("admitted"));

    // A second pinned key under its own version leaves the admission standing.
    let other = issuer();
    let mut two = case(&credential, &signer);
    two["keys"] = json!([format!("old={}", other.public_key_text()), format!("k2={}", signer.public_key_text())]);
    assert_eq!(decided(&two), verdict("admitted"));
}

#[test]
fn a_verification_case_naming_an_action_and_tables_decides_coverage_of_the_admitted_grants() {
    let signer = issuer();
    let credential = minted(&signer);
    let mut read = case(&credential, &signer);
    read["action"] = json!("read");
    read["tables"] = json!(["research/filings", "research/notes"]);
    assert_eq!(decided(&read), verdict("admitted"));
    let mut outside = read.clone();
    outside["tables"] = json!(["research/filings", "sales/invoices"]);
    assert_eq!(decided(&outside), verdict("not_covered"));
    let mut write = read.clone();
    write["action"] = json!("write");
    assert_eq!(decided(&write), verdict("not_covered"));
    let mut unknown = read.clone();
    unknown["action"] = json!("delete");
    assert_eq!(decided(&unknown), refused("GrantActionUnknown"));
    let mut untabled = read;
    untabled.as_object_mut().unwrap().remove("tables");
    assert_eq!(decided(&untabled), refused(CASE_MALFORMED));
}

#[test]
fn a_verification_case_refuses_as_the_checkpoint_does() {
    let signer = issuer();
    let credential = minted(&signer);

    let mut foreign = case(&credential, &signer);
    foreign["keys"] = json!([issuer().public_key_text()]);
    assert_eq!(decided(&foreign), refused("SignatureInvalid"));

    let mut tampered = case(&credential, &signer);
    let mut text = credential.clone().into_bytes();
    let mid = text.len() / 2;
    text[mid] = if text[mid] == b'A' { b'B' } else { b'A' };
    tampered["credential"] = json!(String::from_utf8(text).unwrap());
    assert_eq!(decided(&tampered), refused("SignatureInvalid"));

    let mut elsewhere = case(&credential, &signer);
    elsewhere["audience"] = json!("contextful://another-project");
    assert_eq!(decided(&elsewhere), refused("AudienceMismatch"));

    let mut undeclared = case(&credential, &signer);
    undeclared.as_object_mut().unwrap().remove("audience");
    assert_eq!(decided(&undeclared), refused("AudienceMismatch"), "a network bearer needs a declared audience");

    let mut late = case(&credential, &signer);
    late["at"] = json!(MINTED_SECS + TTL as i64 + 1);
    assert_eq!(decided(&late), refused("AuthorityExpired"));

    let rev_id = introspect(&credential).unwrap().authority.rev.id;
    let mut denied = case(&credential, &signer);
    denied["denylist"] = json!([rev_id]);
    assert_eq!(decided(&denied), refused("AuthorityRevoked"));

    assert_eq!(decided(&case(&lasting(&signer, 7200), &signer)), refused("BearerLifetimeExceeded"));
    assert_eq!(decided(&case(&lasting(&signer, 3600), &signer)), verdict("admitted"));

    let bound = mint(&plan(&signer), &MintClaims { confirmation: Some("thumbprint".into()), epoch: 0 }, &signer).unwrap();
    assert_eq!(decided(&case(&bound, &signer)), refused("PossessionProofInvalid"), "the case carries no proof");

    let mut unpinned = case(&credential, &signer);
    unpinned["keys"] = json!(["rsa/00"]);
    assert_eq!(decided(&unpinned), refused("KeySetUnavailable"));
}

#[test]
fn a_verification_case_of_the_wrong_shape_is_malformed() {
    let signer = issuer();
    let credential = minted(&signer);
    for (key, value) in [("credential", json!(7)), ("keys", json!("k")), ("at", json!(-1)), ("at", json!(1.5)), ("denylist", json!([1]))] {
        let mut c = case(&credential, &signer);
        c[key] = value.clone();
        assert_eq!(decided(&c), refused(CASE_MALFORMED), "{key} = {value}");
    }
    for key in ["credential", "keys", "at"] {
        let mut c = case(&credential, &signer);
        c.as_object_mut().unwrap().remove(key);
        assert_eq!(decided(&c), refused(CASE_MALFORMED), "without {key}");
    }
    assert_eq!(decide(b"\xff"), refused(CASE_MALFORMED));
}

#[test]
fn every_other_case_reaches_the_domain_decisions() {
    for (case, want) in [
        (json!({"op": "covers_name", "pattern": "research/*", "name": "research/filings"}), verdict("covered")),
        (json!({"op": "covers_name", "pattern": "a*b", "name": "ab"}), refused("GrantPatternMalformed")),
        (json!({"op": "verify_all"}), refused(CASE_MALFORMED)),
    ] {
        assert_eq!(decided(&case), contextful_core::decide::decide_value(&case), "{case}");
        assert_eq!(decided(&case), want, "{case}");
    }
}

#[test]
fn a_seeded_mint_encodes_the_same_credential_for_the_same_seed() {
    let signer = SeedSigner::from_seed(&format!("ed25519-private/{}", "07".repeat(32))).unwrap();
    let one = mint_seeded(&plan(&signer), &MintClaims::default(), &signer, &[3; 32]).unwrap();
    let again = mint_seeded(&plan(&signer), &MintClaims::default(), &signer, &[3; 32]).unwrap();
    let other = mint_seeded(&plan(&signer), &MintClaims::default(), &signer, &[4; 32]).unwrap();
    assert_eq!(one, again);
    assert_ne!(one, other);
    assert_eq!(decided(&case(&one, &signer)), verdict("admitted"));
    assert_eq!(decided(&case(&other, &signer)), verdict("admitted"));
}
