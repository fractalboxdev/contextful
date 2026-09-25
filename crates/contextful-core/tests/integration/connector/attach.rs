//! The allowlist, the endpoint check and the origin rules.

use contextful_core::connector::attach::{endpoint, scrub, Allowlist};
use contextful_core::connector::ConnectorError;

/// The outbound allowlist holds exact hosts and subdomain wildcards matched as suffixes; a wildcard entry covers
/// subdomains and not the apex.
// spec: connector.declare-capability.host-allowlist@6b6de822
#[test]
fn exact_hosts_and_subdomain_wildcards_match_as_suffixes() {
    let a = Allowlist::parse(&["api.vendor.example", "*.cdn.example"]).unwrap();
    assert!(a.permits("api.vendor.example"));
    assert!(a.permits("API.Vendor.Example"));
    assert!(!a.permits("evil.api.vendor.example"), "an exact entry covers no subdomain");
    assert!(a.permits("eu.cdn.example") && a.permits("a.b.cdn.example"));
    assert!(!a.permits("cdn.example"), "a wildcard does not cover its apex");
    assert!(!a.permits("evilcdn.example"), "a suffix matches on a label boundary");
}

/// An empty allowlist, a bare wildcard, an empty entry, and an entry carrying a scheme, port or path raise
/// `ConnectorAllowlistRejected`.
// spec: connector.declare-capability.allowlist-shape@de231ee1
#[test]
fn a_malformed_allowlist_is_refused() {
    let empty: [&str; 0] = [];
    assert!(matches!(Allowlist::parse(&empty), Err(ConnectorError::ConnectorAllowlistRejected(_))));
    for entry in ["*", "", "https://api.vendor.example", "api.vendor.example:443", "api.vendor.example/v1"] {
        assert!(matches!(Allowlist::parse(&[entry]), Err(ConnectorError::ConnectorAllowlistRejected(_))), "{entry:?}");
    }
}

/// A configured endpoint carrying userinfo raises `SecretCredentialInUrl` at validation, naming the source.
// spec: connector.attach.credential-in-a-url@12e202ee
#[test]
fn an_endpoint_carrying_userinfo_is_refused() {
    for raw in ["https://svc:hunter2@api.vendor.example/v1", "https://token@api.vendor.example/v1"] {
        match endpoint("http", raw) {
            Err(ConnectorError::SecretCredentialInUrl(m)) => {
                assert!(m.contains("`http`"), "{m}");
                assert!(!m.contains("hunter2") && !m.contains("token@"), "{m}");
            }
            other => panic!("{raw}: {other:?}"),
        }
    }
    let ok = endpoint("http", "https://api.vendor.example:8443/v1?key=x#frag").unwrap();
    assert_eq!(scrub(&ok), "https://api.vendor.example:8443/v1");
}

#[test]
fn a_bound_credential_holds_to_one_exact_host() {
    assert_eq!(Allowlist::parse(&["api.vendor.example"]).unwrap().check_bound().unwrap(), "api.vendor.example");
    assert!(matches!(Allowlist::parse(&["*.vendor.example"]).unwrap().check_bound(), Err(ConnectorError::SecretWildcardHost(_))));
}
