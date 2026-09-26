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
    assert_eq!(p.judge(Some("reports.read, accounts.read")).unwrap(), ["reports.read", "accounts.read"]);
}

#[test]
fn a_scope_absent_from_the_expectation_is_named_in_granted_order() {
    let p = declared(&["reports.read"]);
    assert_eq!(p.violations("files.read,reports.read,some.future.scope"), ["files.read", "some.future.scope"]);
    assert!(matches!(p.judge(Some("reports.read,files.read")), Err(ConnectorError::ConnectorScopeExceeded(m)) if m.contains("files.read")));
    assert!(matches!(p.judge(None), Err(ConnectorError::ConnectorScopeUnverified(_))));
}

#[test]
fn a_scope_differing_from_the_expectation_only_by_case_is_outside_it() {
    let expect = vec!["reports.read".to_string()];
    assert_eq!(scope_violations("  REPORTS.READ  ", &expect), ["REPORTS.READ"]);
    assert_eq!(scope_violations("Reports.Read,reports.read", &expect), ["Reports.Read"]);
    let p = declared(&["reports.read"]);
    assert!(matches!(p.judge(Some("Reports.Read")), Err(ConnectorError::ConnectorScopeExceeded(m)) if m.contains("Reports.Read")));
}

#[test]
fn a_scopes_value_naming_no_scope_is_unverified() {
    let p = declared(&["reports.read"]);
    for granted in ["", "   ", ",", ", ,\t"] {
        assert!(matches!(p.judge(Some(granted)), Err(ConnectorError::ConnectorScopeUnverified(_))), "{granted:?}");
    }
}

#[test]
fn a_scopes_value_holding_a_byte_outside_visible_ascii_is_unverified() {
    let p = declared(&["reports.read"]);
    for granted in ["reports.read caf\u{e9}.read", "reports.read,\u{fffd}", "reports.read\u{0}"] {
        assert!(matches!(p.judge(Some(granted)), Err(ConnectorError::ConnectorScopeUnverified(_))), "{granted:?}");
    }
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

/// A scope probe declaring a scopes header that is empty or not an HTTP field-name token raises
/// `ConnectorScopeProbeRejected`.
// spec: connector.declare-capability.probe-shape@521ccc45
#[test]
fn a_scopes_header_that_is_no_field_name_refuses_at_declaration() {
    for header in ["", "   ", "X Granted", "X-Granted:", "X-Granted\u{e9}", "(scopes)"] {
        let e = ScopeProbe::new("https://api.vendor.example/v1/identity", header, &["reports.read"]).unwrap_err();
        assert!(matches!(e, ConnectorError::ConnectorScopeProbeRejected(_)), "{header:?}: {e}");
    }
    let p = ScopeProbe::new("https://api.vendor.example/v1/identity", " X-OAuth-Scopes ", &["reports.read"]).unwrap();
    assert_eq!(p.scopes_header, "X-OAuth-Scopes");
}
