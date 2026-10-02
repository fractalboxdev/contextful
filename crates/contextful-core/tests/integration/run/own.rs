//! `run.own`: when a closing run releases its owner.

use contextful_core::run::own::releases;
use contextful_core::run::record::RunStatus;

/// `success`, and a failure that wrote no batch, release the owner; every other status holds it.
// spec: run.own.pin-release@910426de
#[test]
fn success_and_an_empty_failure_release_every_other_status_holds() {
    assert!(releases(RunStatus::Success, 0));
    assert!(releases(RunStatus::Success, 3));
    assert!(releases(RunStatus::Failed, 0));
    assert!(!releases(RunStatus::Failed, 1));
    for s in [RunStatus::PartialFailure, RunStatus::Pending, RunStatus::Running, RunStatus::Waiting] {
        assert!(!releases(s, 0), "{s}");
        assert!(!releases(s, 2), "{s}");
    }
}
