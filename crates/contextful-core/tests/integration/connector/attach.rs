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

#[test]
fn a_bracketed_ipv6_literal_is_an_allowlist_host() {
    let a = Allowlist::parse(&["[::1]"]).unwrap();
    assert!(a.permits("[::1]"));
    assert!(!a.permits("[::2]"));
    assert!(Allowlist::parse(&["[::1]:8080"]).is_err());
    assert!(Allowlist::parse(&["[not-an-address]"]).is_err());
}

/// A host name resolving to a private, link-local, unique-local, loopback or cloud-metadata address raises `ConnectorPrivateAddress`, unless the configured host is itself a loopback name or literal.
// spec: connector.attach.private-address@493d9ddd
#[test]
fn a_permitted_name_resolving_inward_is_refused() {
    use contextful_core::connector::attach::vet_address;
    use std::net::IpAddr;
    let internal = [
        "10.0.0.7",
        "172.16.4.2",
        "192.168.1.1",
        "169.254.169.254",
        "127.0.0.1",
        "100.64.0.1",
        "0.0.0.0",
        "::1",
        "fd00:ec2::254",
        "fe80::1",
        "::ffff:10.0.0.7",
    ];
    let mut admitted = 0u64;
    for raw in internal {
        let ip: IpAddr = raw.parse().unwrap();
        match vet_address("api.vendor.example", ip) {
            Err(ConnectorError::ConnectorPrivateAddress(m)) => assert!(m.contains("api.vendor.example"), "{m}"),
            other => {
                admitted += 1;
                eprintln!("{raw} admitted: {other:?}");
            }
        }
    }
    crate::emit("egress-internal-address", admitted as f64, internal.len() as u64, 0);
    assert_eq!(admitted, 0);
    // A public address passes, and a configured loopback host reaches loopback.
    assert!(vet_address("api.vendor.example", "93.184.216.34".parse().unwrap()).is_ok());
    assert!(vet_address("api.vendor.example", "2606:2800:220:1::1".parse().unwrap()).is_ok());
    for host in ["localhost", "127.0.0.1", "[::1]"] {
        assert!(vet_address(host, "127.0.0.1".parse().unwrap()).is_ok(), "{host}");
    }
}
