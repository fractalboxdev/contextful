//! `run.own` and `run.journal`: the execution handle a host opens under a declared scope.

use crate::support::{at, plan, three_pages, Pages, Rig, Sink, T0};
use contextful_core::run::cancel::Scope;
use contextful_core::run::journal::EntryKey;
use contextful_core::run::own::{OwnerPins, OwnerScope, PlanPins};
use contextful_core::run::ports::{ExecutionPort, OpenExecution, Outcome, Substrate, Wake};
use contextful_core::run::ports::Cancellation;
use contextful_core::run::record::{RunStatus, OWNER_LEASE_TTL_SECS};
use contextful_core::run::retry::{Backoff, Schedule};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_engine::awake::Registry;
use contextful_engine::cancel::{Cadence, Keeper};
use contextful_engine::stores::FileAwakeableStore;
use contextful_engine::{Engine, EngineError};
use serde_json::json;
use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const SCOPE: &str = "index-42";

fn pins(plan_ref: &str, model: &str) -> OwnerPins {
    PlanPins { plan_ref: plan_ref.into(), identities: BTreeMap::from([("model".to_string(), model.to_string())]) }.into()
}

/// An open of host scope `index-42` as attempt `run_id`.
fn host(plan_ref: &str, model: &str, run_id: &str) -> OpenExecution {
    host_in(SCOPE, plan_ref, model, run_id)
}

/// An open of host scope `scope` as attempt `run_id`.
fn host_in(scope: &str, plan_ref: &str, model: &str, run_id: &str) -> OpenExecution {
    OpenExecution {
        scope: OwnerScope::host(scope),
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

/// An open under a host scope whose pending owner has an attempt on an unexpired {{run.record.owner-lease}} fails
/// `Transient` naming that attempt, and its run row closes `failed` without joining the owner.
// spec: run.own.live-owner@bba71945
#[test]
fn a_host_open_under_a_live_attempt_fails_transient_and_joins_nothing() {
    let rig = Rig::new();
    let runs = AtomicUsize::new(0);
    let mut held = rig.engine.open_execution(&host("plan-a", "m-1", "job-a")).unwrap();
    held.step("fetch", b"doc-1", &mut counted(&runs, b"fetched")).unwrap();

    match rig.engine.open_execution(&host("plan-a", "m-1", "job-b")) {
        Err(EngineError::Failure(f)) => {
            assert_eq!(f.tag, FailureTag::Transient);
            assert!(f.to_string().contains("job-a") && f.to_string().contains("host scope `index-42`"), "{f}");
        }
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("a second live handle shares the owner"),
    }
    let refused = rig.row("job-b");
    assert_eq!((refused.status, refused.error_kind), (RunStatus::Failed, Some(FailureTag::Transient)));
    let owner = rig.catalog().owner_at(&OwnerScope::host(SCOPE)).unwrap().unwrap();
    assert_eq!(owner.attempts, vec!["job-a".to_string()], "the refused attempt joins no owner");
    assert_eq!(owner.execution_id, held.execution_id());

    // The held attempt keeps its owner and journal, and retires them on its own close.
    assert_eq!(held.step("fetch", b"doc-1", &mut counted(&runs, b"other")).unwrap(), b"fetched");
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    held.commit(Some(json!({"page": 1}))).unwrap();
    assert_eq!(held.close(Outcome::Success { rows: 0, bytes: 0, batches: 0 }).unwrap().status, RunStatus::Success);
    assert!(rig.catalog().owner_at(&OwnerScope::host(SCOPE)).unwrap().is_none());
}

/// A `pipeline`-scoped stop on a host execution's run halts every in-flight run under the same host-declared
/// scope and no run of another scope.
// spec: run.cancel.host-grain@0479ff0d
#[test]
fn a_pipeline_stop_on_a_host_run_halts_only_its_own_scope() {
    let rig = Rig::new();
    // `job-0` dies holding `index-42`; its row stays in flight after its lease lapses.
    drop(rig.engine.open_execution(&host("plan-a", "m-1", "job-0")).unwrap());
    rig.clock.advance(60);
    let a = rig.engine.open_execution(&host("plan-a", "m-1", "job-a")).unwrap();
    let b = rig.engine.open_execution(&host_in("unrelated-scope", "plan-a", "m-1", "job-b")).unwrap();

    let mut marked = rig.engine.cancel("job-a", Scope::Pipeline, None).unwrap();
    marked.sort();
    assert_eq!(marked, vec!["job-0".to_string(), "job-a".to_string()]);
    assert!(rig.row("job-b").stop.is_none(), "another host scope's run carries no mark");
    drop((a, b));
}

/// Poll `holds` every 5 ms until it answers true, failing with `what` after 10 s.
fn eventually(what: &str, holds: impl Fn() -> bool) {
    let started = std::time::Instant::now();
    while !holds() {
        assert!(started.elapsed() < Duration::from_secs(10), "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// One keeper per engine renews every registered execution's owner lease and feeds its token, sleeping until the
/// earliest deadline; opening an execution registers it, and close or drop deregisters it.
// spec: run.cancel.engine-keeper@06bef068
#[test]
fn one_keeper_thread_renews_and_feeds_every_open_execution_of_an_engine() {
    let rig = Rig::new();
    let quick = Duration::from_millis(20);
    let engine = Engine { keeper: Keeper::new(Cadence { poll: quick, renew: quick }), ..rig.engine.clone() };
    assert_eq!(engine.keeper.threads(), 0, "an engine with nothing open runs no keeper thread");

    let mut open: Vec<_> = (0..20).map(|i| engine.open_execution(&host_in(&format!("index-{i}"), "plan-a", "m-1", &format!("job-{i}"))).unwrap()).collect();
    assert_eq!((engine.keeper.threads(), engine.keeper.registered()), (1, 20), "20 open executions share one keeper thread");
    contextful_eval::record::emit("engine-keeper-threads", engine.keeper.threads() as f64, 20, 0);

    // Every registered lease renews against the catalog clock.
    rig.clock.advance(25);
    let renewed = at(T0).plus_secs(25 + OWNER_LEASE_TTL_SECS);
    eventually("every open execution's lease renews", || {
        (0..20).all(|i| rig.row(&format!("job-{i}")).owner.is_some_and(|o| o.lease_expires_at >= renewed))
    });

    // A stop on one run fires that run's token and no other.
    engine.cancel("job-7", Scope::Run, None).unwrap();
    eventually("the stop reaches job-7's token", || open[7].token().requested());
    assert!(open.iter().enumerate().all(|(i, x)| i == 7 || !x.token().requested()), "a stop fired another run's token");

    // Close deregisters; so does a drop, which records nothing.
    for x in open.split_off(10) {
        assert_eq!(x.close(Outcome::Success { rows: 0, bytes: 0, batches: 0 }).unwrap().status, RunStatus::Success);
    }
    assert_eq!(engine.keeper.registered(), 10);
    drop(open);
    assert_eq!(engine.keeper.registered(), 0);
    eventually("the keeper thread exits once nothing is registered", || engine.keeper.threads() == 0);
    let dropped = rig.row("job-3");
    assert_eq!((dropped.status, dropped.ended_at), (RunStatus::Running, None), "a drop records no status");

    // A dropped execution's lease is no longer renewed, so it lapses and the next open resumes its owner.
    rig.clock.advance(OWNER_LEASE_TTL_SECS as i64 + 1);
    std::thread::sleep(Duration::from_millis(60));
    assert!(rig.row("job-3").owner.is_some_and(|o| o.expired(rig.catalog().now().unwrap())), "a dropped execution's lease still renews");
    let x = engine.open_execution(&host_in("index-3", "plan-a", "m-1", "job-3b")).unwrap();
    assert!(x.resumed(), "the next open under the scope takes over after the lease lapses");
    assert_eq!(engine.keeper.threads(), 1);
}

/// Two escape hatches exist and no third: `journal.unsafe(label, effect)` for an idempotent read, and a source
/// declaring that it journals no pull. Each carries its idempotency argument at a greppable call site.
// spec: run.journal.escape-hatch@273348e9
#[test]
fn an_unsafe_read_reruns_on_every_replay_and_each_hatch_states_its_argument() {
    let rig = Rig::new();
    let reads = AtomicUsize::new(0);
    let mut x = rig.engine.open_execution(&host("plan-a", "m-1", "job-1")).unwrap();
    let id = x.execution_id().to_string();
    assert_eq!(x.r#unsafe("peek", &mut counted(&reads, b"v1")).unwrap(), b"v1");
    assert_eq!(x.r#unsafe("peek", &mut counted(&reads, b"v2")).unwrap(), b"v2", "an unsafe read resolves no recorded value");
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    assert_eq!(rig.engine.journal.recorded(&id).unwrap(), 0, "an unsafe read records nothing");
    assert_eq!(x.step("fetch", b"doc", &mut counted(&reads, b"fetched")).unwrap(), b"fetched");
    assert_eq!(rig.engine.journal.recorded(&id).unwrap(), 1, "a step beside it still records");
    drop(x);

    // The second hatch: the sources the one constant lists, each with its argument.
    for (source, why) in contextful_core::pipeline::declare::UNJOURNALED_SOURCES {
        assert!(why.split_whitespace().count() >= 5, "`{source}` states why replaying its pull needs no recorded batch");
    }
    // Every `unsafe` call site in the tree states why its read is idempotent on the line above it.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for dir in ["crates", "tools"] {
        for entry in walk(&root.join(dir)) {
            let text = std::fs::read_to_string(&entry).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if line.contains(".r#unsafe(") && !entry.to_string_lossy().contains("/tests/") {
                    let above = i.checked_sub(1).map(|j| lines[j].trim()).unwrap_or_default();
                    assert!(above.starts_with("// idempotent:"), "{}:{} carries no `// idempotent:` argument", entry.display(), i + 1);
                }
            }
        }
    }
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() && name != "target" && name != "node_modules" {
            out.extend(walk(&p));
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
    out
}

/// A body replays faithfully when every observable side effect passes through a recorded step, a cursor commit or an
/// awakeable; the work between them is pure.
// spec: run.journal.effect-boundary@31a98cb7
#[test]
fn a_body_resumed_across_a_step_and_an_awakeable_commits_what_an_uninterrupted_one_commits() {
    // The body: a recorded fetch, an awakeable, pure work over both, then a cursor commit.
    fn body<X: ExecutionPort<Error = EngineError>>(x: &mut X, fetches: &AtomicUsize, tokens: &Mutex<Vec<String>>) -> Option<serde_json::Value> {
        let fetched = x.step("fetch", b"doc-1", &mut counted(fetches, b"41")).unwrap();
        let token = x.suspend("approval", 3600).unwrap();
        tokens.lock().unwrap().push(token.clone());
        let Wake::Resumed(payload) = x.awaited(&token).unwrap() else { return None };
        let n: u64 = String::from_utf8(fetched).unwrap().parse().unwrap();
        let position = json!({ "page": n + 1, "approved": String::from_utf8(payload).unwrap() });
        x.commit(Some(position.clone())).unwrap();
        Some(position)
    }
    let setup = || {
        let rig = Rig::new();
        let store = Arc::new(FileAwakeableStore::open(rig.dir.path()));
        let engine = Engine { awakeables: Some(store.clone()), ..rig.engine.clone() };
        let registry = Registry::over(store, rig.engine.journal.clone());
        (rig, engine, registry)
    };

    // Uninterrupted: the awakeable resolves before the body awaits it.
    let (rig, engine, registry) = setup();
    let (fetches, tokens) = (AtomicUsize::new(0), Mutex::new(Vec::new()));
    let mut x = engine.open_execution(&host("plan-a", "m-1", "job-1")).unwrap();
    let fetched = x.step("fetch", b"doc-1", &mut counted(&fetches, b"41")).unwrap();
    let token = x.suspend("approval", 3600).unwrap();
    registry.resolve(&token, b"yes", rig.catalog().now().unwrap()).unwrap();
    drop((fetched, x));
    rig.clock.advance(60);
    let mut x = engine.open_execution(&host("plan-a", "m-1", "job-2")).unwrap();
    let uninterrupted = body(&mut x, &fetches, &tokens).expect("the body completes");

    // Interrupted: the process dies while the awakeable is pending, and a later attempt resumes.
    let (rig, engine, registry) = setup();
    let (fetches, tokens) = (AtomicUsize::new(0), Mutex::new(Vec::new()));
    let mut x = engine.open_execution(&host("plan-a", "m-1", "job-1")).unwrap();
    assert_eq!(body(&mut x, &fetches, &tokens), None, "the awakeable is pending");
    drop(x);
    registry.resolve(&tokens.lock().unwrap()[0], b"yes", rig.catalog().now().unwrap()).unwrap();
    rig.clock.advance(60);
    let mut x = engine.open_execution(&host("plan-a", "m-1", "job-2")).unwrap();
    assert!(x.resumed());
    let resumed = body(&mut x, &fetches, &tokens).expect("the resumed body completes");

    assert_eq!(resumed, uninterrupted, "the resumed body commits what the uninterrupted one commits");
    assert_eq!(fetches.load(Ordering::SeqCst), 1, "the recorded fetch ran once across both attempts");
    let tokens = tokens.lock().unwrap();
    assert_eq!(tokens[0], tokens[1], "the resumed attempt awaits the awakeable the first one minted");
    assert_eq!(rig.catalog().cursor_at(&OwnerScope::host(SCOPE)).unwrap().position, Some(resumed));
}
