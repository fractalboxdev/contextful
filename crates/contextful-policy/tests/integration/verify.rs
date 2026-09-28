//! `authority.verify`: admission, the admitted-authority value, the effect boundary and
//! the credential-format interface.

use crate::support::*;
use contextful_core::grant::{Action, TablePattern};
use contextful_core::revoke::Denylist;
use contextful_policy::attenuate::Derivation;
use contextful_policy::issue::MintClaims;
use contextful_policy::revoke::{parse_denylist, RevocationState};
use contextful_policy::verify::{effect_boundary, verify_local_bearer, Admission, BiscuitFormat, CredentialFormat};

/// Verification yields an admitted-authority value carrying the normalized subject tuple and its grants. Every read surface and row-landing effect takes that value as an argument; nothing downstream re-parses a credential or reads ambient state.
// spec: authority.verify.admitted-authority@6a9d4f19
#[test]
fn verification_yields_an_admitted_value_with_the_normalized_subject_and_grants() {
    let signer = issuer();
    let mut padded = dana();
    padded.task = Some("  ".into());
    let credential =
        contextful_policy::issue::mint(&plan_for(&signer, padded, vec![grant(&[Action::Read], &["research/*"])]), &MintClaims::default(), &signer)
            .unwrap();
    let admitted = admit(&credential, &signer, DURING).unwrap();
    assert_eq!(admitted.subject().on_behalf_of(), Some("user://dana@acme.example"));
    assert_eq!(admitted.subject().agent(), Some("agent://research-loop"));
    assert_eq!(admitted.subject().task(), None, "a blank member is dropped once, at normalization");
    assert_eq!(admitted.grants(), &[grant(&[Action::Read], &["research/*"])]);
    assert_eq!(admitted.audience(), AUD);
    assert_eq!(admitted.expires_at(), at("2030-01-01T00:15:00Z"));
    assert_eq!(admitted.key_version(), signer.public_key_text());
    let json = admitted.to_json();
    assert!(json.contains("user://dana@acme.example") && json.contains("research/*"), "{json}");
    // A downstream effect reads the carried value: the boundary takes no credential.
    let revocation = no_revocation();
    assert_eq!(effect_boundary(&admitted, &Admission::new(at(DURING), &revocation)), Ok(()));
}

/// A statement's start and each commit are effect boundaries. Each boundary re-reads expiry, revocation and policy version against the carried value; a lapse between boundaries stops the effect at the next one.
// spec: authority.verify.effect-boundary@b9728137
#[test]
fn each_effect_boundary_re_reads_expiry_revocation_and_profile_version() {
    let signer = issuer();
    let admitted = admit(&minted(&signer), &signer, DURING).unwrap();
    let clean = no_revocation();
    assert_eq!(effect_boundary(&admitted, &Admission::new(at("2030-01-01T00:10:00Z"), &clean)), Ok(()));
    // Expiry lapses between boundaries.
    refused(effect_boundary(&admitted, &Admission::new(at("2030-01-01T00:15:01Z"), &clean)), "AuthorityExpired");
    // A denylist entry lands between boundaries.
    let mut denylist = Denylist::default();
    denylist.deny(admitted.revocation_ids().last().unwrap(), "k1");
    let denied = RevocationState { denylist, ..RevocationState::default() };
    refused(effect_boundary(&admitted, &Admission::new(at(DURING), &denied)), "AuthorityRevoked");
    // An epoch bump lands between boundaries.
    let mut bumped = RevocationState::default();
    bumped.epochs.bump(contextful_core::revoke::EpochScope { project: AUD.into(), tenant: None, principal_class: None });
    refused(effect_boundary(&admitted, &Admission::new(at(DURING), &bumped)), "AuthorityRevoked");
    // The supported profile set moves between boundaries.
    let moved = Admission { profiles: &[2], ..Admission::new(at(DURING), &clean) };
    refused(effect_boundary(&admitted, &moved), "ProfileVersionUnsupported");
}

/// The credential format sits behind one interface of issue, attenuate, verify and introspect; no enforcement call site names a credential type.
// spec: authority.verify.format-interface@c50ea1e5
#[test]
fn issue_attenuate_verify_and_introspect_run_through_one_interface() {
    let format: &dyn CredentialFormat = &BiscuitFormat;
    let signer = issuer();
    let parent = format.issue(&plan(&signer), &MintClaims::default(), &signer).unwrap();
    let declared = format.introspect(&parent).unwrap();
    assert_eq!(declared.format, format.name());
    let child = format
        .attenuate(&parent, &Derivation::narrowing(&declared.authority.grants, None, Some(&[TablePattern::parse("research/filings").unwrap()])))
        .unwrap();
    let revocation = no_revocation();
    let admitted = format.verify(&child, &keys(&signer), &Admission::new(at(DURING), &revocation).expecting(AUD)).unwrap();
    assert_eq!(admitted.format(), format.name());
    assert!(admitted.permits(Action::Read, &["research/filings"]));
    assert!(!admitted.permits(Action::Read, &["research/notes"]));
}

/// A credential with any block signature failing against a pinned key raises `SignatureInvalid` and admits nothing, no verified prefix included.
// spec: authority.verify.bad-signature@a143346b
#[test]
fn any_failing_block_signature_admits_nothing_not_even_a_verified_prefix() {
    let signer = issuer();
    let parent = minted(&signer);
    let child = contextful_policy::attenuate::attenuate(
        &parent,
        &Derivation::narrowing(&[grant(&[Action::Read], &["research/*"])], None, Some(&[TablePattern::parse("research/filings").unwrap()])),
    )
    .unwrap();
    // A key nobody pinned.
    refused(admit(&parent, &issuer(), DURING), "SignatureInvalid");
    // A tampered child block: its parent prefix verifies, and still nothing is admitted.
    let key = keys(&signer).keys().next().unwrap().public_key;
    let token = biscuit_auth::Biscuit::from_base64(&child, key).unwrap();
    let mut container = token.container().clone();
    // The child block carries the authority block's signature, which signs other bytes.
    container.blocks[0].signature = container.authority.signature.clone();
    let tampered = base64_url(&container.to_vec().unwrap());
    refused(admit(&tampered, &signer, DURING), "SignatureInvalid");
    // Flipped transmitted text, as the milestone flow tampers it.
    let mut bytes = child.clone().into_bytes();
    let mid = bytes.len() / 2;
    bytes[mid] = if bytes[mid] == b'A' { b'B' } else { b'A' };
    refused(admit(&String::from_utf8(bytes).unwrap(), &signer, DURING), "SignatureInvalid");
}

fn base64_url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE.encode(bytes)
}

/// A checkpoint declaring an expected audience raises `AudienceMismatch` for a credential with a different audience or none; a checkpoint declaring none performs no audience check.
// spec: authority.verify.audience-mismatch@2e0b0171
#[test]
fn a_declared_audience_refuses_another_or_none_and_an_undeclared_one_checks_nothing() {
    let signer = issuer();
    let credential = minted(&signer);
    let revocation = no_revocation();
    let other = Admission::new(at(DURING), &revocation).expecting("contextful://other");
    refused(verify_local_bearer(&credential, &keys(&signer), &other), "AudienceMismatch");
    let none = craft(&signer, &block(&signer), &["aud"], "");
    refused(admit(&none, &signer, DURING), "AudienceMismatch");
    let undeclared = Admission::new(at(DURING), &revocation);
    assert!(verify_local_bearer(&credential, &keys(&signer), &undeclared).is_ok());
    assert!(verify_local_bearer(&none, &keys(&signer), &undeclared).is_ok());
}

/// A credential whose expiry precedes the evaluation instant raises `AuthorityExpired` at admission and at each later effect boundary.
// spec: authority.verify.expired@3fb3ae75
#[test]
fn a_credential_past_its_expiry_is_refused_at_admission_and_every_later_boundary() {
    let signer = issuer();
    let credential = minted(&signer);
    // Expiry is 2030-01-01T00:15:00Z; the instant itself still admits.
    assert!(admit(&credential, &signer, "2030-01-01T00:15:00Z").is_ok());
    refused(admit(&credential, &signer, "2030-01-01T00:15:01Z"), "AuthorityExpired");
    // A child's own earlier expiry governs the chain.
    let child = contextful_policy::attenuate::attenuate(
        &credential,
        &Derivation { expires_at: Some(at("2030-01-01T00:06:00Z")), ..Derivation::default() },
    )
    .unwrap();
    let admitted = admit(&child, &signer, DURING).unwrap();
    refused(admit(&child, &signer, "2030-01-01T00:07:00Z"), "AuthorityExpired");
    let revocation = no_revocation();
    for later in ["2030-01-01T00:07:00Z", "2030-01-01T00:20:00Z"] {
        refused(effect_boundary(&admitted, &Admission::new(at(later), &revocation)), "AuthorityExpired");
    }
}

#[test]
fn a_credential_naming_no_subject_member_admits_nothing() {
    let signer = issuer();
    let credential = craft(&signer, &block(&signer), &["sub", "att"], "");
    refused(admit(&credential, &signer, DURING), "AuthoritySubjectMissing");
}

#[test]
fn a_credential_naming_another_scheme_than_its_pinned_key_is_refused() {
    let signer = issuer();
    let credential = craft(&signer, &block(&signer), &["alg"], "alg(\"ES256\");");
    refused(admit(&credential, &signer, DURING), "SignatureAlgorithmMismatch");
}

#[test]
fn a_timestamp_outside_the_grammar_is_refused() {
    let signer = issuer();
    refused(admit(&craft(&signer, &block(&signer), &["exp"], "exp(\"soon\");"), &signer, DURING), "TimestampMalformed");
}

/// The milestone-1 flow, through the API a command line wires up.
#[test]
fn the_authority_core_flow_admits_narrows_and_re_reads() {
    let signer = issuer();
    let parent = minted(&signer);
    let admitted = admit(&parent, &signer, DURING).unwrap().to_json();
    assert!(admitted.contains("user://dana@acme.example") && admitted.contains("research/*"));
    let grants = contextful_policy::verify::introspect(&parent).unwrap().authority.grants;
    let child =
        contextful_policy::attenuate::attenuate(&parent, &Derivation::narrowing(&grants, None, Some(&[TablePattern::parse("research/filings").unwrap()])))
            .unwrap();
    assert!(admit(&child, &signer, DURING).unwrap().to_json().contains("research/filings"));
    refused(admit(&child, &signer, "2030-01-01T00:20:00Z"), "AuthorityExpired");
    let rev = contextful_policy::verify::introspect(&child).unwrap().rev_id;
    let revocation = RevocationState { denylist: parse_denylist(&format!("{rev}\n"), "k1"), ..RevocationState::default() };
    let admission = Admission::new(at(DURING), &revocation).expecting(AUD);
    refused(verify_local_bearer(&child, &keys(&signer), &admission), "AuthorityRevoked");
    assert!(verify_local_bearer(&parent, &keys(&signer), &admission).is_ok());
}
