//! `authority.issue`, credential side: the seed-file adapter and the mint.

use crate::support::*;
use contextful_core::issue::SignatureAlgorithm;
use contextful_core::ports::SigningPort;
use contextful_policy::issue::{mint, MintClaims, SeedSigner, KEYGEN_COMMAND};
use contextful_policy::keyset::{KeySource, StaticPins};
use contextful_policy::verify::introspect;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("contextful-policy-issue-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A set issuer key reference that resolves to no material raises `IssuerKeyUnresolvable`; no local key is fabricated.
// spec: authority.issue.unresolvable-key@763c7765
#[test]
fn an_issuer_key_reference_resolving_to_nothing_is_refused_and_fabricates_no_key() {
    let dir = scratch("unresolvable");
    let absent = dir.join("issuer.seed");
    refused(SeedSigner::resolve(Some(&absent)), "IssuerKeyUnresolvable");
    assert!(!absent.exists(), "no key is written in its place");
    let garbage = dir.join("garbage.seed");
    std::fs::write(&garbage, "not a key\n").unwrap();
    refused(SeedSigner::resolve(Some(&garbage)), "IssuerKeyUnresolvable");
    assert_eq!(std::fs::read_to_string(&garbage).unwrap(), "not a key\n");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
}

#[test]
fn no_configured_issuer_key_names_the_command_that_writes_one() {
    let err = SeedSigner::resolve(None).unwrap_err();
    assert_eq!(err_name(&err), "IssuerKeyMissing");
    assert!(err.to_string().contains(KEYGEN_COMMAND), "{err}");
}

#[test]
fn a_seed_file_round_trips_and_its_public_key_is_a_static_pin() {
    for algorithm in [SignatureAlgorithm::Ed25519, SignatureAlgorithm::Es256] {
        let signer = SeedSigner::generate(algorithm);
        let dir = scratch(&format!("seed-{algorithm}"));
        let path = dir.join("issuer.seed");
        std::fs::write(&path, format!("{}\n", signer.seed())).unwrap();
        let loaded = SeedSigner::resolve(Some(&path)).unwrap();
        assert_eq!(loaded.public_key_text(), signer.public_key_text());
        assert_eq!(loaded.algorithm(), algorithm);
        let pins = StaticPins::parse(&signer.public_key_text()).unwrap();
        assert_eq!(pins.keys().unwrap().keys().next().unwrap().algorithm(), algorithm);
        // A credential minted under either scheme admits under its pin.
        let credential = mint(&plan(&loaded), &MintClaims::default(), &loaded).unwrap();
        assert_eq!(introspect(&credential).unwrap().authority.alg, algorithm.to_string());
        assert!(admit(&credential, &signer, DURING).is_ok());
    }
}

#[test]
fn a_mint_carries_the_plan_the_policy_checked() {
    let signer = issuer();
    let p = plan(&signer);
    let credential = mint(&p, &MintClaims { confirmation: Some("jkt-1".into()), epoch: 7 }, &signer).unwrap();
    let block = introspect(&credential).unwrap().authority;
    assert_eq!(block.aud, AUD);
    assert_eq!(block.iss, AUD);
    assert_eq!(block.iat, at(MINTED).unix_secs());
    assert_eq!(block.exp, at(MINTED).unix_secs() + TTL as i64);
    assert_eq!(block.sub, dana());
    assert_eq!(block.att, p.subject.clone().normalize().attestations());
    assert_eq!(block.grants, p.grants);
    assert_eq!(block.cnf.unwrap().jkt, "jkt-1");
    assert_eq!(block.rev.epoch, 7);
    assert_eq!(block.rev.id, format!("rev://{}", block.jti));
    // Every mint names a fresh credential identifier.
    assert_ne!(introspect(&mint(&p, &MintClaims::default(), &signer).unwrap()).unwrap().authority.jti, block.jti);
}

#[test]
fn a_plan_naming_another_scheme_than_the_signing_key_mints_nothing() {
    let signer = issuer();
    let mut p = plan(&signer);
    p.algorithm = SignatureAlgorithm::Es256;
    refused(mint(&p, &MintClaims::default(), &signer), "SignatureAlgorithmMismatch");
}
