//! `connector.attach` at the guest's mediation point.

use crate::support::{loopback, open_with, text, Response, Server};
use contextful_core::connector::attach::Allowlist;
use contextful_core::connector::reference::Hydrated;
use contextful_wasm::{Grant, Limits};
use contextful_outbound::client::HeaderValue;

fn bearer() -> Vec<(String, HeaderValue)> {
    vec![("Authorization".into(), HeaderValue::Sensitive(Hydrated::new("Bearer vendor-token-value")))]
}

/// No host import hands credential bytes to guest code; a guest names a request and receives a response.
// spec: connector.attach.no-material-to-a-guest@643f937c
#[test]
fn the_host_attaches_the_credential_and_the_guest_sees_only_the_response() {
    let server = Server::start(|r| Response::text(200, if r.header("authorization").is_some() { "attached" } else { "bare" }));
    let grant = Grant { attach: bearer(), ..loopback() };
    let mut s = open_with(grant, &Limits::default(), None).unwrap();
    s.open(&format!("fetch {}", server.url("/v1")), None).unwrap();
    let seen = text(&s.next().unwrap().unwrap());
    assert_eq!(seen, "200 attached");
    assert!(!seen.contains("vendor-token-value"));
    let got = server.received();
    assert_eq!(got[0].header("authorization"), Some("Bearer vendor-token-value"), "the vendor received the host's header");
    assert!(format!("{:?}", s.traffic()).find("vendor-token-value").is_none(), "the record names no value");
}

/// At session open, a credential beside a wildcard or a second host raises `SecretWildcardHost`. The guest host has
/// no validation step, so `connector.attach.bound-host` stays unpinned here.
#[test]
fn a_credential_beside_a_wildcard_or_second_host_refuses_the_session() {
    for hosts in [&["*.vendor.example"][..], &["api.vendor.example", "127.0.0.1"][..]] {
        let grant = Grant { allow: Allowlist::parse(hosts).unwrap(), attach: bearer(), gate: None, hook: None, class: None, run_id: None, transport: None };
        let f = open_with(grant, &Limits::default(), None).err().expect("refused");
        assert!(f.message.starts_with("SecretWildcardHost"), "{hosts:?}: {f}");
    }
    let unbound = Grant { allow: Allowlist::parse(&["*.vendor.example"]).unwrap(), attach: Vec::new(), gate: None, hook: None, class: None, run_id: None, transport: None };
    assert!(open_with(unbound, &Limits::default(), None).is_ok(), "a wildcard binds no credential");
}
