//! `run.suspend.caller-binding`: a resume token bound at mint to a verified caller subject.

use crate::support::at;
use contextful_core::run::suspend::AwakeableState;
use contextful_core::run::RunError;
use contextful_engine::awake::{AwakeError, Awaited, Registry};
use contextful_engine::Journal;

fn registry(dir: &std::path::Path) -> Registry {
    Registry::open(dir, Journal::open(dir))
}

fn unknown<T: std::fmt::Debug>(out: Result<T, AwakeError>, token: &str) {
    match out {
        Err(AwakeError::Refused(RunError::AwakeableUnknown(m))) => assert!(m.contains(token), "{m}"),
        other => panic!("a wrong caller read {other:?}"),
    }
}

/// An awakeable may bind a verified caller subject at mint; resolving it then takes the token and a credential
/// for that subject, and any other caller answers as {{run.suspend.unknown-token}}. An unbound token is its whole
/// authority.
#[test]
fn a_bound_token_resolves_only_for_its_subject_and_answers_others_as_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let r = registry(dir.path());
    let bound = r.suspend_for("x-1", "approve", "2030-01-01T00:00:00Z", 600, Some("reviewer@acme")).unwrap();
    assert_eq!(bound.caller.as_deref(), Some("reviewer@acme"), "the subject persists on the row");
    let now = at("2030-01-01T00:01:00Z");

    // The token alone, or a credential for another subject, reaches nothing and changes nothing.
    for caller in [None, Some("intruder@acme")] {
        unknown(r.state_as(&bound.token, caller, now), &bound.token);
        unknown(r.resolve_as(&bound.token, caller, b"approved", now), &bound.token);
    }
    unknown(r.resolve(&bound.token, b"approved", now), &bound.token);
    assert_eq!(r.state_as(&bound.token, Some("reviewer@acme"), now).unwrap().state, AwakeableState::Pending, "no wrong caller resolved it");
    assert_eq!(r.awaited("x-1", &bound.token, now).unwrap(), Awaited::Pending);

    // The bound subject's credential resolves it.
    assert_eq!(r.resolve_as(&bound.token, Some("reviewer@acme"), b"approved", now).unwrap(), b"approved");
    assert_eq!(r.awaited("x-1", &bound.token, now).unwrap(), Awaited::Resumed(b"approved".to_vec()));

    // An unbound token is its whole authority: any caller holding it resolves it.
    let open = r.suspend("x-1", "notify", "2030-01-01T00:00:00Z", 600).unwrap();
    assert_eq!(open.caller, None);
    assert_eq!(r.resolve_as(&open.token, Some("anyone@acme"), b"seen", now).unwrap(), b"seen");
}
