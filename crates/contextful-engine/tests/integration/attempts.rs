//! `run.retry.attempt-counter`: an in-flight schedule's attempt counter on its step's
//! journal row.

use crate::support::Rig;
use contextful_core::run::journal::{EntryKey, Row};
use contextful_core::run::own::{OwnerPins, OwnerScope, PlanPins};
use contextful_core::run::ports::{ExecutionPort, OpenExecution};
use contextful_core::run::retry::ScheduleSpec;
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_engine::EngineError;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};

fn open(run_id: &str) -> OpenExecution {
    // Three attempts, 1 ms apart, no jitter.
    let schedule = ScheduleSpec { backoff: Some("fixed".into()), base_ms: Some(1), attempts: Some(3), jitter_ms: Some(0), ..ScheduleSpec::default() }.compile().unwrap();
    OpenExecution {
        scope: OwnerScope::host("vendor-sync"),
        pins: OwnerPins::from(PlanPins { plan_ref: "plan-a".into(), identities: Default::default() }),
        run_id: run_id.into(),
        site_id: "site-a".into(),
        pid: 4242,
        boot_id: "boot-a".into(),
        trace_id: None,
        connector: None,
        schedule,
    }
}

/// An in-flight schedule's attempt counter persists on its step's journal row, so a crash mid-backoff resumes the
/// remaining budget instead of restarting it.
// spec: run.retry.attempt-counter@c00bb878
#[test]
fn a_crash_mid_schedule_resumes_the_remaining_attempts() {
    let rig = Rig::new();
    let calls = AtomicUsize::new(0);
    let mut execution = String::new();
    let died = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut x = rig.engine.open_execution(&open("job-1")).unwrap();
        execution = x.execution_id().to_string();
        // Attempt 1 fails and the schedule backs off; the process dies in attempt 2.
        let _ = x.step("charge", b"order-9", &mut |_| match calls.fetch_add(1, Ordering::SeqCst) {
            0 => Err(Failure::new(FailureTag::Transient, "the vendor reset the connection")),
            _ => panic!("the host process dies mid-schedule"),
        });
    }));
    assert!(died.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let key = EntryKey::new(&execution, "charge", b"order-9");
    match rig.engine.journal.row(&key).unwrap() {
        Some(Row::Pending { run_id, attempts, .. }) => assert_eq!((run_id.as_str(), attempts), ("job-1", 1), "the closed attempt persists on the step's row"),
        other => panic!("the step's row reads {other:?}"),
    }

    // The resumed run spends only the two attempts left of three.
    rig.clock.advance(60);
    let resumed = AtomicUsize::new(0);
    let mut x = rig.engine.open_execution(&open("job-2")).unwrap();
    assert_eq!(x.execution_id(), execution, "the resume keys on the same execution");
    let failed = x.step("charge", b"order-9", &mut |_| {
        resumed.fetch_add(1, Ordering::SeqCst);
        Err(Failure::new(FailureTag::Transient, "the vendor still resets"))
    });
    assert!(matches!(&failed, Err(EngineError::Refused(RunError::StepFailed { label, failure })) if label == "charge" && failure.tag == FailureTag::Transient), "{failed:?}");
    assert_eq!(resumed.load(Ordering::SeqCst), 2, "the budget resumed at attempt 2 instead of restarting at 1");
    assert!(rig.engine.journal.row(&key).unwrap().is_none(), "a schedule that closed leaves no count behind");
}
