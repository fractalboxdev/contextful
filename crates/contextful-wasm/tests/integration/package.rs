//! `connector.package`: the per-connector resource bounds.

use crate::support::{host, loopback, open, open_with, text, Response, Server, PROBE, PROBE_BASE};
use contextful_core::connector::package::{Artifact, Digest, PinRequirement};
use contextful_core::run::FailureTag;
use contextful_wasm::limits::{DISCOVERY_DEADLINE, EPOCH_TICK, IN_FLIGHT, READ_DEADLINE, SESSION_LOG_BYTES};
use contextful_wasm::Limits;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The host re-hashes the resolved bytes and raises `ConnectorDigestMismatch` on a difference, before the bytes reach
/// the engine.
// spec: connector.package.digest-mismatch@d9ebfd9b
#[test]
fn bytes_off_their_pin_never_reach_the_compiler() {
    let (host, _, _) = host();
    let pin = Digest::of(PROBE).to_string();
    let artifact = Artifact::parse("https://dl.vendor.example/probe.wasm", Some(&pin)).unwrap();
    let (_, digest) = host.load_artifact(&artifact, PROBE, PinRequirement::default()).unwrap();
    assert_eq!(digest.as_str(), pin);

    // The base probe is a valid component; only the pin keeps it out.
    let f = host.load_artifact(&artifact, PROBE_BASE, PinRequirement::default()).err().expect("refused");
    assert!(f.message.starts_with("ConnectorDigestMismatch"), "{f}");
    assert!(f.message.contains(&pin) && f.message.contains(Digest::of(PROBE_BASE).as_str()), "{f}");
    assert!(f.deterministic);
    let garbage = host.load_artifact(&artifact, b"not a component", PinRequirement::default()).err().expect("refused");
    assert!(garbage.message.starts_with("ConnectorDigestMismatch"), "the pin is judged ahead of compilation: {garbage}");
}

/// A connector runs under 256 MiB of linear memory by default, raised per connector to at most 2 GiB.
// spec: connector.package.linear-memory@6aaddab0
#[test]
fn linear_memory_defaults_to_256_mib_and_rises_to_at_most_2_gib() {
    assert_eq!(Limits::default().memory_bytes, 256 * 1024 * 1024);
    assert!(Limits::default().with_memory(2 * 1024 * 1024 * 1024).is_ok());
    let over = Limits::default().with_memory(2 * 1024 * 1024 * 1024 + 1).unwrap_err();
    assert_eq!(over.tag, FailureTag::Config);

    let capped = Limits::default().with_memory(64 * 1024 * 1024).unwrap();
    let mut s = open_with(loopback(), &capped, None).unwrap();
    s.open("grow", None).unwrap();
    let f = s.next().unwrap_err();
    assert_eq!(f.tag, FailureTag::Transient, "{f}");
    assert!(f.message.contains("trapped"), "{f}");
}

/// A read call carries a 30 s wall-clock deadline and a discovery call a 60 s one, armed by epoch interruption at
/// 100 ms granularity.
// spec: connector.package.call-deadline@f0fb48af
#[test]
fn a_read_call_is_interrupted_at_its_deadline() {
    assert_eq!((READ_DEADLINE, DISCOVERY_DEADLINE, EPOCH_TICK), (Duration::from_secs(30), Duration::from_secs(60), Duration::from_millis(100)));
    assert_eq!((Limits::default().read_deadline, Limits::default().discovery_deadline), (READ_DEADLINE, DISCOVERY_DEADLINE));

    let short = Limits { read_deadline: Duration::from_millis(300), ..Limits::default() };
    let mut s = open_with(loopback(), &short, None).unwrap();
    s.open("spin", None).unwrap();
    let started = Instant::now();
    let f = s.next().unwrap_err();
    assert_eq!(f.tag, FailureTag::Transient, "{f}");
    assert!(f.message.contains("deadline"), "{f}");
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());

    // A discovery call runs under its own deadline: a read deadline below one epoch tick
    // leaves discovery untouched.
    let tight = Limits { read_deadline: Duration::from_millis(1), ..Limits::default() };
    assert_eq!(open_with(loopback(), &tight, None).unwrap().discover().unwrap().len(), 1);
}

/// A session carries a 1 MiB logging budget, dropping and counting messages past it, and holds at most 8 requests
/// outbound at once.
// spec: connector.package.session-budget@7505b6a2
#[test]
fn a_session_logs_within_1_mib_and_holds_8_requests_outbound() {
    assert_eq!((SESSION_LOG_BYTES, IN_FLIGHT), (1024 * 1024, 8));
    let mut s = open();
    s.open("log", None).unwrap();
    assert_eq!(s.next().unwrap(), None, "logging never fails the call");
    let kept: usize = s.logs().iter().map(|l| l.context.len() + l.message.len()).sum();
    assert!(kept <= SESSION_LOG_BYTES, "{kept}");
    assert!(s.logs_dropped() > 0);
    assert_eq!(s.logs().len() as u64 + s.logs_dropped(), 2048);

    let (now, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (n, p) = (now.clone(), peak.clone());
    let server = Server::start(move |_| {
        let at = n.fetch_add(1, Ordering::SeqCst) + 1;
        p.fetch_max(at, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(300));
        n.fetch_sub(1, Ordering::SeqCst);
        Response::text(200, "ok")
    });
    let mut s = open();
    s.open(&format!("burst 12 {}", server.url("/slow")), None).unwrap();
    assert_eq!(text(&s.next().unwrap().unwrap()), "12", "every request is answered");
    let peak = peak.load(Ordering::SeqCst);
    assert!(peak <= IN_FLIGHT, "{peak} requests were outbound at once");
    assert!(peak > 1, "the guest's requests overlap");
}
