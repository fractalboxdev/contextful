//! `run.suspend`: the deadline and single-valued resolution against an injected instant.

use super::at;
use contextful_core::run::suspend::{Awakeable, AwakeableState, Resolution};
use contextful_core::run::RunError;

fn minted() -> Awakeable {
    Awakeable::mint("tok-1", "x-1", "approve", "2030-01-01T00:00:00Z", 60).unwrap()
}

/// A deadline is the creation instant plus a time-to-live, both caller-supplied RFC3339 Zulu strings; the core
/// reads no wall clock.
// spec: run.suspend.deadline@4fe90b97
#[test]
fn the_deadline_is_creation_plus_ttl_from_caller_instants() {
    let a = minted();
    assert_eq!(a.created_at, at("2030-01-01T00:00:00Z"));
    assert_eq!(a.deadline, at("2030-01-01T00:01:00Z"));
    assert_eq!(serde_json::to_value(&a).unwrap()["deadline"], "2030-01-01T00:01:00Z");
    // A creation instant outside Zulu is refused rather than read under a guessed zone.
    assert!(matches!(Awakeable::mint("t", "x", "s", "2030-01-01T08:00:00+08:00", 60), Err(RunError::Invalid(_))));
    // An instant decades from any wall clock evaluates the same way: only the injected instant counts.
    let mut past = Awakeable::mint("t", "x", "s", "1990-01-01T00:00:00Z", 60).unwrap();
    assert_eq!(past.evaluate(at("1990-01-01T00:00:59Z")), AwakeableState::Pending);
}

/// Expiry is evaluated against an injected instant; a suspension past its deadline becomes `timed_out` and stays
/// so under any later instant.
// spec: run.suspend.timeout-is-sticky@648c4a2c
#[test]
fn a_timeout_sticks_under_any_later_instant() {
    let mut a = minted();
    assert_eq!(a.evaluate(at("2030-01-01T00:00:59Z")), AwakeableState::Pending);
    assert_eq!(a.evaluate(at("2030-01-01T00:01:00Z")), AwakeableState::TimedOut);
    // An earlier instant read afterwards does not revive it.
    assert_eq!(a.evaluate(at("2030-01-01T00:00:10Z")), AwakeableState::TimedOut);
    assert_eq!(a.evaluate(at("2031-01-01T00:00:00Z")), AwakeableState::TimedOut);
}

/// Resolving a token again with the identical payload returns the recorded value.
// spec: run.suspend.idempotent-resolution@96d13cc8
#[test]
fn an_identical_second_resolution_returns_the_recorded_value() {
    let mut a = minted();
    assert_eq!(a.resolve(b"{\"approved\":true}", at("2030-01-01T00:00:30Z")).unwrap(), Resolution::First);
    let recorded = a.payload.clone();
    // Even past the deadline, the identical payload reads the recorded value.
    assert_eq!(a.resolve(b"{\"approved\":true}", at("2030-01-01T00:05:00Z")).unwrap(), Resolution::Recorded);
    assert_eq!(a.payload, recorded);
    assert_eq!(a.state, AwakeableState::Resolved);
}

/// A second resolution with a different payload raises `AwakeableAlreadyResolved` and leaves the recorded value
/// untouched.
// spec: run.suspend.conflicting-resolution@546c0da1
#[test]
fn a_different_second_payload_is_refused_and_the_first_stands() {
    let mut a = minted();
    a.resolve(b"yes", at("2030-01-01T00:00:30Z")).unwrap();
    let before = a.clone();
    assert!(matches!(a.resolve(b"no", at("2030-01-01T00:00:31Z")), Err(RunError::AwakeableAlreadyResolved(_))));
    assert_eq!(a, before);
}

/// Resolving a token past its deadline raises `AwakeableTimedOut`.
// spec: run.suspend.expired-token@895b15b7
#[test]
fn a_resolution_past_the_deadline_is_refused() {
    let mut a = minted();
    match a.resolve(b"late", at("2030-01-01T00:01:00Z")) {
        Err(RunError::AwakeableTimedOut(m)) => assert!(m.contains("tok-1"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(a.state, AwakeableState::TimedOut);
    assert!(a.payload.is_none());
}
