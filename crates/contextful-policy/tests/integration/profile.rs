//! `authority.profile`: the delegation profile's admitted elements, reserved facts,
//! evaluator bound, restrictions, introspection and the scoped session.

use crate::support::*;
use contextful_core::grant::{Action, AggregateGrant};
use contextful_core::issue::SignatureAlgorithm;
use contextful_policy::attenuate::{attenuate, Derivation};
use contextful_policy::issue::{mint, MintClaims};
use contextful_policy::profile::{authority_facts, ENGINE_FACTS, EVALUATOR_FACT_CEILING, PROFILE_VERSION, SUPPORTED_PROFILE_VERSIONS};
use contextful_policy::revoke::RevocationState;
use contextful_policy::verify::{introspect, verify, Admission};

/// Delegated authority travels in one attenuable, chain-signed library format. The library owns serialization, signatures, block chaining and evaluation; a versioned profile names every fact, check and restriction the engine admits.
// spec: authority.profile.delegation-profile@8fba5b66
#[test]
fn a_credential_is_one_library_chain_whose_every_block_the_profile_reads() {
    let signer = issuer();
    let parent = minted(&signer);
    // The library's own format: its unverified reader parses the credential and names
    // the chain's blocks.
    let token = biscuit_auth::Biscuit::from_base64(&parent, keys(&signer).keys().next().unwrap().public_key).unwrap();
    assert_eq!(token.block_count(), 1);
    let child = attenuate(&parent, &Derivation::narrowing(&[grant(&[Action::Read], &["research/*"])], None, None)).unwrap();
    let token = biscuit_auth::UnverifiedBiscuit::from_base64(&child).unwrap();
    assert_eq!(token.block_count(), 2);
    let seen = introspect(&child).unwrap();
    assert_eq!(seen.profile, PROFILE_VERSION);
    assert_eq!(seen.blocks.len(), 1);
    assert_eq!(seen.authority.aud, AUD);
    assert!(admit(&child, &signer, DURING).is_ok());
}

/// A credential carrying a block version, predicate, rule or restriction the profile does not name raises `ProfileElementUnrecognized`.
// spec: authority.profile.unrecognized-element@36e553a7
#[test]
fn an_element_the_profile_does_not_name_is_refused() {
    let signer = issuer();
    let b = block(&signer);
    // A predicate.
    refused(admit(&craft(&signer, &b, &[], "colour(\"blue\");"), &signer, DURING), "ProfileElementUnrecognized");
    // A rule.
    refused(admit(&craft(&signer, &b, &[], "owner($x) <- iss($x);"), &signer, DURING), "ProfileElementUnrecognized");
    // A check.
    refused(admit(&craft(&signer, &b, &[], "check if iss($x);"), &signer, DURING), "ProfileElementUnrecognized");
    // An authority-only claim in an attenuation block.
    refused(admit(&append_raw(&minted(&signer), "aud(\"contextful://other\");"), &signer, DURING), "ProfileElementUnrecognized");
    // A block version: a fact holding an array term moves the block past version 3.
    refused(admit(&append_raw(&minted(&signer), "exp([1, 2]);"), &signer, DURING), "ProfileElementUnrecognized");
    // A grant field.
    refused(
        admit(
            &append_raw(&minted(&signer), r#"grant(0, "{\"actions\":[\"read\"],\"tables\":[\"research/*\"],\"colour\":1}");"#),
            &signer,
            DURING,
        ),
        "ProfileElementUnrecognized",
    );
}

/// Current time, audience, resolved resources and authenticated request identity are reserved facts the engine supplies. A token block introducing one raises `ProfileReservedFact`.
// spec: authority.profile.reserved-fact@20e8c2bb
#[test]
fn a_token_block_introducing_a_reserved_fact_is_refused() {
    let signer = issuer();
    let b = block(&signer);
    for fact in ["time(2030-01-01T00:00:00Z);", "audience(\"x\");", "resource(\"research/filings\");", "request(\"dana\");"] {
        refused(admit(&craft(&signer, &b, &[], fact), &signer, DURING), "ProfileReservedFact");
        refused(admit(&append_raw(&minted(&signer), fact), &signer, DURING), "ProfileReservedFact");
    }
    // A rule deriving one is refused as introducing it.
    refused(admit(&append_raw(&minted(&signer), "time($t) <- exp($t);"), &signer, DURING), "ProfileReservedFact");
}

/// The evaluator runs with no third-party block, external function, recursion or regular-expression predicate. Input past {{authority.profile.fact-ceiling}} or {{authority.profile.iteration-ceiling}} raises `ProfileEvaluationBudget`.
// spec: authority.profile.evaluator-bound@c0f2bd1e
#[test]
fn the_evaluator_admits_no_third_party_block_rule_or_regex_and_refuses_input_past_its_ceiling() {
    let signer = issuer();
    let b = block(&signer);
    // Recursion and regular expressions need a rule or a check, neither of which a block holds.
    refused(admit(&craft(&signer, &b, &[], "check if iss($x), $x.matches(\"^c\");"), &signer, DURING), "ProfileElementUnrecognized");
    refused(admit(&append_raw(&minted(&signer), "r($x) <- r($x);"), &signer, DURING), "ProfileElementUnrecognized");
    // A third-party block.
    let third = biscuit_auth::KeyPair::new();
    let token = biscuit_auth::UnverifiedBiscuit::from_base64(minted(&signer)).unwrap();
    let request = token.third_party_request().unwrap();
    let block = request.create_block(&third.private(), biscuit_auth::BlockBuilder::new()).unwrap();
    let with_third = token.append_third_party(&block.serialize().unwrap()).unwrap().to_base64().unwrap();
    refused(admit(&with_third, &signer, DURING), "ProfileElementUnrecognized");
    // Input past the fact ceiling.
    refused(admit(&append_raw(&minted(&signer), &grants(EVALUATOR_FACT_CEILING as usize)), &signer, DURING), "ProfileEvaluationBudget");
}

/// One authorization holds at most 1000 entries in its fact set.
// spec: authority.profile.fact-ceiling@96c27502
#[test]
fn one_authorization_holds_at_most_1000_facts() {
    assert_eq!(EVALUATOR_FACT_CEILING, 1000);
    let signer = issuer();
    let parent = minted(&signer);
    let authority = authority_facts(&block(&signer)).unwrap().len();
    // The engine supplies one fact, the current time; an appended block fills the rest
    // with grants each lying within the parent's.
    let room = EVALUATOR_FACT_CEILING as usize - authority - ENGINE_FACTS;
    assert_eq!(ENGINE_FACTS, 1);
    assert!(admit(&append_raw(&parent, &grants(room)), &signer, DURING).is_ok());
    refused(admit(&append_raw(&parent, &grants(room + 1)), &signer, DURING), "ProfileEvaluationBudget");
}

/// `n` copies of the parent's grant, as one block's facts.
fn grants(n: usize) -> String {
    (0..n).map(|i| format!(r#"grant({i}, "{{\"actions\":[\"read\"],\"tables\":[\"research/*\"]}}");"#)).collect()
}

/// A block after the first contributes no authority fact to an allow decision; appending narrows a credential or adds nothing.
// spec: authority.profile.appended-block@892baf15
#[test]
fn an_appended_block_narrows_or_adds_nothing() {
    let signer = issuer();
    let parent = minted(&signer);
    // A block proposing nothing admits the parent's authority unchanged.
    let empty = append_raw(&parent, "sub(\"task\", \"task://q4\");");
    let admitted = admit(&empty, &signer, DURING).unwrap();
    assert_eq!(admitted.grants(), admit(&parent, &signer, DURING).unwrap().grants());
    // A block claiming a broader grant, appended past the holder's check, contributes
    // nothing: admission rechecks the chain and refuses it.
    let wider = append_raw(&parent, r#"grant(0, "{\"actions\":[\"read\",\"write\"],\"tables\":[\"*\"]}");"#);
    refused(admit(&wider, &signer, DURING), "AttenuationWidens");
    // A block extending expiry adds no lifetime.
    let longer = append_raw(&parent, "exp(1900000000);");
    refused(admit(&longer, &signer, DURING), "AttenuationExpiryExtended");
}

/// A grant carrying a row restriction or aggregate bound with no read evaluator raises `ProfileRestrictionUnevaluated` at the mint, at derivation on parent and child, and at admission.
// spec: authority.profile.unevaluated-restriction@2c55e6d0
#[test]
fn a_restriction_with_no_read_evaluator_is_refused_at_mint_derivation_and_admission() {
    let signer = issuer();
    let mut aggregated = grant(&[Action::Read], &["research/*"]);
    aggregated.aggregate = Some(AggregateGrant {
        min_group_size: 5,
        max_contributor_share: 0.5,
        functions: vec!["count".into()],
        max_groups: 10,
        max_rows: None,
    });
    // At the mint.
    let plan = plan_for(&signer, dana(), vec![aggregated.clone()]);
    refused(mint(&plan, &MintClaims::default(), &signer), "ProfileRestrictionUnevaluated");
    // At derivation, on the child.
    let parent = minted(&signer);
    let child = Derivation { grants: Some(vec![aggregated]), ..Derivation::default() };
    refused(attenuate(&parent, &child), "ProfileRestrictionUnevaluated");
    // At derivation, on the parent, and at admission.
    let b = block(&signer);
    let restricted = craft(
        &signer,
        &b,
        &["grant"],
        r#"grant(0, "{\"actions\":[\"read\"],\"tables\":[\"research/*\"],\"row_restriction\":{\"region\":\"eu\"}}");"#,
    );
    refused(attenuate(&restricted, &Derivation::default()), "ProfileRestrictionUnevaluated");
    refused(admit(&restricted, &signer, DURING), "ProfileRestrictionUnevaluated");
}

/// A restriction field the engine refuses stays declared in the profile and is parsed at every admission.
// spec: authority.profile.declared-field@bf442977
#[test]
fn a_refused_restriction_field_stays_declared_and_parsed() {
    let signer = issuer();
    let b = block(&signer);
    let with = |field: &str| {
        craft(&signer, &b, &["grant"], &format!(r#"grant(0, "{{\"actions\":[\"read\"],\"tables\":[\"research/*\"],{field}}}");"#))
    };
    // A declared field is recognized and refused for want of an evaluator.
    refused(admit(&with(r#"\"row_restriction\":{\"region\":\"eu\"}"#), &signer, DURING), "ProfileRestrictionUnevaluated");
    refused(
        admit(&with(r#"\"aggregate\":{\"min_group_size\":5,\"max_contributor_share\":0.5,\"functions\":[],\"max_groups\":1}"#), &signer, DURING),
        "ProfileRestrictionUnevaluated",
    );
    // An undeclared field is an element the profile does not name.
    refused(admit(&with(r#"\"row_filter\":{\"region\":\"eu\"}"#), &signer, DURING), "ProfileElementUnrecognized");
}

/// Introspection reports the scope a credential declares without evaluating it. Table policy, source access lists, time bounds and revocation settle effective authority afterwards.
// spec: authority.profile.declared-scope@7a4c1afc
#[test]
fn introspection_reports_the_declared_scope_without_evaluating_it() {
    let signer = issuer();
    let parent = minted(&signer);
    let child = attenuate(&parent, &Derivation::narrowing(&[grant(&[Action::Read], &["research/*"])], None, Some(&[contextful_core::grant::TablePattern::parse("research/filings").unwrap()]))).unwrap();
    let seen = introspect(&child).unwrap();
    // Declared scope: the authority block's grants and each block's proposal.
    assert_eq!(seen.authority.grants, vec![grant(&[Action::Read], &["research/*"])]);
    assert_eq!(seen.blocks[0].grants, Some(vec![grant(&[Action::Read], &["research/filings"])]));
    assert_eq!(seen.revocation_ids.len(), 2);
    assert_eq!(seen.rev_id, seen.revocation_ids[1]);
    let json = seen.to_json();
    assert!(json.contains(&format!("\"rev_id\": \"{}\"", seen.rev_id)), "{json}");
    // No evaluation: an expired, denylisted credential still introspects, while
    // admission refuses it.
    assert!(introspect(&child).is_ok());
    refused(admit(&child, &signer, "2031-01-01T00:00:00Z"), "AuthorityExpired");
    refused(admit_denying(&child, &signer, &[&seen.rev_id]), "AuthorityRevoked");
    // No verification either: a key nobody pins changes nothing about what it declares.
    let stranger = contextful_policy::issue::SeedSigner::generate(SignatureAlgorithm::Ed25519);
    refused(admit(&child, &stranger, DURING), "SignatureInvalid");
}

/// Authorization yields a scoped session carrying every restriction the credential expressed. A statement over two tables needs both accesses in one coherent grant; actions and tables never flatten into independent allowlists.
// spec: authority.profile.scoped-session@61b396ef
#[test]
fn a_statement_over_two_tables_needs_one_grant_covering_both() {
    let signer = issuer();
    let grants = vec![grant(&[Action::Read], &["research/filings"]), grant(&[Action::Write], &["sales/invoices"])];
    let credential = mint(&plan_for(&signer, dana(), grants.clone()), &MintClaims::default(), &signer).unwrap();
    let admitted = admit(&credential, &signer, DURING).unwrap();
    assert_eq!(admitted.grants(), grants.as_slice());
    assert!(admitted.permits(Action::Read, &["research/filings"]));
    assert!(admitted.permits(Action::Write, &["sales/invoices"]));
    // Flattened allowlists would admit these; the grants do not.
    assert!(!admitted.permits(Action::Write, &["research/filings"]));
    assert!(!admitted.permits(Action::Read, &["research/filings", "sales/invoices"]));
    let both = vec![grant(&[Action::Read], &["research/filings", "sales/invoices"])];
    let credential = mint(&plan_for(&signer, dana(), both), &MintClaims::default(), &signer).unwrap();
    assert!(admit(&credential, &signer, DURING).unwrap().permits(Action::Read, &["research/filings", "sales/invoices"]));
}

/// A checkpoint reading a profile version outside its supported set raises `ProfileVersionUnsupported`. Widening the profile mints a new version.
// spec: authority.profile.version-unsupported@0eb5690f
#[test]
fn a_profile_version_outside_the_supported_set_is_refused() {
    assert_eq!(SUPPORTED_PROFILE_VERSIONS, &[PROFILE_VERSION]);
    let signer = issuer();
    let b = block(&signer);
    let v2 = craft(&signer, &b, &["profile"], "profile(2);");
    refused(admit(&v2, &signer, DURING), "ProfileVersionUnsupported");
    let none = craft(&signer, &b, &["profile"], "");
    refused(admit(&none, &signer, DURING), "ProfileVersionUnsupported");
    // A checkpoint whose supported set names the version admits it.
    let revocation = RevocationState::default();
    let widened = Admission { profiles: &[1, 2], ..Admission::new(at(DURING), &revocation) };
    assert_eq!(verify(&v2, &keys(&signer), &widened).unwrap().profile(), 2);
}
