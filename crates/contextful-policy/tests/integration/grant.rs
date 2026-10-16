//! `authority.grant`, chain side: the bare star as an ordinary, bounded grant.

use crate::support::*;
use contextful_core::grant::{Action, TablePattern};
use contextful_policy::issue::{mint, MintClaims};
use contextful_policy::verify::{introspect, no_holder_proof, verify_network, Admission};

/// `*` is an ordinary grant, bounded by the credential's actions, audience and expiry and visible in the audit record.
// spec: authority.grant.star@c1d6178c
#[test]
fn a_star_grant_is_bounded_by_actions_audience_and_expiry_and_names_itself() {
    let signer = issuer();
    let star = mint(&plan_for(&signer, dana(), vec![grant(&[Action::Read], &["*"])]), &MintClaims::default(), &signer).unwrap();
    let admitted = admit(&star, &signer, DURING).unwrap();
    // Every table, under the actions it names and no other.
    assert!(admitted.permits(Action::Read, &["research/filings", "hr/salaries"]));
    for action in [Action::Write, Action::Execute, Action::Admin, Action::Forget] {
        assert!(!admitted.permits(action, &["research/filings"]), "{action:?}");
    }
    // The audience and expiry bind it as they bind any grant.
    let revocation = no_revocation();
    let elsewhere = Admission::new(at(DURING), &revocation).expecting("contextful://globex");
    refused(verify_network(&star, &keys(&signer), &elsewhere, no_holder_proof::<contextful_core::AuthorityError>), "AudienceMismatch");
    refused(admit(&star, &signer, "2030-01-01T00:15:01Z"), "AuthorityExpired");
    // The credential the audit record names declares `*` as written.
    let declared = introspect(&star).unwrap();
    assert_eq!(declared.authority.rev.id, admitted.credential_id());
    assert_eq!(declared.authority.grants[0].tables, vec![TablePattern::All]);
    let rendered: serde_json::Value = serde_json::from_str(&declared.to_json()).unwrap();
    assert_eq!(rendered["authority"]["grants"][0]["tables"], serde_json::json!(["*"]), "{rendered}");
}
