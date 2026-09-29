//! `authority.revoke`: the denylist, the scoped epoch, rotation grace and format withdrawal.

use contextful_core::issue::{IssuancePolicy, ISSUANCE_LIFETIME_CEILING_SECS, ISSUER_KEY_ROTATION_CADENCE_SECS};
use contextful_core::revoke::{
    check_revoked, Denylist, EpochScope, Epochs, FormatWithdrawals, RevocationClaims, RotationPolicy,
};
use contextful_core::time::Instant;
use contextful_core::AuthorityError;
use std::collections::BTreeSet;

const NOW: &str = "2030-01-01T00:00:00Z";

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

fn claims(ids: &[&str], tenant: Option<&str>, class: &str, epoch: u64) -> RevocationClaims {
    RevocationClaims {
        project: "acme-research".into(),
        tenant: tenant.map(str::to_string),
        principal_class: class.into(),
        revocation_ids: ids.iter().map(|s| s.to_string()).collect(),
        epoch,
    }
}

fn scope(tenant: Option<&str>, class: Option<&str>) -> EpochScope {
    EpochScope {
        project: "acme-research".into(),
        tenant: tenant.map(str::to_string),
        principal_class: class.map(str::to_string),
    }
}

fn revoked(r: Result<(), AuthorityError>) -> bool {
    matches!(r, Err(AuthorityError::AuthorityRevoked(_)))
}

fn policy(max_lifetime_secs: u64) -> IssuancePolicy {
    IssuancePolicy::parse(&format!("default_audience = \"contextful://acme\"\nmax_lifetime_secs = {max_lifetime_secs}\n"))
        .unwrap()
}

/// A checkpoint reads a denylist keyed on credential identifier. An entry ages out once its identifier verifies under no live key version.
// spec: authority.revoke.denylist@ba8319e4
#[test]
fn a_denylist_entry_ages_out_once_no_live_key_version_verifies_it() {
    let mut deny = Denylist::default();
    deny.deny("rev://01JBQ7K4F3S9W2X6", "k1");
    deny.deny("rev://01JBQ7K4F3S9W2X7", "k2");
    assert!(deny.contains("rev://01JBQ7K4F3S9W2X6"));
    assert!(!deny.contains("rev://unlisted"));

    // k1 still verifies: nothing ages out.
    let live: BTreeSet<String> = ["k1", "k2"].map(String::from).into();
    assert!(deny.age_out(&live).is_empty());
    assert_eq!(deny.len(), 2);

    // k1 retires: its entry ages out, k2's stays.
    let live: BTreeSet<String> = ["k2", "k3"].map(String::from).into();
    assert_eq!(deny.age_out(&live), vec!["rev://01JBQ7K4F3S9W2X6".to_string()]);
    assert!(!deny.contains("rev://01JBQ7K4F3S9W2X6"));
    assert!(deny.contains("rev://01JBQ7K4F3S9W2X7"));
}

/// Every derivation carries its own revocation identifier, withdrawing one subtree without ending the root it descends from.
// spec: authority.revoke.revocation-id@d9df7c95
#[test]
fn denying_a_derivation_withdraws_its_subtree_and_leaves_the_root() {
    let root = claims(&["rev://root"], None, "user", 0);
    let child = claims(&["rev://root", "rev://child"], None, "user", 0);
    let grandchild = claims(&["rev://root", "rev://child", "rev://grandchild"], None, "user", 0);
    let sibling = claims(&["rev://root", "rev://sibling"], None, "user", 0);
    let epochs = Epochs::default();

    let mut deny = Denylist::default();
    deny.deny("rev://child", "k1");
    assert!(check_revoked(&root, &deny, &epochs).is_ok());
    assert!(check_revoked(&sibling, &deny, &epochs).is_ok());
    assert!(revoked(check_revoked(&child, &deny, &epochs)));
    assert!(revoked(check_revoked(&grandchild, &deny, &epochs)));

    deny.deny("rev://root", "k1");
    assert!(revoked(check_revoked(&root, &deny, &epochs)));
    assert!(revoked(check_revoked(&sibling, &deny, &epochs)));
}

/// A revocation epoch scoped on project, tenant and principal class invalidates that slice of outstanding authority.
#[test]
fn a_scoped_epoch_invalidates_only_its_slice() {
    let deny = Denylist::default();
    let mut epochs = Epochs::default();
    let eu_agent = claims(&["rev://a"], Some("acme-eu"), "agent", 0);
    let eu_user = claims(&["rev://b"], Some("acme-eu"), "user", 0);
    let us_agent = claims(&["rev://c"], Some("acme-us"), "agent", 0);

    assert_eq!(epochs.bump(scope(Some("acme-eu"), Some("agent"))), 1);
    assert!(revoked(check_revoked(&eu_agent, &deny, &epochs)));
    assert!(check_revoked(&eu_user, &deny, &epochs).is_ok());
    assert!(check_revoked(&us_agent, &deny, &epochs).is_ok());
    assert_eq!(epochs.current_for(&eu_agent), 1);

    // A credential minted at the new epoch admits.
    assert!(check_revoked(&claims(&["rev://d"], Some("acme-eu"), "agent", 1), &deny, &epochs).is_ok());

    // A tenant-wide bump reaches every class in the tenant and no other tenant.
    epochs.bump(scope(Some("acme-eu"), None));
    assert!(revoked(check_revoked(&eu_user, &deny, &epochs)));
    assert!(check_revoked(&us_agent, &deny, &epochs).is_ok());

    // A project-wide bump reaches all of it; another project's epoch reaches none of it.
    let mut other = Epochs::default();
    other.bump(EpochScope { project: "globex".into(), tenant: None, principal_class: None });
    assert!(check_revoked(&us_agent, &deny, &other).is_ok());
    epochs.bump(scope(None, None));
    assert!(revoked(check_revoked(&us_agent, &deny, &epochs)));
}

/// A credential on the denylist, or carrying an epoch below the current scoped epoch, raises `AuthorityRevoked` at the next effect boundary.
// spec: authority.revoke.revoked@0dc2bfca
#[test]
fn a_denylisted_or_stale_epoch_credential_is_revoked() {
    let cred = claims(&["rev://01JBQ7K4F3S9W2X6"], Some("acme-eu"), "user", 7);
    let mut deny = Denylist::default();
    let mut epochs = Epochs::default();
    assert!(check_revoked(&cred, &deny, &epochs).is_ok());

    deny.deny("rev://01JBQ7K4F3S9W2X6", "k1");
    let err = check_revoked(&cred, &deny, &epochs).unwrap_err();
    assert!(err.to_string().starts_with("AuthorityRevoked"), "{err}");

    let deny = Denylist::default();
    for _ in 0..7 {
        epochs.bump(scope(Some("acme-eu"), Some("user")));
    }
    assert!(check_revoked(&cred, &deny, &epochs).is_ok(), "epoch equal to current admits");
    epochs.bump(scope(Some("acme-eu"), Some("user")));
    assert!(revoked(check_revoked(&cred, &deny, &epochs)));
}

/// A rotation policy declares a cadence and a grace window, validated against the persisted issuance policy, during which the retiring key version still verifies.
// spec: authority.revoke.rotation-grace@e84fb78c
#[test]
fn a_retiring_key_verifies_through_a_grace_window_validated_against_issuance() {
    let rotation = RotationPolicy::default();
    assert_eq!(rotation.cadence_secs, ISSUER_KEY_ROTATION_CADENCE_SECS);
    assert_eq!(rotation.grace_secs, ISSUANCE_LIFETIME_CEILING_SECS);
    assert!(rotation.validate(&policy(ISSUANCE_LIFETIME_CEILING_SECS), at(NOW)).is_ok());

    let rotation = RotationPolicy { cadence_secs: ISSUER_KEY_ROTATION_CADENCE_SECS, grace_secs: 3600 };
    assert!(rotation.validate(&policy(3600), at(NOW)).is_ok());
    let retiring_since = at(NOW);
    assert!(rotation.retiring_key_verifies(retiring_since, retiring_since.plus_secs(3599)));
    assert!(!rotation.retiring_key_verifies(retiring_since, retiring_since.plus_secs(3600)));
}

/// A declared or overridden grace window shorter than the effective issuance ceiling raises `RotationGraceTooShort`.
// spec: authority.revoke.short-grace@0dabf73d
#[test]
fn a_grace_window_shorter_than_the_effective_ceiling_refuses() {
    let declared = RotationPolicy { cadence_secs: ISSUER_KEY_ROTATION_CADENCE_SECS, grace_secs: 1800 };
    let err = declared.validate(&policy(3600), at(NOW)).unwrap_err();
    assert!(matches!(err, AuthorityError::RotationGraceTooShort(_)), "{err}");
    assert!(err.to_string().starts_with("RotationGraceTooShort"), "{err}");

    // An override is held to the same floor.
    let overridden = RotationPolicy::default().with_grace(600);
    assert!(matches!(overridden.validate(&policy(900), at(NOW)), Err(AuthorityError::RotationGraceTooShort(_))));

    // The effective ceiling: a lowered ceiling's recorded value binds until it lapses.
    let mut lowered = policy(86_400);
    lowered.lower_ceiling(3600, at(NOW)).unwrap();
    let grace = RotationPolicy { cadence_secs: ISSUER_KEY_ROTATION_CADENCE_SECS, grace_secs: 7200 };
    assert!(matches!(grace.validate(&lowered, at(NOW).plus_secs(3600)), Err(AuthorityError::RotationGraceTooShort(_))));
    assert!(grace.validate(&lowered, at(NOW).plus_secs(86_400)).is_ok());
}

/// From a declared cutover instant, every admission path raises `CredentialFormatWithdrawn` for a credential in a withdrawn format. Only an explicit declaration restores acceptance.
// spec: authority.revoke.format-withdrawn@5dfe4109
#[test]
fn a_withdrawn_format_refuses_from_its_cutover_until_explicitly_restored() {
    let cutover = at("2030-06-01T00:00:00Z");
    let mut formats = FormatWithdrawals::default();
    assert!(formats.check("biscuit-v2", cutover).is_ok());

    formats.withdraw("biscuit-v2", cutover);
    assert!(formats.check("biscuit-v2", at("2030-05-31T23:59:59Z")).is_ok());
    let err = formats.check("biscuit-v2", cutover).unwrap_err();
    assert!(matches!(err, AuthorityError::CredentialFormatWithdrawn(_)), "{err}");
    assert!(err.to_string().contains("biscuit-v2"), "{err}");
    assert!(formats.check("biscuit-v2", at("2031-01-01T00:00:00Z")).is_err());
    assert!(formats.check("biscuit-v3", at("2031-01-01T00:00:00Z")).is_ok());

    formats.restore("biscuit-v2");
    assert!(formats.check("biscuit-v2", at("2031-01-01T00:00:00Z")).is_ok());
}
