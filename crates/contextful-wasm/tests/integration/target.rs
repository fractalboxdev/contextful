//! `connector.package`: the instruction set a host runs components on. The interpreted
//! target's suite runs under the `pulley` feature, and its refusal without it.

use crate::support::PROBE;
#[cfg(feature = "pulley")]
use crate::support::{loopback, text, Response, Server};
use contextful_wasm::{ComponentHost, Target};
#[cfg(feature = "pulley")]
use contextful_wasm::Limits;
#[cfg(feature = "pulley")]
use contextful_core::connector::attach::Allowlist;
#[cfg(feature = "pulley")]
use contextful_core::run::FailureTag;
#[cfg(feature = "pulley")]
use contextful_wasm::Grant;
#[cfg(feature = "pulley")]
use std::time::{Duration, Instant};

#[test]
fn the_native_target_is_the_default() {
    assert_eq!(Target::default(), Target::Native);
    let host = ComponentHost::with_target(Target::Native).expect("the native target builds");
    host.load(PROBE).expect("the probe compiles for the native target");
}

/// A host built with the `pulley` feature runs components on the interpreted target as portable bytecode, mapping
/// no executable memory, under the same deadlines and mediated client; the compiled native target stays the default.
// spec: connector.package.interpreted-target@037b0a88
#[cfg(feature = "pulley")]
#[test]
fn the_interpreted_target_runs_the_probe_under_its_deadline_and_mediated_client() {
    let host = ComponentHost::with_target(Target::Pulley).expect("the interpreted target builds");
    let probe = host.load(PROBE).expect("the probe compiles to bytecode");

    let server = Server::start(|_| Response::text(200, "ok"));
    let mut s = host.open(&probe, loopback(), &Limits::default(), None).unwrap();
    s.open(&format!("fetch {}", server.url("/v1")), None).unwrap();
    assert_eq!(text(&s.next().unwrap().unwrap()), "200 ok");
    assert_eq!(server.received().len(), 1, "the request reached the vendor through the mediated client");

    // The allowlist holds: a host the grant omits never receives the request.
    let elsewhere = Grant { allow: Allowlist::parse(&["api.vendor.example"]).unwrap(), ..loopback() };
    let mut s = host.open(&probe, elsewhere, &Limits::default(), None).unwrap();
    s.open(&format!("swallow {}", server.url("/v2")), None).unwrap();
    let f = s.next().unwrap_err();
    assert!(f.message.starts_with("SecretUnpermittedRequest"), "{f}");
    assert_eq!(server.received().len(), 1, "an unlisted host received a request");

    // Epoch interruption reaches interpreted guest code that never yields.
    let short = Limits { read_deadline: Duration::from_millis(300), ..Limits::default() };
    let mut s = host.open(&probe, loopback(), &short, None).unwrap();
    s.open("spin", None).unwrap();
    let started = Instant::now();
    let f = s.next().unwrap_err();
    assert_eq!(f.tag, FailureTag::Transient, "{f}");
    assert!(f.message.contains("deadline"), "{f}");
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
}

#[cfg(feature = "pulley")]
#[test]
#[ignore = "manual native-versus-Pulley throughput measurement"]
fn probe_guest_throughput() {
    for target in [Target::Native, Target::Pulley] {
        let host = ComponentHost::with_target(target).unwrap();
        let probe = host.load(PROBE).unwrap();
        let started = Instant::now();
        let iterations = 100;
        for _ in 0..iterations {
            let mut session = host.open(&probe, loopback(), &Limits::default(), None).unwrap();
            session.open("items", None).unwrap();
            while session.next().unwrap().is_some() {}
        }
        let elapsed = started.elapsed();
        eprintln!("{target:?}: {iterations} probe sessions in {elapsed:?}; {:.1} sessions/s", iterations as f64 / elapsed.as_secs_f64());
    }
}

/// A host built without the `pulley` feature refuses the interpreted target at construction, naming the feature,
/// before any component compiles.
// spec: connector.package.interpreted-target-absent@ae3dba10
#[cfg(not(feature = "pulley"))]
#[test]
fn a_build_without_the_feature_refuses_the_interpreted_target() {
    let f = ComponentHost::with_target(Target::Pulley).err().expect("refused without the feature");
    assert!(f.message.contains("`pulley` feature"), "{f}");
}
