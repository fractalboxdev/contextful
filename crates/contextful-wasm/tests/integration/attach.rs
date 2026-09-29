//! `connector.attach` at the guest's mediation point.

use crate::support::{loopback, open_with, text, Response, Server};
use contextful_core::connector::attach::Allowlist;
use contextful_core::connector::reference::Hydrated;
use contextful_core::run::{Failure, FailureTag};
use contextful_wasm::{Grant, Hydrate, Limits};
use contextful_outbound::client::HeaderValue;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

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
        let grant = Grant { allow: Allowlist::parse(hosts).unwrap(), attach: bearer(), hydrate: Vec::new(), gate: None, hook: None, class: None, run_id: None, transport: None };
        let f = open_with(grant, &Limits::default(), None).err().expect("refused");
        assert!(f.message.starts_with("SecretWildcardHost"), "{hosts:?}: {f}");
        let grant = Grant { allow: Allowlist::parse(hosts).unwrap(), hydrate: minted(), ..loopback() };
        let f = open_with(grant, &Limits::default(), None).err().expect("refused");
        assert!(f.message.starts_with("SecretWildcardHost"), "a per-request header binds a credential too: {hosts:?}: {f}");
    }
    let unbound = Grant { allow: Allowlist::parse(&["*.vendor.example"]).unwrap(), attach: Vec::new(), hydrate: Vec::new(), gate: None, hook: None, class: None, run_id: None, transport: None };
    assert!(open_with(unbound, &Limits::default(), None).is_ok(), "a wildcard binds no credential");
}

/// A lease provider stand-in: each hydration mints the next token, or fails as the mint endpoint does.
#[derive(Default)]
struct Mint {
    asked: AtomicUsize,
    down: bool,
}

impl Hydrate for Mint {
    fn hydrate(&self) -> Result<HeaderValue, Failure> {
        let n = self.asked.fetch_add(1, Ordering::SeqCst) + 1;
        if self.down {
            return Err(Failure::new(FailureTag::Transient, "the mint endpoint is unreachable"));
        }
        Ok(HeaderValue::Sensitive(Hydrated::new(format!("Bearer lease-{n}"))))
    }
}

fn minted() -> Vec<(String, Arc<dyn Hydrate>)> {
    vec![("Authorization".into(), Arc::new(Mint::default()) as Arc<dyn Hydrate>)]
}

/// Material enters the process per request, while the request is built: a session holds no hydrated value between
/// requests, so each request carries what the provider answers at that moment.
#[test]
fn a_hydrated_header_renders_afresh_for_each_request() {
    let server = Server::start(|_| Response::text(200, "ok"));
    let mint = Arc::new(Mint::default());
    let grant = Grant { hydrate: vec![("Authorization".into(), mint.clone() as Arc<dyn Hydrate>)], ..loopback() };
    let mut s = open_with(grant, &Limits::default(), None).unwrap();
    assert_eq!(mint.asked.load(Ordering::SeqCst), 0, "opening a session hydrates nothing");
    for _ in 0..2 {
        s.open(&format!("fetch {}", server.url("/v1")), None).unwrap();
        assert_eq!(text(&s.next().unwrap().unwrap()), "200 ok");
    }
    let got = server.received();
    assert_eq!(got[0].header("authorization"), Some("Bearer lease-1"));
    assert_eq!(got[1].header("authorization"), Some("Bearer lease-2"));
    assert!(format!("{:?}", s.traffic()).find("lease-").is_none(), "the record names no value");
}

/// A hydration failure sends the vendor nothing and fails the call with the provider's failure, even when the guest
/// swallows the refused request.
#[test]
fn a_failed_hydration_sends_nothing_and_fails_the_call() {
    let server = Server::start(|_| Response::text(200, "ok"));
    let mint = Arc::new(Mint { down: true, ..Mint::default() });
    let grant = Grant { hydrate: vec![("Authorization".into(), mint.clone() as Arc<dyn Hydrate>)], ..loopback() };
    let mut s = open_with(grant, &Limits::default(), None).unwrap();
    s.open(&format!("swallow {}", server.url("/v1")), None).unwrap();
    let f = s.next().unwrap_err();
    assert_eq!(f.tag, FailureTag::Transient, "{f}");
    assert!(f.message.contains("mint endpoint is unreachable"), "{f}");
    assert_eq!(mint.asked.load(Ordering::SeqCst), 1);
    assert!(server.received().is_empty(), "the vendor received a request without its credential");
}
