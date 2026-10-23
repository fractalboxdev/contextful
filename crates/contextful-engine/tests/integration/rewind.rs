//! `run.own` rewind: retiring a pending owner an operator or an applied declaration supersedes.

use crate::support::{at, Rig, T0};
use contextful_core::run::own::{OwnerPins, OwnerScope, PlanPins};
use contextful_core::run::ports::{AwakeableStore, ExecutionPort, OpenExecution, Outcome};
use contextful_core::run::record::RunStatus;
use contextful_core::run::retry::Schedule;
use contextful_core::run::suspend::{Awakeable, AwakeableState};
use contextful_core::run::RunError;
use contextful_engine::stores::FileAwakeableStore;
use contextful_engine::EngineError;
use std::sync::Arc;

const SCOPE: &str = "job:score";

fn pins(plan_ref: &str) -> OwnerPins {
    PlanPins { plan_ref: plan_ref.into(), identities: Default::default() }.into()
}

fn open(plan_ref: &str, run_id: &str) -> OpenExecution {
    OpenExecution {
        scope: OwnerScope::host(SCOPE),
        pins: pins(plan_ref),
        run_id: run_id.into(),
        site_id: "site-a".into(),
        pid: 4242,
        boot_id: "boot-a".into(),
        trace_id: None,
        connector: None,
        schedule: Schedule::single_attempt(),
    }
}

/// Leave a pending owner on plan reference `plan-a`, one recorded step, its lease lapsed; answers its execution id.
fn interrupted(rig: &Rig) -> String {
    let mut x = rig.engine.open_execution(&open("plan-a", "fire-1")).unwrap();
    x.step("read", b"input", &mut |_| Ok(b"rows".to_vec())).unwrap();
    let id = x.execution_id().to_string();
    drop(x);
    rig.clock.advance(60);
    id
}

fn refusal(result: Result<contextful_core::run::record::RunRow, EngineError>) -> RunError {
    match result {
        Err(EngineError::Refused(e)) => e,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// A rewind naming an execution no pending owner holds raises `ExecutionRewindNotPending`.
// spec: run.own.rewind-not-pending@24ffdaca
#[test]
fn a_rewind_of_a_completed_or_unknown_execution_is_not_pending() {
    let rig = Rig::new();
    let x = rig.engine.open_execution(&open("plan-a", "fire-1")).unwrap();
    let done = x.execution_id().to_string();
    x.close(Outcome::Success { rows: 0, bytes: 0, batches: 0 }).unwrap();
    for id in [done.as_str(), "x-unknown"] {
        assert!(matches!(refusal(rig.engine.rewind(id, "operator", true)), RunError::ExecutionRewindNotPending(m) if m.contains(id)));
    }
}

/// A rewind of an owner no attempt has asked another plan reference of raises `ExecutionRewindPlanCurrent`
/// unless `--force` is given.
// spec: run.own.rewind-plan-current@a381eb09
#[test]
fn an_unforced_rewind_of_a_current_owner_refuses_and_a_forced_one_retires_it() {
    let rig = Rig::new();
    let id = interrupted(&rig);
    let current = refusal(rig.engine.rewind(&id, "operator", false));
    assert!(matches!(&current, RunError::ExecutionRewindPlanCurrent(m) if m.contains("--force")), "{current}");
    assert_eq!(rig.catalog().owner_at(&OwnerScope::host(SCOPE)).unwrap().unwrap().execution_id, id);
    let row = rig.engine.rewind(&id, "operator", true).unwrap();
    assert_eq!((row.status, row.execution_id.as_str(), row.plan_ref.as_deref()), (RunStatus::Canceled, id.as_str(), Some("plan-a")));
    assert_eq!(row.started_at, at(T0), "the retirement's row spans the owner's life");
    assert_eq!(row.site_id, "site-a", "the retirement's row carries its last attempt's site");
    assert!(rig.catalog().owner_at(&OwnerScope::host(SCOPE)).unwrap().is_none());
    let fresh = rig.engine.open_execution(&open("plan-a", "fire-2")).unwrap();
    assert!(!fresh.resumed());
    assert_ne!(fresh.execution_id(), id);
}

/// A rewind after a refused open under another plan retires the owner unforced, and the scope's next fire plans fresh.
#[test]
fn a_rewind_after_a_pin_mismatch_lets_the_next_open_plan_fresh() {
    let rig = Rig::new();
    let id = interrupted(&rig);
    assert!(matches!(rig.engine.open_execution(&open("plan-b", "fire-2")), Err(EngineError::Refused(RunError::ExecutionPinMismatch(m))) if m.contains(&format!("contextful run rewind {id} --reason <text>"))));
    let row = rig.engine.rewind(&id, "declaration changed", false).unwrap();
    assert!(row.error_message.unwrap().contains("declaration changed"));
    let x = rig.engine.open_execution(&open("plan-b", "fire-3")).unwrap();
    assert!(!x.resumed());
    assert_eq!(x.close(Outcome::Success { rows: 0, bytes: 0, batches: 0 }).unwrap().status, RunStatus::Success);
    assert_eq!(rig.engine.journal.recorded(&id).unwrap(), 0, "the retired execution's journal is collected");
}

/// A rewind or applied retirement of an owner with an attempt on an unexpired owner lease, a `waiting` attempt, or a
/// pending awakeable raises `ExecutionRewindUnsafe`; `--force` overrides none.
// spec: run.own.rewind-unsafe@b7c7db60
#[test]
fn a_live_or_waiting_attempt_or_pending_awakeable_refuses_every_retirement() {
    let rig = Rig::new();
    let live = rig.engine.open_execution(&open("plan-a", "fire-1")).unwrap();
    let id = live.execution_id().to_string();
    let unsafe_ = |rig: &Rig| {
        assert!(matches!(refusal(rig.engine.rewind(&id, "operator", true)), RunError::ExecutionRewindUnsafe(_)));
        assert!(matches!(rig.engine.retire_superseded(&OwnerScope::host(SCOPE), &|_| true, "applied v2"), Err(EngineError::Refused(RunError::ExecutionRewindUnsafe(_)))));
        assert_eq!(rig.catalog().owner_at(&OwnerScope::host(SCOPE)).unwrap().unwrap().execution_id, id);
    };
    unsafe_(&rig);
    drop(live);
    rig.clock.advance(60);
    rig.catalog().update_run("fire-1", &mut |r| { r.status = RunStatus::Waiting; r.owner = None; Ok(()) }).unwrap();
    unsafe_(&rig);
    rig.catalog().update_run("fire-1", &mut |r| { r.status = RunStatus::PartialFailure; Ok(()) }).unwrap();
    let mut rig = rig;
    let store = Arc::new(FileAwakeableStore::open(rig.dir.path()));
    let now = rig.catalog().now().unwrap();
    store.insert(&Awakeable { token: "aw-1".into(), execution_id: id.clone(), step_label: "callback".into(), created_at: now, ttl_secs: 3600, deadline: now.plus_secs(3600), state: AwakeableState::Pending, payload_sha256: None, payload: None, caller: None }).unwrap();
    rig.engine.awakeables = Some(store);
    unsafe_(&rig);
    rig.clock.advance(7200);
    assert!(rig.engine.rewind(&id, "operator", true).is_ok(), "a lapsed awakeable holds nothing");
}

/// Applied retirement retires an owner only while the predicate names its pins superseded.
#[test]
fn applied_retirement_spares_an_owner_whose_declared_reference_holds() {
    let rig = Rig::new();
    let id = interrupted(&rig);
    let scope = OwnerScope::host(SCOPE);
    assert!(rig.engine.retire_superseded(&scope, &|p| p.declared_ref() != "plan-a", "applied v2").unwrap().is_none());
    assert_eq!(rig.catalog().owner_at(&scope).unwrap().unwrap().execution_id, id);
    let row = rig.engine.retire_superseded(&scope, &|p| p.declared_ref() != "plan-b", "superseded by applied snapshot v2").unwrap().expect("retired");
    assert!(row.error_message.unwrap().contains("superseded by applied snapshot v2"));
    assert!(rig.catalog().owner_at(&scope).unwrap().is_none());
}
