use crate::support::{admit, dana, grant, issuer, plan_for, DURING};
use contextful_core::grant::{Action, TenantScope};
use contextful_policy::enforce::erase::ForgetAdmission;
use contextful_policy::issue::{mint, MintClaims};

#[test]
fn forget_admission_requires_distinct_authority_for_every_requested_table() {
    let signer = issuer();
    let authority = |actions: &[Action]| {
        let token = mint(&plan_for(&signer, dana(), vec![grant(actions, &["research/*"])]), &MintClaims::default(), &signer).unwrap();
        admit(&token, &signer, DURING).unwrap()
    };
    for actions in [vec![Action::Read], vec![Action::Write], vec![Action::Execute], vec![Action::Read, Action::Write, Action::Execute]] {
        let authority = authority(&actions);
        let refusal = ForgetAdmission::admit(&authority, &["research/notes"]).unwrap_err();
        assert!(refusal.to_string().starts_with("ErasureUngranted"), "{refusal}");
    }
    let authority = authority(&[Action::Forget]);
    let admitted = ForgetAdmission::admit(&authority, &["research/notes", "research/keys"]).unwrap();
    assert_eq!(admitted.tables().collect::<Vec<_>>(), ["research/keys", "research/notes"]);
    assert_eq!(admitted.authority().credential_id(), authority.credential_id());
    assert!(ForgetAdmission::admit(&authority, &["research/notes", "hr/salaries"]).unwrap_err().to_string().starts_with("ErasureUngranted"));
}

#[test]
fn unsupported_forget_narrowing_and_empty_target_sets_refuse_before_store_access() {
    let signer = issuer();
    let mut tenant = grant(&[Action::Forget], &["research/*"]);
    tenant.tenant = Some(TenantScope { table: "research/notes".into(), value: "alice".into() });
    let mut aggregate = grant(&[Action::Forget], &["research/*"]);
    aggregate.max_rows = Some(1);
    let mut template = grant(&[Action::Forget], &["research/*"]);
    template.templates = Some(vec!["summarize".into()]);
    for grant in [tenant, aggregate, template] {
        let token = mint(&plan_for(&signer, dana(), vec![grant]), &MintClaims::default(), &signer).unwrap();
        let authority = admit(&token, &signer, DURING).unwrap();
        let refusal = ForgetAdmission::admit(&authority, &["research/notes"]).unwrap_err();
        assert!(refusal.to_string().starts_with("ErasureScopeUnsupported"), "{refusal}");
    }
    let token = mint(&plan_for(&signer, dana(), vec![grant(&[Action::Forget], &["research/*"])]), &MintClaims::default(), &signer).unwrap();
    let authority = admit(&token, &signer, DURING).unwrap();
    assert!(ForgetAdmission::admit(&authority, &[]).unwrap_err().to_string().starts_with("ErasureScopeUnsupported"));
}
