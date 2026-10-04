//! `connector.package`: the per-connector resource bounds.

use crate::support::{host, loopback, open, open_with, text, Response, Server, MEMORIES, PROBE, PROBE_BASE};
use contextful_core::connector::package::{Artifact, Digest, PinRequirement};
use contextful_core::run::FailureTag;
use contextful_wasm::limits::{DISCOVERY_DEADLINE, EPOCH_TICK, IN_FLIGHT, READ_DEADLINE, REQUEST_BODY_BYTES, SESSION_LOG_BYTES};
use contextful_wasm::{ComponentHost, Limits, Target};
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

// spec: connector.package.artifact-cache@192423af
#[test]
fn a_cached_artifact_deserializes_then_recompiles_if_altered_or_incompatible() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("cache");
    let host = ComponentHost::with_cache_dir(Target::Native, &cache).unwrap();
    let pin = Digest::of(PROBE).to_string();
    let artifact = Artifact::parse("https://dl.vendor.example/probe.wasm", Some(&pin)).unwrap();

    host.load_artifact(&artifact, PROBE, PinRequirement::default()).unwrap();
    assert_eq!((host.cache_stats().compilations, host.cache_stats().hits), (1, 0));
    host.load_artifact(&artifact, PROBE, PinRequirement::default()).unwrap();
    assert_eq!((host.cache_stats().compilations, host.cache_stats().hits), (1, 1));

    let entry = std::fs::read_dir(&cache).unwrap().map(|e| e.unwrap().path()).find(|p| p.extension().is_some_and(|x| x == "cwasm")).unwrap();
    std::fs::write(&entry, b"altered cache bytes").unwrap();
    host.load_artifact(&artifact, PROBE, PinRequirement::default()).unwrap();
    assert_eq!((host.cache_stats().compilations, host.cache_stats().hits), (2, 1));

    std::fs::rename(&entry, cache.join("stale-compatibility-hash.cwasm")).unwrap();
    host.load_artifact(&artifact, PROBE, PinRequirement::default()).unwrap();
    assert_eq!((host.cache_stats().compilations, host.cache_stats().hits), (3, 1));
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

    // The cap is one budget across every memory the component instantiates: three
    // memories each under 256 MiB, 281.25 MiB together, refuse at instantiation.
    let (host, _, _) = host();
    let many = host.load(MEMORIES).unwrap();
    let f = host.open(&many, loopback(), &Limits::default(), None).err().expect("refused");
    assert_eq!(f.tag, FailureTag::Transient, "{f}");
    assert!(f.message.contains("linear memory"), "{f}");
    let roomy = Limits::default().with_memory(512 * 1024 * 1024).unwrap();
    let f = host.open(&many, loopback(), &roomy, None).err().expect("not a source connector");
    assert!(f.message.contains("does not load"), "under a larger cap the same component instantiates: {f}");
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

    // The deadline bounds a guest blocked inside a host import: a clock wait the guest
    // sized itself, and an exchange with a vendor slower than the deadline.
    let mut s = open_with(loopback(), &short, None).unwrap();
    s.open("sleep 5000", None).unwrap();
    let started = Instant::now();
    let f = s.next().unwrap_err();
    assert!(f.message.contains("deadline"), "{f}");
    assert!(started.elapsed() < Duration::from_secs(2), "a clock wait outran the deadline: {:?}", started.elapsed());

    let slow = Server::start(|_| {
        std::thread::sleep(Duration::from_secs(4));
        Response::text(200, "late")
    });
    let mut s = open_with(loopback(), &short, None).unwrap();
    s.open(&format!("fetch {}", slow.url("/slow")), None).unwrap();
    let started = Instant::now();
    let f = s.next().unwrap_err();
    assert_eq!(f.tag, FailureTag::Transient, "{f}");
    assert!(f.message.contains("deadline"), "{f}");
    assert!(started.elapsed() < Duration::from_secs(2), "a vendor wait outran the deadline: {:?}", started.elapsed());

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

    // Requests a call abandoned at its deadline stay outbound until the vendor answers,
    // and the instance replacing the overrun one waits for their slots.
    let (now, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (n, p) = (now.clone(), peak.clone());
    let server = Server::start(move |_| {
        let at = n.fetch_add(1, Ordering::SeqCst) + 1;
        p.fetch_max(at, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(1500));
        n.fetch_sub(1, Ordering::SeqCst);
        Response::text(200, "ok")
    });
    let short = Limits { read_deadline: Duration::from_millis(300), ..Limits::default() };
    let mut s = open_with(loopback(), &short, None).unwrap();
    let burst = format!("burst {IN_FLIGHT} {}", server.url("/slow"));
    s.open(&burst, None).unwrap();
    assert!(s.next().unwrap_err().message.contains("deadline"));
    s.open(&burst, None).unwrap();
    assert!(s.next().unwrap_err().message.contains("deadline"));
    std::thread::sleep(Duration::from_millis(2000));
    let peak = peak.load(Ordering::SeqCst);
    assert!(peak <= IN_FLIGHT, "{peak} requests were outbound at once across a replaced instance");
}

/// A guest's outbound request body carries at most 8 MiB. The host stops reading past it and fails the call as a
/// deterministic refusal before the request reaches the vendor.
// spec: connector.package.request-body@6e5f58ea
#[test]
fn an_outbound_body_past_8_mib_fails_the_call_before_the_vendor() {
    assert_eq!(REQUEST_BODY_BYTES, 8 * 1024 * 1024);
    let server = Server::start(|r| Response::text(200, r.header("content-length").unwrap_or("none")));
    let mut s = open();
    s.open(&format!("post 1024 {}", server.url("/v1")), None).unwrap();
    assert_eq!(text(&s.next().unwrap().unwrap()), "200", "a 1 MiB body goes out");
    assert_eq!(server.received()[0].header("content-length"), Some("1048576"));

    let mut s = open();
    s.open(&format!("post 9216 {}", server.url("/v1")), None).unwrap();
    let f = s.next().unwrap_err();
    assert!(f.deterministic, "{f}");
    assert!(f.message.contains("request body"), "{f}");
    assert_eq!(server.received().len(), 1, "the over-long request never reached the vendor");
    assert_eq!(s.traffic().refused.len(), 1);
}
