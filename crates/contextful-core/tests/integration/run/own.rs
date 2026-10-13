//! `run.own`: when a closing run releases its owner.

use contextful_core::run::own::releases;
use contextful_core::run::record::RunStatus;

#[test]
fn execution_admission_orders_leases_and_refuses_conflicting_fences() {
    use contextful_core::coordinate::{ExecutionAdmission, Lease};
    use contextful_core::time::Instant;
    let lease = |key: &str, fence| Lease { key: key.into(), holder: "holder".into(), fence, expires_at: Instant::from_unix_secs(60).unwrap() };
    let admission = ExecutionAdmission::new(Vec::new(), vec![lease("z", 1), lease("a", 2), lease("a", 2)], false).unwrap();
    assert_eq!(admission.leases().iter().map(|l| l.key.as_str()).collect::<Vec<_>>(), vec!["a", "z"]);
    assert!(ExecutionAdmission::new(Vec::new(), vec![lease("a", 1), lease("a", 2)], false).is_err());
}

/// `success`, and a failure that wrote no batch, release the owner; every other status holds it.
// spec: run.own.pin-release@dbab6ab5
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
