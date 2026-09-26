//! The scope probe's declaration and its judgment of a granted-scopes header.

use contextful_core::connector::probe::{scope_violations, ScopeProbe};
use contextful_core::connector::ConnectorError;

fn declared(expect: &[&str]) -> ScopeProbe {
    ScopeProbe::new("https://api.vendor.example/v1/identity", "X-Granted-Scopes", expect).unwrap()
}

#[test]
fn the_expectation_is_a_ceiling_and_a_subset_is_within_it() {
    let p = declared(&["reports.read", "accounts.read"]);
    assert!(p.violations("reports.read,accounts.read").is_empty());
    assert!(p.violations("accounts.read reports.read").is_empty(), "order and separator are the vendor's");
    assert!(p.violations("reports.read").is_empty());
    assert_eq!(p.judge(Some("REPORTS.READ, accounts.read")).unwrap(), ["REPORTS.READ", "accounts.read"]);
}

#[test]
fn a_scope_absent_from_the_expectation_is_named_in_granted_order() {
    let p = declared(&["reports.read"]);
    assert_eq!(p.violations("files.read,reports.read,some.future.scope"), ["files.read", "some.future.scope"]);
    assert!(matches!(p.judge(Some("reports.read,files.read")), Err(ConnectorError::ConnectorScopeExceeded(m)) if m.contains("files.read")));
    assert!(matches!(p.judge(None), Err(ConnectorError::ConnectorScopeUnverified(_))));
}

#[test]
fn comparison_is_case_insensitive_and_ignores_empty_entries() {
    let expect = vec!["reports.read".to_string()];
    assert!(scope_violations("  REPORTS.READ  ", &expect).is_empty());
    assert!(scope_violations("", &expect).is_empty());
    assert!(scope_violations(", ,", &expect).is_empty());
}

#[test]
fn an_empty_expectation_admits_no_granted_scope() {
    let p = declared(&[]);
    assert_eq!(p.violations("reports.read"), ["reports.read"]);
}

#[test]
fn a_probe_endpoint_carrying_userinfo_is_refused() {
    let e = ScopeProbe::new("https://token@api.vendor.example/v1/identity", "X-Granted-Scopes", &["reports.read"]).unwrap_err();
    assert!(matches!(e, ConnectorError::SecretCredentialInUrl(_)), "{e}");
    assert!(ScopeProbe::new("api.vendor.example/v1/identity", "X-Granted-Scopes", &["reports.read"]).is_err());
}
