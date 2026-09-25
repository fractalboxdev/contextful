//! `run.cancel`: the stop mark on a run row.

use super::{at, row};
use contextful_core::run::cancel::{mark, stops, Scope, POLL_INTERVAL_MS};
use contextful_core::run::record::RunStatus;
use contextful_core::run::RunError;

/// Marking an already-marked row overwrites it with the newer request.
// spec: run.cancel.re-mark@d92c0b5b
#[test]
fn a_second_mark_overwrites_the_first() {
    let mut r = row("run-1", "2030-01-01T00:00:00Z");
    mark(&mut r, Scope::Run, Some("first".into()), at("2030-01-01T00:00:01Z")).unwrap();
    mark(&mut r, Scope::Pipeline, Some("second".into()), at("2030-01-01T00:00:02Z")).unwrap();
    let stop = r.stop.unwrap();
    assert_eq!(stop.requested_at, at("2030-01-01T00:00:02Z"));
    assert_eq!(stop.scope, "pipeline");
    assert_eq!(stop.reason.as_deref(), Some("second"));
}

#[test]
fn only_an_in_flight_row_accepts_a_mark_and_scopes_read_by_grain() {
    assert_eq!(POLL_INTERVAL_MS, 500);
    let mut r = row("run-1", "2030-01-01T00:00:00Z");
    for s in [RunStatus::Success, RunStatus::Failed, RunStatus::Canceled, RunStatus::PartialFailure] {
        r.status = s;
        assert!(matches!(mark(&mut r, Scope::Run, None, at("2030-01-01T00:00:01Z")), Err(RunError::CancelTargetNotInFlight(_))), "{s}");
        assert!(r.stop.is_none());
    }
    assert_eq!(Scope::read("pipeline"), Scope::Pipeline);
    assert_eq!(Scope::read("fire"), Scope::Run, "an unrecognized stored scope reads as run");
    let mut target = row("run-1", "2030-01-01T00:00:00Z");
    mark(&mut target, Scope::Pipeline, None, at("2030-01-01T00:00:01Z")).unwrap();
    let sibling = row("run-2", "2030-01-01T00:00:00Z");
    let mut elsewhere = row("run-3", "2030-01-01T00:00:00Z");
    elsewhere.pipeline_id = "other".into();
    assert!(stops(&target, &sibling));
    assert!(!stops(&target, &elsewhere));
}
