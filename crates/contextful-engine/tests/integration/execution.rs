//! `run.own` and `run.journal`: the execution handle a host opens under a declared scope.

use crate::support::{plan, three_pages, Pages, Rig, Sink, T0};
use contextful_core::run::journal::EntryKey;
use contextful_core::run::own::{OwnerPins, OwnerScope, PlanPins};
use contextful_core::run::ports::{ExecutionPort, OpenExecution, Outcome, Substrate, Wake};
use contextful_core::run::record::RunStatus;
use contextful_core::run::retry::{Backoff, Schedule};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_engine::awake::Registry;
use contextful_engine::stores::FileAwakeableStore;
use contextful_engine::{Engine, EngineError};
use serde_json::json;
use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const SCOPE: &str = "index-42";

fn pins(plan_ref: &str, model: &str) -> OwnerPins {
    PlanPins { plan_ref: plan_ref.into(), identities: BTreeMap::from([("model".to_string(), model.to_string())]) }.into()
}

/// An open of host scope `index-42` as attempt `run_id`.
fn host(plan_ref: &str, model: &str, run_id: &str) -> OpenExecution {
    OpenExecution {
        scope: OwnerScope::host(SCOPE),
        pins: pins(plan_ref, model),
        run_id: run_id.into(),
        site_id: "site-a".into(),
        pid: 4242,
        boot_id: "boot-a".into(),
        trace_id: None,
        connector: None,
        schedule: Schedule::single_attempt(),
    }
}

/// An effect that counts its runs and answers `value`.
fn counted<'a>(runs: &'a AtomicUsize, value: &'a [u8]) -> impl FnMut(&dyn contextful_core::run::ports::Cancellation) -> Result<Vec<u8>, Failure> + 'a {
    move |_| {
        runs.fetch_add(1, Ordering::SeqCst);
        Ok(value.to_vec())
    }
}

/// An execution handle dropped without close records no status; its owner stays pending, and the next open under
/// its scope resumes it once {{run.record.owner-lease}} lapses.
// spec: run.own.unclosed-execution@ec6d4ebb
#[test]
fn a_dropped_execution_resumes_under_its_scope_and_replays_its_steps() {
    let rig = Rig::new();
    let (fetch, embed, store) = (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
    let mut first = String::new();
    let died = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut x = rig.engine.open_execution(&host("plan-a", "m-1", "job-1")).unwrap();
        first = x.execution_id().to_string();
        assert_eq!(x.step("fetch", b"doc-1", &mut counted(&fetch, b"fetched")).unwrap(), b"fetched");
        assert_eq!(x.step("embed", b"fetched", &mut counted(&embed, b"vector")).unwrap(), b"vector");
        // The process dies inside the third effect, holding its claim; no destructor closes the row.
        let _ = x.step("store", b"vector", &mut |_| panic!("the host process dies mid-step"));
    }));
    assert!(died.is_err());

    let row = rig.row("job-1");
    assert_eq!(row.status, RunStatus::Running, "a drop records no status");
    assert_eq!((row.ended_at, row.error_kind), (None, None));
    assert_eq!(row.host_scope.as_deref(), Some(SCOPE));
    assert_eq!((row.pipeline_id.as_str(), row.table.as_str()), ("", ""), "a host run names no pipeline or table");
    let owner = rig.catalog().owner_at(&OwnerScope::host(SCOPE)).unwrap().expect("the owner stays pending");
    assert_eq!(owner.execution_id, first);

    rig.clock.advance(60);
    let before = (fetch.load(Ordering::SeqCst), embed.load(Ordering::SeqCst));
    let mut x = rig.engine.open_execution(&host("plan-a", "m-1", "job-2")).unwrap();
    assert!(x.resumed());
    assert_eq!(x.execution_id(), first, "the resumed attempt keys on the same execution");
    assert_eq!(x.step("fetch", b"doc-1", &mut counted(&fetch, b"other")).unwrap(), b"fetched");
    assert_eq!(x.step("embed", b"fetched", &mut counted(&embed, b"other")).unwrap(), b"vector");
    let replayed_effects = fetch.load(Ordering::SeqCst) + embed.load(Ordering::SeqCst) - before.0 - before.1;
    contextful_eval::record::emit("execution-resume", replayed_effects as f64, 2, 0);
    assert_eq!(replayed_effects, 0, "both recorded steps replay without their effects");
    // The dead attempt's pending claim passes to the resumed one.
    assert_eq!(x.step("store", b"vector", &mut counted(&store, b"stored")).unwrap(), b"stored");
    assert_eq!(store.load(Ordering::SeqCst), 1);

    let row = x.close(Outcome::Success { rows: 0, bytes: 0, batches: 0 }).unwrap();
    assert_eq!(row.status, RunStatus::Success);
    assert!(rig.catalog().owner_at(&OwnerScope::host(SCOPE)).unwrap().is_none(), "success releases the owner");
    assert_eq!(rig.engine.journal.recorded(&first).unwrap(), 0, "retirement collects the journal");
}

/// A run resolves the plan reference it started against for its whole life.
// spec: run.journal.plan-pin@614bbd50
#[test]
fn a_resumed_execution_holds_the_plan_reference_it_started_against() {
    let rig = Rig::new();
    let runs = AtomicUsize::new(0);
    {
        let mut x = rig.engine.open_execution(&host("plan-a", "m-1", "job-1")).unwrap();
        assert_eq!(x.plan_ref(), "plan-a");
        x.step("fetch", b"doc-1", &mut counted(&runs, b"fetched")).unwrap();
    }
    rig.clock.advance(60);

    for (i, (plan_ref, model)) in [("plan-b", "m-1"), ("plan-a", "m-2")].into_iter().enumerate() {
        let run_id = format!("job-x{i}");
        match rig.engine.open_execution(&host(plan_ref, model, &run_id)) {
            Err(EngineError::Refused(RunError::ExecutionPinMismatch(m))) => {
                assert!(m.contains("host scope `index-42`") && m.contains("plan reference plan-a") && m.contains(plan_ref), "{m}");
            }
            Err(e) => panic!("{e}"),
            Ok(_) => panic!("an open under moved pins resumed"),
        }
        let row = rig.row(&run_id);
        assert_eq!(row.status, RunStatus::Failed);
        assert!(row.error_message.unwrap().starts_with("ExecutionPinMismatch"));
    }
    assert_eq!(runs.load(Ordering::SeqCst), 1, "no refused open replays or runs a step");
    let owner = rig.catalog().owner_at(&OwnerScope::host(SCOPE)).unwrap().expect("the pending owner holds");
    assert_eq!(owner.pins, pins("plan-a", "m-1"));

    let mut x = rig.engine.open_execution(&host("plan-a", "m-1", "job-2")).unwrap();
    assert_eq!(x.plan_ref(), "plan-a");
    assert_eq!(x.step("fetch", b"doc-1", &mut counted(&runs, b"other")).unwrap(), b"fetched");
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

/// Every call a host makes, through the port alone: capabilities, open, a retried step, a suspension and its
/// resume, a cursor commit and the close. Returns the execution id and the status it closed on.
fn drive<S: Substrate<Error = EngineError>>(substrate: &S, resolve: &dyn Fn(&str)) -> (String, RunStatus) {
    let caps = substrate.capabilities();
    assert!(caps.hosts("native") && caps.awakeables, "{caps:?}");
    let mut open = host("plan-a", "m-1", "job-1");
    open.schedule = Schedule { backoff: Backoff::Fixed { delay_ms: 1 }, attempts: 3, jitter_ceiling_ms: 0, ..Schedule::default() };
    let mut x = substrate.open(&open).unwrap();
    assert_eq!(x.position(), None);

    let tries = AtomicUsize::new(0);
    let value = x
        .step("call", b"q", &mut |_| match tries.fetch_add(1, Ordering::SeqCst) {
            0 => Err(Failure::new(FailureTag::Transient, "the vendor blinked")),
            _ => Ok(b"answer".to_vec()),
        })
        .unwrap();
    assert_eq!((value.as_slice(), tries.load(Ordering::SeqCst)), (b"answer".as_slice(), 2), "the schedule retried once");

    let token = x.suspend("approval", 3600).unwrap();
    assert_eq!(x.awaited(&token).unwrap(), Wake::Pending);
    resolve(&token);
    assert_eq!(x.awaited(&token).unwrap(), Wake::Resumed(b"yes".to_vec()));

    x.commit(Some(json!({"page": 2}))).unwrap();
    let id = x.execution_id().to_string();
    let row = x.close(Outcome::Success { rows: 3, bytes: 30, batches: 1 }).unwrap();
    (id, row.status)
}

/// The run-path substrate is one interface: open or resume an execution under a scope for a content-hashed plan
/// reference, record a step output, commit a cursor, suspend on an awakeable with a timeout, attach a retry
/// schedule, describe capabilities.
// spec: run.journal.substrate-port@a41ec362
#[test]
fn the_substrate_port_opens_steps_suspends_commits_and_closes_an_execution() {
    let rig = Rig::new();
    let store = Arc::new(FileAwakeableStore::open(rig.dir.path()));
    let engine = Engine { awakeables: Some(store.clone()), ..rig.engine.clone() };
    let registry = Registry::over(store, rig.engine.journal.clone());
    let resolve = |token: &str| {
        registry.resolve(token, b"yes", rig.catalog().now().unwrap()).unwrap();
    };

    let (first, status) = drive(&engine, &resolve);
    assert_eq!(status, RunStatus::Success);
    let row = rig.row("job-1");
    assert_eq!((row.rows, row.bytes, row.batches), (3, 30, 1));
    let scope = OwnerScope::host(SCOPE);
    assert!(rig.catalog().owner_at(&scope).unwrap().is_none(), "the commit retired the owner");
    assert_eq!(rig.catalog().cursor_at(&scope).unwrap().position, Some(json!({"page": 2})));
    assert_eq!(engine.journal.recorded(&first).unwrap(), 0);

    // A later open starts a fresh execution from the committed position.
    let x = engine.open_execution(&host("plan-b", "m-1", "job-2")).unwrap();
    assert!(!x.resumed());
    assert_ne!(x.execution_id(), first);
    assert_eq!(x.position(), Some(&json!({"page": 2})));
}

/// The catalog keys every owner on its scope — live table, backfill chunk or host-declared id — a table owner
/// keeping its stored key byte for byte, and a host owner pinning the plan reference and identities the host
/// supplies.
// spec: run.own.host-scope@41bc251a
#[test]
fn every_owner_is_keyed_on_its_scope_and_a_table_owner_keeps_its_stored_row() {
    let rig = Rig::new();
    let p = plan("kind = \"opaque-token\"", "");
    let c = p.connector_pin("artifact-1");
    // A pending table owner exactly as a table-keyed catalog stored it.
    let stored = format!(
        r#"{{
  "cursor": {{
    "position": null,
    "version": 0,
    "marker_run_id": null,
    "marker_committed_at": null
  }},
  "owner": {{
    "execution_id": "x-stored",
    "pipeline_id": "feed",
    "table": "filings",
    "pins": {{
      "connector": {{
        "id": "{}",
        "version": "{}",
        "world": "{}",
        "hash": "{}"
      }},
      "content_hash": "{}",
      "input_hash": "74234e98afe7498fb5daf1f36ac2d78acc339464f950703b8c019892f982b90b"
    }},
    "attempts": [
      "run-0"
    ],
    "opened_at": "{T0}"
  }}
}}"#,
        c.id, c.version, c.world, c.hash, p.content_hash
    );
    let path = rig.dir.path().join("scopes/feed/filings.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &stored).unwrap();

    let table = rig.catalog().owner("feed", "filings").unwrap().expect("the stored owner reads under its table scope");
    assert_eq!(table.scope, OwnerScope::table("feed", "filings"));
    rig.catalog().put_owner(&table).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), stored, "the table owner's key and row stay byte for byte");

    // A chunk of the same table and a host scope each hold their own owner, beside the table's.
    let chunk = OwnerScope::chunk("feed", "filings", "2030-01");
    let mut other = table.clone();
    other.execution_id = "x-chunk".into();
    other.scope = chunk.clone();
    rig.catalog().put_owner(&other).unwrap();
    let mut x = rig.engine.open_execution(&host("plan-a", "m-1", "job-1")).unwrap();
    x.step("fetch", b"doc-1", &mut |_| Ok(b"fetched".to_vec())).unwrap();
    let hosted = rig.catalog().owner_at(&OwnerScope::host(SCOPE)).unwrap().unwrap();
    assert_eq!(hosted.pins, pins("plan-a", "m-1"));
    assert_eq!(hosted.execution_id, x.execution_id());
    assert_eq!(rig.catalog().owner_at(&chunk).unwrap().unwrap().execution_id, "x-chunk");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), stored, "no other scope touches the table's row");
    drop(x);

    // The stored table owner resumes under the runner.
    let mut source = Pages::new(three_pages());
    let row = rig.run(&p, "1.0.0", "run-1", &mut source, &mut Sink::default()).unwrap();
    assert_eq!((row.status, row.execution_id.as_str()), (RunStatus::Success, "x-stored"));
    assert!(rig.catalog().owner("feed", "filings").unwrap().is_none());
    assert_eq!(rig.catalog().owner_at(&chunk).unwrap().unwrap().execution_id, "x-chunk", "a finishing table releases no chunk");
}

/// Reaching for a capability the running profile does not wire raises `CapabilityUnwired` at the first reach, before
/// any half-finished work.
// spec: run.journal.unwired-capability@aa634203
#[test]
fn suspending_on_an_engine_without_an_awakeable_store_is_refused_at_the_first_reach() {
    let rig = Rig::new();
    assert!(!rig.engine.capabilities().awakeables);
    let mut x = rig.engine.open_execution(&host("plan-a", "m-1", "job-1")).unwrap();
    match x.suspend("approval", 60) {
        Err(EngineError::Refused(RunError::CapabilityUnwired(m))) => assert!(m.contains("awakeable store"), "{m}"),
        other => panic!("{other:?}"),
    }
    let key = EntryKey::new(x.execution_id(), "suspend:approval", &60u64.to_be_bytes());
    assert!(rig.engine.journal.row(&key).unwrap().is_none(), "the refusal claims no journal entry");
    assert_eq!(rig.row("job-1").status, RunStatus::Running);
    assert!(matches!(x.awaited("0000"), Err(EngineError::Refused(RunError::CapabilityUnwired(_)))));
}
