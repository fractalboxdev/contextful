//! `run.callback-guard`: guards bound to the rows they protect, their recorded effect
//! identity, release after the holder stops, reconciliation and inspection.

use crate::Fixture;
use contextful_core::coordinate::{Catalog, ExecutionAdmission, ExecutionExpectation, Lease, LeaseKey};
use contextful_core::run::journal::EntryKey;
use contextful_core::run::own::{ConnectorPin, ExecutionOwner, OwnerScope, Pins};
use contextful_core::run::record::{Owner as RunOwner, Phase, RunRow, RunStatus};
use contextful_core::run::FailureTag;
use contextful_core::time::Instant;
use contextful_d1::{D1Catalog, D1, GUARD_PARENT_VAR};
use serde_json::{json, Value};
use std::sync::Once;
use std::time::Duration;

/// The serve identity every catalog of this test binary registers under.
const PARENT: &str = "serve-identity-under-test";

/// Each catalog this binary connects names [`PARENT`], as a serve process's child does.
fn parented() {
    static SET: Once = Once::new();
    SET.call_once(|| std::env::set_var(GUARD_PARENT_VAR, PARENT));
}

fn database() -> (Fixture, D1) {
    parented();
    let fixture = Fixture::start();
    let db = D1::connect(&fixture.endpoint).unwrap();
    db.initialize_recovery("sample", "generation").unwrap();
    (fixture, db)
}

fn catalog(fixture: &Fixture) -> D1Catalog {
    D1Catalog::connect(&fixture.endpoint).unwrap()
}

fn pins() -> contextful_core::run::own::OwnerPins {
    Pins {
        connector: ConnectorPin { id: "source".into(), version: "1".into(), world: "native".into(), hash: "hash".into() },
        content_hash: "plan".into(),
        input_hash: "input".into(),
    }
    .into()
}

fn run_row(id: &str, execution: &str, owner: Option<RunOwner>) -> RunRow {
    RunRow {
        run_id: id.into(),
        pipeline_id: "feed".into(),
        table: "filings".into(),
        site_id: "site".into(),
        status: RunStatus::Running,
        owner,
        started_at: Instant::parse("2026-01-01T00:00:00Z").unwrap(),
        ended_at: None,
        rows: 0,
        bytes: 0,
        batches: 0,
        skipped: 0,
        fetched: 0,
        kept: 0,
        declined: Default::default(),
        audit: vec![],
        error_kind: None,
        error_message: None,
        connector_id: "source".into(),
        connector_version: "1".into(),
        connector_hash: "hash".into(),
        trace_id: None,
        phase: Phase::Plan,
        pipeline_phase: None,
        execution_id: execution.into(),
        stop: None,
        host_scope: None,
        plan_ref: None,
        input: None,
        failed_branches: vec![],
        credential_providers: Default::default(),
        bound_hits: Vec::new(),
        next_attempt_at: None,
        unpushed: None,
    }
}

/// Seat `execution` as the owner of host scope `scope` under its running attempt `run`,
/// its owner lease unexpired.
fn seat(catalog: &D1Catalog, scope: &str, execution: &str, run: &str) -> ExecutionExpectation {
    let owner = ExecutionOwner {
        execution_id: execution.into(),
        scope: OwnerScope::host(scope),
        pins: pins(),
        attempts: vec![run.into()],
        opened_at: Instant::parse("2026-01-01T00:00:00Z").unwrap(),
        next_attempt_at: None,
    };
    catalog.put_owner(&owner).unwrap();
    catalog.put_run(&run_row(run, execution, Some(RunOwner::leased(1, "boot", catalog.now().unwrap())))).unwrap();
    ExecutionExpectation { scope: owner.scope.clone(), execution_id: execution.into(), pins: pins(), run_id: run.into() }
}

/// Lapse the owner lease of attempt `run`, as a holder that misses its renewals leaves it.
fn lapse(catalog: &D1Catalog, run: &str) {
    catalog
        .update_run(run, &mut |row| {
            row.owner = Some(RunOwner::leased(1, "boot", Instant::parse("2026-01-01T00:00:00Z").unwrap()));
            Ok(())
        })
        .unwrap()
        .unwrap()
        .unwrap();
}

/// The next attempt of `expected`'s execution taking over its owner row.
fn next_attempt(expected: &ExecutionExpectation, run: &str) -> ExecutionOwner {
    ExecutionOwner {
        execution_id: expected.execution_id.clone(),
        scope: expected.scope.clone(),
        pins: pins(),
        attempts: vec![expected.run_id.clone(), run.into()],
        opened_at: Instant::parse("2026-01-01T00:00:00Z").unwrap(),
        next_attempt_at: None,
    }
}

fn rows(db: &D1, sql: &str) -> Vec<Value> {
    db.query(sql, vec![]).unwrap().rows
}

fn entity(catalog: &D1Catalog, holder: &str) -> Lease {
    catalog
        .acquire(&LeaseKey::Partition { pipeline: "job".into(), partition: "entity-a".into() }, holder, 90)
        .unwrap()
        .unwrap()
}

/// A guard on one item blocks its owner, its ancestor and its entity lease, naming its token,
/// class and `reconcile-guard`, while another job's admission and an unrelated export proceed.
// spec: run.callback-guard.scope@59e6ef5f
// spec: run.callback-guard.blocked@bc18612f
#[test]
fn a_guard_protects_its_item_its_ancestor_and_its_entity_lease_alone() {
    let (fixture, db) = database();
    let holder = catalog(&fixture);
    let item = seat(&holder, "item:a", "item-execution", "item-run");
    let job = seat(&holder, "job", "job-execution", "job-run");
    let fence = entity(&holder, "item-run");
    let admission = ExecutionAdmission::new(vec![item.clone(), job.clone()], vec![fence.clone()], false).unwrap();
    let guard = holder.execution_guard(&admission).unwrap();
    let protected = rows(&db, "SELECT scope FROM contextful_guard ORDER BY scope");
    assert_eq!(protected.len(), 3, "item, ancestor and entity lease: {protected:?}");

    let other = catalog(&fixture);
    let token = other.guards().unwrap()[0].token.clone();
    let refusals = [
        other.put_owner(&next_attempt(&item, "taker")).unwrap_err(),
        other.put_owner(&next_attempt(&job, "taker")).unwrap_err(),
        other.take_over(&fence_key(), "taker", 90).unwrap_err(),
    ];
    for refusal in &refusals {
        assert_eq!(refusal.tag, FailureTag::Transient, "{refusal}");
        let message = refusal.to_string();
        assert!(message.contains(&token) && message.contains("(unknown)") && message.contains("contextful storage reconcile-guard"), "{message}");
    }

    let another_job = seat(&other, "job-b", "job-b-execution", "job-b-run");
    let admitted = other.execution_guard(&ExecutionAdmission::new(vec![another_job], vec![], false).unwrap()).unwrap();
    let export = other.acquire(&LeaseKey::Cadence("export".into()), "exporter", 90).unwrap().unwrap();
    let exporting = other.execution_guard(&ExecutionAdmission::new(vec![], vec![export], true).unwrap()).unwrap();
    assert_eq!(other.guards().unwrap().len(), 3, "three guards held together");
    drop((admitted, exporting, guard));
    other.put_owner(&next_attempt(&item, "taker")).unwrap();
    other.take_over(&fence_key(), "taker", 90).unwrap();
    assert!(rows(&db, "SELECT scope FROM contextful_guard").is_empty());
}

fn fence_key() -> LeaseKey {
    LeaseKey::Partition { pipeline: "job".into(), partition: "entity-a".into() }
}

/// A guard on a job scope alone leaves the item owner beneath it free to transfer, and the
/// item's next callback, which admits the job row too, waits on it.
// spec: run.callback-guard.scope@59e6ef5f
#[test]
fn a_job_scope_guard_lets_an_item_owner_beneath_it_transfer() {
    let (fixture, _db) = database();
    let catalog = catalog(&fixture);
    let item = seat(&catalog, "item:a", "item-execution", "item-run");
    let job = seat(&catalog, "job", "job-execution", "job-run");
    let publication = catalog.execution_guard(&ExecutionAdmission::new(vec![job.clone()], vec![], true).unwrap()).unwrap();
    catalog.put_owner(&next_attempt(&item, "item-run-2")).unwrap();
    catalog.put_run(&run_row("item-run-2", "item-execution", Some(RunOwner::leased(1, "boot", catalog.now().unwrap())))).unwrap();
    let next = ExecutionExpectation { run_id: "item-run-2".into(), ..item };
    let refused = catalog.execution_guard(&ExecutionAdmission::new(vec![next.clone(), job.clone()], vec![], false).unwrap()).err().expect("the shared job row admits no second callback");
    assert_eq!(refused.tag, FailureTag::Transient, "{refused}");
    drop(publication);
    catalog.execution_guard(&ExecutionAdmission::new(vec![next, job], vec![], false).unwrap()).unwrap();
}

/// Callbacks on disjoint rows hold guards together; one sharing a row admits nothing and
/// binds none of its rows, and a stale fence or superseded attempt admits nothing.
// spec: run.callback-guard.acquire@083b66e1
#[test]
fn callbacks_on_disjoint_rows_run_together_and_a_shared_row_admits_one() {
    let (fixture, db) = database();
    let catalog = catalog(&fixture);
    let a = seat(&catalog, "item:a", "a", "a-run");
    let b = seat(&catalog, "item:b", "b", "b-run");
    let shared = seat(&catalog, "job", "job", "job-run");
    let first = catalog.execution_guard(&ExecutionAdmission::new(vec![a.clone(), shared.clone()], vec![], false).unwrap()).unwrap();
    let second = catalog.execution_guard(&ExecutionAdmission::new(vec![b.clone()], vec![], false).unwrap()).unwrap();
    assert_eq!(rows(&db, "SELECT scope FROM contextful_guard").len(), 3);
    assert!(catalog.execution_guard(&ExecutionAdmission::new(vec![b.clone(), shared.clone()], vec![], false).unwrap()).is_err());
    assert_eq!(rows(&db, "SELECT scope FROM contextful_guard").len(), 3, "a refused admission binds none of its rows");
    assert_eq!(rows(&db, "SELECT token FROM contextful_callback_guard").len(), 2);
    drop((first, second));
    let lease = entity(&catalog, "a-run");
    let stale = Lease { fence: lease.fence + 1, ..lease.clone() };
    assert!(catalog.execution_guard(&ExecutionAdmission::new(vec![a.clone()], vec![stale], false).unwrap()).is_err());
    let superseded = ExecutionExpectation { run_id: "an-older-run".into(), ..a.clone() };
    assert!(catalog.execution_guard(&ExecutionAdmission::new(vec![superseded], vec![], false).unwrap()).is_err());
    assert!(rows(&db, "SELECT scope FROM contextful_guard").is_empty());
    catalog.execution_guard(&ExecutionAdmission::new(vec![a], vec![lease], false).unwrap()).unwrap();
}

/// A guard records its execution, attempt, holder, entry key, idempotency key, admission
/// time and window, and inspection prints them with its protected rows and evidence class.
// spec: run.callback-guard.identity@4a643aac
// spec: run.callback-guard.inspect@9e37be9b
// spec: run.callback-guard.holder-identity@bcec075d
// spec: run.callback-guard.class@bba3aabd
#[test]
fn a_guard_records_its_effect_identity_and_inspection_prints_it() {
    let (fixture, db) = database();
    let catalog = catalog(&fixture);
    let item = seat(&catalog, "item:a", "item-execution", "item-run");
    assert!(rows(&db, "SELECT holder FROM contextful_guard_holder").is_empty(), "no holder registers before its first guard");
    let fence = entity(&catalog, "item-run");
    let entry = EntryKey::new("item-execution", "pull", b"page-1");
    let before = Instant::from_unix_nanos(catalog.now().unwrap().unix_nanos() - 1_000_000_000).unwrap();
    let windowed = catalog
        .execution_guard(&ExecutionAdmission::new(vec![item.clone()], vec![fence.clone()], false).unwrap().with_effect(entry.clone(), Some(600)))
        .unwrap();
    let holders = rows(&db, "SELECT holder, exited_at FROM contextful_guard_holder");
    assert_eq!(holders.len(), 1);
    assert_eq!(holders[0]["holder"], catalog.holder_identity());
    assert!(holders[0]["exited_at"].is_null());
    let views = catalog.guards().unwrap();
    assert_eq!(views.len(), 1);
    let view = &views[0];
    assert_eq!(view.scopes, vec![item.scope.clone()]);
    assert_eq!(view.lease_keys, vec![fence.key.clone()]);
    assert_eq!(view.execution, "item-execution");
    assert_eq!(view.attempt.as_deref(), Some("item-run"));
    assert_eq!(view.holder, catalog.holder_identity());
    assert_eq!(view.entry_key, entry);
    assert_eq!(view.idempotency_key, entry.idempotency_key());
    assert!(view.admitted_at >= before);
    assert_eq!(view.window, json!(600));
    assert_eq!(view.class.as_str(), "replay-safe");
    drop(windowed);

    let job = seat(&catalog, "job", "job-execution", "job-run");
    let publication = catalog.execution_guard(&ExecutionAdmission::new(vec![job], vec![], true).unwrap()).unwrap();
    let step = catalog.execution_guard(&ExecutionAdmission::new(vec![item], vec![fence], false).unwrap().with_effect(entry, None)).unwrap();
    let mut classes: Vec<(String, Value)> = catalog.guards().unwrap().into_iter().map(|v| (v.class.as_str().to_string(), v.window)).collect();
    classes.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(classes, vec![("receipt-required".to_string(), json!("none")), ("unknown".to_string(), json!("none"))]);
    let printed = serde_json::to_value(catalog.guards().unwrap()).unwrap();
    for key in ["token", "scopes", "lease_keys", "execution", "attempt", "holder", "entry_key", "idempotency_key", "admitted_at", "window", "class"] {
        assert!(printed[0].get(key).is_some(), "inspection prints `{key}`: {printed}");
    }
    drop((publication, step));
}

/// A window covers the key from admission until admission plus the window, then nothing.
// spec: run.callback-guard.window@e5d51d05
#[test]
fn a_window_covers_its_key_from_admission_until_it_ends() {
    let (fixture, _db) = database();
    let catalog = catalog(&fixture);
    let item = seat(&catalog, "item:a", "item-execution", "item-run");
    let entry = EntryKey::new("item-execution", "pull", b"page-1");
    let _guard = catalog.execution_guard(&ExecutionAdmission::new(vec![item], vec![], false).unwrap().with_effect(entry, Some(1))).unwrap();
    assert_eq!(catalog.guards().unwrap()[0].class.as_str(), "replay-safe");
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(catalog.guards().unwrap()[0].class.as_str(), "unknown");
}

/// A holder's guard leaves both its protected rows and its identity when its callback returns.
// spec: run.callback-guard.release@6750f8fe
#[test]
fn a_holder_deletes_its_own_guard_when_its_callback_returns() {
    let (fixture, db) = database();
    let catalog = catalog(&fixture);
    let item = seat(&catalog, "item:a", "item-execution", "item-run");
    let guard = catalog.execution_guard(&ExecutionAdmission::new(vec![item.clone()], vec![], false).unwrap()).unwrap();
    assert_eq!(rows(&db, "SELECT token FROM contextful_callback_guard").len(), 1);
    drop(guard);
    assert!(rows(&db, "SELECT scope FROM contextful_guard").is_empty());
    assert!(rows(&db, "SELECT token FROM contextful_callback_guard").is_empty());
    catalog.put_owner(&next_attempt(&item, "item-run-2")).unwrap();
}

/// A holder records its exit on stop, a serve process records its reaped child's, and a
/// lapsed owner lease records none.
// spec: run.callback-guard.holder-gone@fd6ab002
#[test]
fn a_holder_is_gone_only_once_its_identity_records_an_exit() {
    let (fixture, db) = database();
    let stopping = catalog(&fixture);
    let item = seat(&stopping, "item:a", "a", "a-run");
    drop(stopping.execution_guard(&ExecutionAdmission::new(vec![item], vec![], false).unwrap()).unwrap());
    let stopped = stopping.holder_identity().to_string();
    drop(stopping);
    let exited = |holder: &str| !db.query("SELECT exited_at FROM contextful_guard_holder WHERE holder = ?", vec![json!(holder)]).unwrap().rows[0]["exited_at"].is_null();
    assert!(exited(&stopped), "a holder records its exit on stop");

    let killed = catalog(&fixture);
    let item = seat(&killed, "item:b", "b", "b-run");
    std::mem::forget(killed.execution_guard(&ExecutionAdmission::new(vec![item], vec![], false).unwrap()).unwrap());
    lapse(&killed, "b-run");
    let holder = killed.holder_identity().to_string();
    assert!(!exited(&holder), "a lapsed owner lease records no exit");
    let serve = catalog(&fixture);
    serve.record_child_exit("another-serve", std::process::id()).unwrap();
    assert!(!exited(&holder), "another serve's reaping records nothing");
    serve.record_child_exit(PARENT, std::process::id()).unwrap();
    assert!(exited(&holder), "the reaping serve records its child's exit");
}

/// A guard over a gone holder and a lapsed owner lease releases without an operator only
/// while its window covers its key, and that release commits with the takeover.
// spec: run.callback-guard.replay@ffbf0b39
#[test]
fn a_gone_holder_s_replay_safe_guard_releases_with_the_takeover() {
    let (fixture, db) = database();
    let holder = catalog(&fixture);
    let covered = seat(&holder, "item:covered", "covered", "covered-run");
    let unwindowed = seat(&holder, "item:none", "none", "none-run");
    let passed = seat(&holder, "item:passed", "passed", "passed-run");
    let entry = EntryKey::new("covered", "pull", b"page-1");
    for (expected, window, key) in [(&covered, Some(600), entry.clone()), (&unwindowed, None, EntryKey::new("none", "pull", b"page-1")), (&passed, Some(1), EntryKey::new("passed", "pull", b"page-1"))] {
        let admission = ExecutionAdmission::new(vec![expected.clone()], vec![], false).unwrap().with_effect(key, window);
        std::mem::forget(holder.execution_guard(&admission).unwrap());
    }
    let recorded = holder.guards().unwrap().into_iter().find(|g| g.execution == "covered").unwrap();
    for run in ["covered-run", "none-run", "passed-run"] {
        lapse(&holder, run);
    }
    let taker = catalog(&fixture);
    // A lapsed owner lease whose holder records no exit leaves every guard.
    for expected in [&covered, &unwindowed, &passed] {
        assert!(taker.put_owner(&next_attempt(expected, "taker-run")).is_err());
    }
    assert_eq!(rows(&db, "SELECT token FROM contextful_callback_guard").len(), 3);
    taker.record_child_exit(PARENT, std::process::id()).unwrap();
    std::thread::sleep(Duration::from_millis(1200));
    // A `none` window and a passed one leave their guards for an operator.
    for expected in [&unwindowed, &passed] {
        let refused = taker.put_owner(&next_attempt(expected, "taker-run")).unwrap_err().to_string();
        assert!(refused.contains("contextful storage reconcile-guard"), "{refused}");
    }
    taker.put_owner(&next_attempt(&covered, "taker-run")).unwrap();
    assert_eq!(taker.owner_at(&covered.scope).unwrap().unwrap().attempts.last().map(String::as_str), Some("taker-run"));
    assert!(db.query("SELECT 1 FROM contextful_callback_guard WHERE token = ?", vec![json!(recorded.token)]).unwrap().rows.is_empty());
    assert_eq!(rows(&db, "SELECT token FROM contextful_callback_guard").len(), 2);
    // The replay re-enters the recorded entry, so its request carries the recorded key.
    assert_eq!(EntryKey::new("covered", "pull", b"page-1").idempotency_key(), recorded.idempotency_key);
}

/// Reconciliation of a guard whose owner lease is unexpired and whose holder records no exit
/// raises `ExecutionGuardHeld` and leaves it.
// spec: run.callback-guard.live-holder@7bb8a500
#[test]
fn reconciling_a_live_holder_s_guard_raises_execution_guard_held() {
    let (fixture, db) = database();
    let holder = catalog(&fixture);
    let item = seat(&holder, "item:a", "a", "a-run");
    let entry = EntryKey::new("a", "pull", b"page-1");
    std::mem::forget(holder.execution_guard(&ExecutionAdmission::new(vec![item], vec![], false).unwrap().with_effect(entry.clone(), None)).unwrap());
    let token = holder.guards().unwrap()[0].token.clone();
    let operator = catalog(&fixture);
    let held = operator.reconcile_guard(&token, &format!("receipt:{}:r-1", entry.idempotency_key()), "ops").unwrap_err().to_string();
    assert!(held.contains("ExecutionGuardHeld"), "{held}");
    assert_eq!(rows(&db, "SELECT token FROM contextful_callback_guard").len(), 1);
    lapse(&holder, "a-run");
    operator.reconcile_guard(&token, &format!("receipt:{}:r-1", entry.idempotency_key()), "ops").unwrap();
}

/// A holder whose container stops before serve reaps it records no exit: its guard holds
/// against takeover while the owner lease runs and after it lapses, and only reconciliation
/// with matching evidence releases it, once the lease lapses.
// spec: run.callback-guard.container-stop@fdff388d
#[test]
fn a_stopped_container_s_holder_keeps_its_guard_until_reconciled_after_its_lease_lapses() {
    let (fixture, db) = database();
    let stopped = catalog(&fixture);
    let item = seat(&stopped, "item:a", "a", "a-run");
    let entry = EntryKey::new("a", "pull", b"page-1");
    std::mem::forget(stopped.execution_guard(&ExecutionAdmission::new(vec![item.clone()], vec![], false).unwrap().with_effect(entry.clone(), Some(600))).unwrap());
    let holder = stopped.holder_identity().to_string();
    let token = stopped.guards().unwrap()[0].token.clone();
    let evidence = format!("receipt:{}:r-1", entry.idempotency_key());
    // The container stops: no exit hook runs and no serve process reaps the holder.
    std::mem::forget(stopped);
    let exited = || !rows(&db, "SELECT exited_at FROM contextful_guard_holder").iter().all(|r| r["exited_at"].is_null());
    let registered = rows(&db, "SELECT holder FROM contextful_guard_holder");
    assert_eq!(registered, vec![json!({"holder": holder})], "the stopped holder registered before its guard");

    let operator = catalog(&fixture);
    assert!(operator.put_owner(&next_attempt(&item, "taker")).is_err(), "an unexpired owner lease admits no takeover");
    let held = operator.reconcile_guard(&token, &evidence, "ops").unwrap_err().to_string();
    assert!(held.contains("ExecutionGuardHeld"), "{held}");

    lapse(&operator, "a-run");
    assert!(!exited(), "a lapsed owner lease records no exit");
    let refused = operator.put_owner(&next_attempt(&item, "taker")).unwrap_err().to_string();
    assert!(refused.contains(&token), "a replay-safe guard holds while its holder records no exit: {refused}");
    assert_eq!(rows(&db, "SELECT token FROM contextful_callback_guard"), vec![json!({"token": token})]);

    operator.reconcile_guard(&token, &evidence, "on-call").unwrap();
    assert!(operator.guards().unwrap().is_empty());
    operator.put_owner(&next_attempt(&item, "taker")).unwrap();
}

/// Evidence releases a guard only when it carries the guard's recorded key: an idempotency
/// key within its window, or a receipt; anything else raises `ExecutionGuardEvidenceMismatch`.
// spec: run.callback-guard.evidence@8be2c9ea
#[test]
fn evidence_not_tied_to_the_recorded_key_raises_execution_guard_evidence_mismatch() {
    let (fixture, db) = database();
    let holder = catalog(&fixture);
    let unwindowed = seat(&holder, "item:none", "none", "none-run");
    let windowed = seat(&holder, "item:window", "window", "window-run");
    let passed = seat(&holder, "item:passed", "passed", "passed-run");
    let key = |execution: &str| EntryKey::new(execution, "pull", b"page-1");
    for (expected, window) in [(&unwindowed, None), (&windowed, Some(600)), (&passed, Some(1))] {
        let admission = ExecutionAdmission::new(vec![expected.clone()], vec![], false).unwrap().with_effect(key(&expected.execution_id), window);
        std::mem::forget(holder.execution_guard(&admission).unwrap());
        lapse(&holder, &expected.run_id);
    }
    let token = |execution: &str| holder.guards().unwrap().into_iter().find(|g| g.execution == execution).unwrap().token;
    let operator = catalog(&fixture);
    let mismatch = |token: &str, evidence: &str| {
        let refused = operator.reconcile_guard(token, evidence, "ops").unwrap_err().to_string();
        assert!(refused.contains("ExecutionGuardEvidenceMismatch"), "{evidence}: {refused}");
    };
    let none = token("none");
    let recorded = key("none").idempotency_key();
    mismatch(&none, "idempotency:another-key");
    mismatch(&none, &format!("idempotency:{recorded}"));
    mismatch(&none, "receipt:another-key:r-1");
    mismatch(&none, &format!("receipt:{recorded}:"));
    mismatch(&none, "a provider said so");
    assert_eq!(rows(&db, "SELECT token FROM contextful_callback_guard").len(), 3, "mismatched evidence leaves every guard");
    operator.reconcile_guard(&none, &format!("receipt:{recorded}:r-1"), "ops").unwrap();
    operator.reconcile_guard(&token("window"), &format!("idempotency:{}", key("window").idempotency_key()), "ops").unwrap();
    std::thread::sleep(Duration::from_millis(1200));
    mismatch(&token("passed"), &format!("idempotency:{}", key("passed").idempotency_key()));
    assert_eq!(rows(&db, "SELECT token FROM contextful_callback_guard").len(), 1);
}

/// A manual release deletes the guard and appends an audit row naming its token, evidence and
/// operator in one batch; nothing else an operator sends releases it.
// spec: run.callback-guard.manual@790459e8
#[test]
fn a_manual_reconciliation_deletes_the_guard_and_appends_its_audit_row() {
    let (fixture, db) = database();
    let holder = catalog(&fixture);
    let item = seat(&holder, "item:a", "a", "a-run");
    let entry = EntryKey::new("a", "pull", b"page-1");
    std::mem::forget(holder.execution_guard(&ExecutionAdmission::new(vec![item.clone()], vec![], true).unwrap().with_effect(entry.clone(), None)).unwrap());
    lapse(&holder, "a-run");
    let token = holder.guards().unwrap()[0].token.clone();
    let operator = catalog(&fixture);
    assert!(operator.put_owner(&next_attempt(&item, "taker")).is_err(), "a lapsed lease alone releases nothing");
    assert!(operator.reconcile_guard(&"0".repeat(64), &format!("receipt:{}:r-1", entry.idempotency_key()), "ops").is_err());
    assert!(operator.reconcile_guard(&token, &format!("receipt:{}:r-1", entry.idempotency_key()), " ").is_err(), "an audit row names its operator");
    let evidence = format!("receipt:{}:r-1", entry.idempotency_key());
    operator.reconcile_guard(&token, &evidence, "on-call").unwrap();
    let audit = rows(&db, "SELECT token, evidence, operator FROM contextful_guard_reconciliation");
    assert_eq!(audit, vec![json!({"token": token, "evidence": evidence, "operator": "on-call"})]);
    assert!(rows(&db, "SELECT scope FROM contextful_guard").is_empty());
    assert!(operator.guards().unwrap().is_empty());
    operator.put_owner(&next_attempt(&item, "taker")).unwrap();
}

/// A schema-3 database holding a `global` guard stays at schema 3, its refusal naming the
/// token and the step; once that guard is gone, initialization reaches schema 4.
// spec: run.callback-guard.legacy-guard@1a0f8b0e
#[test]
fn a_schema_3_global_guard_holds_the_database_at_schema_3_and_names_the_step() {
    let (_fixture, db) = database();
    let token = "c".repeat(64);
    db.query("UPDATE contextful_execution_schema SET version = 3 WHERE id = 1", vec![]).unwrap();
    db.query("INSERT INTO contextful_guard (scope, token, admitted_at) VALUES ('global', ?, 1)", vec![json!(token)]).unwrap();
    let refused = db.initialize_recovery("sample", "generation").unwrap_err();
    assert!(refused.starts_with("ExecutionSchemaIncompatible"), "{refused}");
    assert!(refused.contains(&token) && refused.contains("reconcile it with the release that wrote it") && refused.contains("contextful storage initialize"), "{refused}");
    assert_eq!(rows(&db, "SELECT version FROM contextful_execution_schema")[0]["version"], 3);
    db.query("DELETE FROM contextful_guard WHERE scope = 'global'", vec![]).unwrap();
    db.initialize_recovery("sample", "generation").unwrap();
    assert_eq!(rows(&db, "SELECT version FROM contextful_execution_schema")[0]["version"], contextful_d1::EXECUTION_SCHEMA_VERSION);
}
