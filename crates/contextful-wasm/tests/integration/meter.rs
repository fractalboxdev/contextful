//! `connector.meter` at the guest's mediation point.

use crate::support::{loopback, open_with, Gate, Response, Server};
use contextful_core::connector::attach::Allowlist;
use contextful_core::run::FailureTag;
use contextful_wasm::{Grant, Limits, Reservation};
use std::sync::atomic::Ordering;

fn gated(gate: std::sync::Arc<Gate>, allow: &[&str]) -> Grant {
    Grant { allow: Allowlist::parse(allow).unwrap(), attach: Vec::new(), gate: Some(gate) }
}

/// A denied reservation returns to the guest as a synthesized `429` carrying the retry-after. An unreachable limiter
/// fails the request as a transport failure.
// spec: connector.meter.synthesized-throttle@8f9740b9
#[test]
fn a_denied_reservation_is_a_synthesized_429_and_an_unreachable_limiter_a_transport_failure() {
    let server = Server::start(|_| Response::text(200, "vendor"));
    let gate = Gate::new(Reservation::Denied { retry_after_secs: 7 });
    let mut s = open_with(gated(gate.clone(), &["127.0.0.1"]), &Limits::default(), None).unwrap();
    s.open(&format!("fetch {}", server.url("/v1")), None).unwrap();
    let f = s.next().unwrap_err();
    assert_eq!((f.tag, f.retry_after_secs), (FailureTag::RateLimited, Some(7)), "the guest read the 429 and its retry-after");
    assert_eq!(s.traffic().throttled, 1);
    assert!(server.received().is_empty(), "a denied request never reaches the vendor");

    let gate = Gate::new(Reservation::Unreachable("connection refused".into()));
    let mut s = open_with(gated(gate, &["127.0.0.1"]), &Limits::default(), None).unwrap();
    s.open(&format!("fetch {}", server.url("/v1")), None).unwrap();
    let f = s.next().unwrap_err();
    assert_eq!(f.tag, FailureTag::Transient, "{f}");
    assert!(server.received().is_empty());

    let granted = Gate::new(Reservation::Granted);
    let mut s = open_with(gated(granted.clone(), &["127.0.0.1"]), &Limits::default(), None).unwrap();
    s.open(&format!("fetch {}", server.url("/v1")), None).unwrap();
    assert!(s.next().unwrap().is_some());
    assert_eq!((granted.asked.load(Ordering::SeqCst), server.received().len()), (1, 1), "one permit covers one request");
}

/// A request the allowlist refuses never reaches the limiter and spends no permit.
// spec: connector.meter.allowlist-precedence@a04fd503
#[test]
fn a_refused_request_spends_no_permit() {
    let server = Server::start(|_| Response::text(200, "vendor"));
    let gate = Gate::new(Reservation::Granted);
    let mut s = open_with(gated(gate.clone(), &["api.vendor.example"]), &Limits::default(), None).unwrap();
    s.open(&format!("fetch {}", server.url("/v1")), None).unwrap();
    let f = s.next().unwrap_err();
    assert!(f.message.starts_with("SecretUnpermittedRequest"), "{f}");
    assert_eq!(gate.asked.load(Ordering::SeqCst), 0);
    assert!(server.received().is_empty());
}

/// A guest swallowing a held-back request still fails its call, and a fault inside the reservation machinery fails
/// the request rather than passing it unmetered.
// spec: connector.meter.held-back-request@cae9fde5
#[test]
fn a_swallowed_held_back_request_still_fails_the_call() {
    let server = Server::start(|_| Response::text(200, "vendor"));
    let gate = Gate::new(Reservation::Unreachable("limiter timed out".into()));
    let mut s = open_with(gated(gate, &["127.0.0.1"]), &Limits::default(), None).unwrap();
    s.open(&format!("swallow {}", server.url("/v1")), None).unwrap();
    let f = s.next().unwrap_err();
    assert!(f.message.starts_with("ConnectorUnmetered"), "{f}");
    assert_eq!(f.tag, FailureTag::Transient);
    assert_eq!(s.traffic().held_back.len(), 1);
    assert!(server.received().is_empty(), "nothing went out unmetered");

    // A swallowed allowlist refusal fails its call the same way.
    let mut s = open_with(Grant { allow: Allowlist::parse(&["api.vendor.example"]).unwrap(), ..loopback() }, &Limits::default(), None).unwrap();
    s.open(&format!("swallow {}", server.url("/v1")), None).unwrap();
    assert!(s.next().unwrap_err().message.starts_with("SecretUnpermittedRequest"));
}
