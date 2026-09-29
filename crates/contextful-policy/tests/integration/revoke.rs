//! `authority.revoke`, checkpoint side: the denylist file, per-derivation identifiers,
//! the scoped epoch and format withdrawal at admission.

use crate::support::*;
use contextful_core::revoke::EpochScope;
use contextful_policy::issue::{mint, MintClaims};
use contextful_policy::revoke::{parse_denylist, revocation_claims, RevocationState, PRINCIPAL_CLASS_DELEGATED};
use contextful_policy::verify::{introspect, verify_inherited_pipe, Admission, BISCUIT_FORMAT};

#[test]
fn a_denylist_file_holds_one_identifier_per_line() {
    let denylist = parse_denylist("# withdrawn\nabc\n\n  def  \n", "k1");
    assert_eq!(denylist.len(), 2);
    assert!(denylist.contains("abc") && denylist.contains("def"));
}

#[test]
fn the_credential_identifier_withdraws_the_whole_chain() {
    let signer = issuer();
    let credential = minted(&signer);
    let id = introspect(&credential).unwrap().authority.rev.id;
    refused(admit_denying(&credential, &signer, &[&id]), "AuthorityRevoked");
}

#[test]
fn an_epoch_below_the_current_scoped_epoch_is_refused_at_admission() {
    let signer = issuer();
    let credential = mint(&plan(&signer), &MintClaims { confirmation: None, epoch: 1 }, &signer).unwrap();
    let admitted = admit(&credential, &signer, DURING).unwrap();
    let claims = revocation_claims(&admitted);
    assert_eq!(claims.project, AUD);
    assert_eq!(claims.principal_class, PRINCIPAL_CLASS_DELEGATED);
    assert_eq!(claims.revocation_ids.len(), 2);
    let mut state = RevocationState::default();
    let scope = EpochScope { project: AUD.into(), tenant: None, principal_class: Some(PRINCIPAL_CLASS_DELEGATED.into()) };
    state.epochs.bump(scope.clone());
    assert!(verify_inherited_pipe(&credential, &keys(&signer), &Admission::new(at(DURING), &state)).is_ok());
    state.epochs.bump(scope);
    refused(verify_inherited_pipe(&credential, &keys(&signer), &Admission::new(at(DURING), &state)), "AuthorityRevoked");
}

#[test]
fn a_withdrawn_format_is_refused_from_its_cutover() {
    let signer = issuer();
    let credential = minted(&signer);
    let mut state = RevocationState::default();
    state.withdrawals.withdraw(BISCUIT_FORMAT, at("2030-01-01T00:10:00Z"));
    assert!(verify_inherited_pipe(&credential, &keys(&signer), &Admission::new(at(DURING), &state)).is_ok());
    refused(verify_inherited_pipe(&credential, &keys(&signer), &Admission::new(at("2030-01-01T00:10:00Z"), &state)), "CredentialFormatWithdrawn");
}
