//! A scratch engine over a temporary directory, a settable clock, a scripted source and
//! an in-memory destination.

use contextful_core::coordinate::{Cas, Catalog, CursorRow, ExecutionAdmission, ExecutionGuard, Lease, LeaseKey, LeaseRow};
use contextful_core::ports::Clock;
use contextful_core::run::backfill::ChunkRow;
use contextful_core::run::own::{ConnectorPin, ExecutionOwner, OwnerScope};
use contextful_core::run::plan::Plan;
use contextful_core::run::ports::{Cancellation, Commit, DeadLetter, Destination, Landed, Marker, Part, PullRequest, Row, Source, Stage};
use contextful_core::run::record::{HistoryPage, RunRow, Window};
use contextful_core::run::{Failure, RunError};
use contextful_core::time::Instant;
use contextful_engine::cancel::{Cadence, Keeper};
use contextful_engine::{Engine, EngineError, Journal, LocalCatalog, RunSpec};
use contextful_engine::stores::{FileBlobStore, FileJournalStore};
use serde_json::{json, Value};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

pub const T0: &str = "2030-01-01T00:00:00Z";

/// A clock a test moves by hand.
#[derive(Clone)]
pub struct SetClock(Arc<AtomicI64>);

impl SetClock {
    pub fn new(s: &str) -> SetClock {
        SetClock(Arc::new(AtomicI64::new(at(s).unix_secs())))
    }

    pub fn advance(&self, secs: i64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }
}

impl Clock for SetClock {
    fn now(&self) -> Instant {
        Instant::from_unix_secs(self.0.load(Ordering::SeqCst)).unwrap()
    }
}

pub struct Rig {
    pub dir: tempfile::TempDir,
    pub clock: SetClock,
    pub engine: Engine,
}

impl Rig {
    pub fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let clock = SetClock::new(T0);
        let journal = Journal::over(FileJournalStore::open(dir.path()), FileBlobStore::open(dir.path()));
        let catalog = Arc::new(LocalCatalog::open(dir.path(), Arc::new(clock.clone())));
        let engine = Engine { catalog, journal, awakeables: None, keeper: Keeper::new(Cadence { poll: Duration::from_millis(20), renew: Duration::from_secs(10) }), emitter: None, worlds: Vec::new() };
        Rig { dir, clock, engine }
    }

    pub fn catalog(&self) -> &dyn Catalog {
        self.engine.catalog.as_ref()
    }

    pub fn row(&self, run_id: &str) -> RunRow {
        self.catalog().run(run_id).unwrap().unwrap_or_else(|| panic!("no run `{run_id}`"))
    }

    /// Run `plan` as `run_id`, admitting connector version `version`.
    pub fn run(&self, plan: &Plan, version: &str, run_id: &str, source: &mut dyn Source, dest: &mut dyn Destination) -> Result<RunRow, EngineError> {
        let spec = RunSpec {
            connector: ConnectorPin { version: version.into(), ..plan.connector_pin("artifact-1") },
            plan: plan.clone(),
            run_id: run_id.into(),
            site_id: "site-a".into(),
            pid: 4242,
            boot_id: "boot-a".into(),
            trace_id: None,
        };
        self.engine.run(&spec, source, dest)
    }

    /// Run until the source or destination panics, as a process dying mid-step leaves its state.
    pub fn crash(&self, plan: &Plan, run_id: &str, source: &mut dyn Source, dest: &mut dyn Destination) {
        let out = std::panic::catch_unwind(AssertUnwindSafe(|| self.run(plan, "1.0.0", run_id, source, dest)));
        assert!(out.is_err(), "the run was expected to die: {out:?}");
    }
}

/// A plan over table `filings` with the given cursor block and extra top-level lines.
pub fn plan(cursor: &str, extra: &str) -> Plan {
    let text = format!(
        "pipeline = \"feed\"\ntable = \"filings\"\n{extra}\n[connector]\nid = \"vendor\"\nversion = \"1.0.0\"\ncommand = [\"vendor\"]\n[cursor]\n{cursor}\n"
    );
    Plan::compile(text.as_bytes()).unwrap()
}

/// Every call a source served: the position asked for and the idempotency key sent.
pub type Calls = Arc<Mutex<Vec<(Option<Value>, String)>>>;

/// A paged vendor: page `n` answers `rows[n]` and continues to `p{n+1}`; it panics
/// after serving the page named in `die_after`, before the runner can record it.
pub struct Pages {
    pub pages: Vec<Vec<Value>>,
    pub calls: Calls,
    pub die_after: Option<usize>,
    pub fail: Vec<Failure>,
}

impl Pages {
    pub fn new(pages: Vec<Vec<Value>>) -> Pages {
        Pages { pages, calls: Arc::default(), die_after: None, fail: Vec::new() }
    }

    pub fn calls(&self) -> Vec<(Option<Value>, String)> {
        self.calls.lock().unwrap().clone()
    }
}

impl Source for Pages {
    fn pull(&mut self, request: &PullRequest, _cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        self.calls.lock().unwrap().push((request.position.clone(), request.idempotency_key.clone()));
        if !self.fail.is_empty() {
            return Err(self.fail.remove(0));
        }
        let n = match &request.position {
            None => 0,
            Some(Value::String(p)) => p.trim_start_matches('p').parse::<usize>().unwrap(),
            Some(other) => panic!("position {other}"),
        };
        let rows = self.pages.get(n).cloned().unwrap_or_default();
        let more = n + 1 < self.pages.len();
        let body = json!({ "rows": rows, "cursor": format!("p{}", (n + 1).min(self.pages.len())), "more": more });
        if self.die_after == Some(n) {
            self.die_after = None;
            panic!("the process dies after the vendor served page {n}");
        }
        Ok(serde_json::to_vec(&body).unwrap())
    }
}

/// A commit as the sink holds it: the rows of every part it names, in part order.
#[derive(Debug, Clone)]
pub struct Committed {
    pub pipeline_id: String,
    pub table: String,
    pub run_id: String,
    pub batches: Vec<Vec<Row>>,
    pub cursor: Option<Value>,
    pub cursor_kind: contextful_core::run::advance::CursorKind,
    pub committed_at: Instant,
}

/// The store stand-in: every stage and commit it accepted, and whether it dies after the
/// next commit.
#[derive(Default)]
pub struct Sink {
    pub commits: Vec<Committed>,
    /// Every batch staged, committed or not, in stage order.
    pub staged: Vec<Stage>,
    pub die_after_land: bool,
    /// Counts to report instead of the ones the commit carries.
    pub report: Option<Landed>,
    /// Whether recording a lease's fence refuses.
    pub refuse_fence: bool,
    /// Bytes each staged part measures.
    pub part_bytes: u64,
    /// Observes each stage as it arrives.
    pub on_stage: Option<Box<dyn FnMut(&Stage)>>,
    /// Every run whose staged parts a discard removed, as `(table, run_id)`.
    pub discarded: Vec<(String, String)>,
    /// Whether every table replaces its source state.
    pub replace: bool,
    /// Every dead-letter stage, as `(table, run_id, letters)`.
    pub dead_letters: Vec<(String, String, Vec<DeadLetter>)>,
}

impl Destination for Sink {
    fn replaces(&self, _: &str) -> bool {
        self.replace
    }

    fn stage_batch(&mut self, stage: Stage) -> Result<Part, Failure> {
        if let Some(observe) = self.on_stage.as_mut() {
            observe(&stage);
        }
        let part = Part { name: format!("{}/{}", stage.run_id, stage.ordinal), rows: stage.rows.len() as u64, bytes: self.part_bytes };
        self.staged.push(stage);
        Ok(part)
    }

    fn commit(&mut self, commit: Commit, precommit: &dyn Fn() -> Result<(), Failure>) -> Result<Landed, Failure> {
        precommit()?;
        let batches: Vec<Vec<Row>> = commit
            .parts
            .iter()
            .map(|p| {
                let stage = self.staged.iter().find(|s| format!("{}/{}", s.run_id, s.ordinal) == p.name).unwrap_or_else(|| panic!("no staged part `{}`", p.name));
                stage.rows.clone()
            })
            .collect();
        let rows = commit.parts.iter().map(|p| p.rows).sum();
        let bytes = commit.parts.iter().map(|p| p.bytes).sum();
        self.commits.push(Committed {
            pipeline_id: commit.pipeline_id,
            table: commit.table,
            run_id: commit.run_id,
            batches,
            cursor: commit.cursor,
            cursor_kind: commit.cursor_kind,
            committed_at: commit.committed_at,
        });
        if std::mem::take(&mut self.die_after_land) {
            panic!("the process dies after the commit marker lands");
        }
        Ok(self.report.unwrap_or(Landed { rows, bytes }))
    }

    fn stage_dead_letters(&mut self, table: &str, run_id: &str, _: &str, letters: &[DeadLetter]) -> Result<(), Failure> {
        self.dead_letters.push((table.to_string(), run_id.to_string(), letters.to_vec()));
        Ok(())
    }

    fn discard(&mut self, table: &str, run_id: &str) -> Result<(), Failure> {
        self.discarded.push((table.to_string(), run_id.to_string()));
        Ok(())
    }

    fn open_fence(&mut self, _: &str, _: &str, _: u64) -> Result<(), Failure> {
        if self.refuse_fence {
            return Err(Failure::new(contextful_core::run::FailureTag::Storage, "the store refused the fence record"));
        }
        Ok(())
    }

    fn newest_marker(&self, pipeline_id: &str, table: &str) -> Result<Option<Marker>, Failure> {
        Ok(self
            .commits
            .iter()
            .filter(|c| c.pipeline_id == pipeline_id && c.table == table)
            .max_by_key(|c| c.committed_at)
            .map(|c| Marker { run_id: c.run_id.clone(), cursor: c.cursor.clone(), committed_at: c.committed_at }))
    }
}

/// Ids of the rows of every batch, per batch.
pub fn ids(commit: &Committed) -> Vec<Vec<String>> {
    commit.batches.iter().map(|b| b.iter().map(|r| r["id"].as_str().unwrap_or_default().to_string()).collect()).collect()
}

/// Three pages: two rows, one row, one row.
pub fn three_pages() -> Vec<Vec<Value>> {
    vec![vec![json!({"id": "d1"}), json!({"id": "d2"})], vec![json!({"id": "d3"})], vec![json!({"id": "d4"})]]
}

/// A catalog forwarding every one of the 29 `Catalog` methods, defaults included, to
/// `inner`, recording each call in order with the arguments a replay against another
/// catalog needs.
pub struct Counted<C> {
    pub inner: C,
    trace: Mutex<Vec<Value>>,
}

/// The calls that change a row; every other call reads.
pub const CATALOG_WRITES: &[&str] =
    &["acquire", "release", "renew", "cursor_cas_at", "cursor_cas", "put_owner", "retire_at", "retire", "put_chunk", "delete_chunks", "update_chunk", "put_run", "update_run"];

/// A lease key as the trace spells it.
pub fn lease_key_json(key: &LeaseKey) -> Value {
    match key {
        LeaseKey::Pipeline(p) => json!({ "pipeline": p }),
        LeaseKey::Partition { pipeline, partition } => json!({ "partition": [pipeline, partition] }),
        LeaseKey::Compaction(t) => json!({ "compaction": t }),
        LeaseKey::Cadence(d) => json!({ "cadence": d }),
        LeaseKey::Dispatch(u) => json!({ "dispatch": u }),
    }
}

impl<C: Catalog> Counted<C> {
    pub fn new(inner: C) -> Counted<C> {
        Counted { inner, trace: Mutex::default() }
    }

    /// Every call recorded since the last take, oldest first.
    pub fn take(&self) -> Vec<Value> {
        std::mem::take(&mut *self.trace.lock().unwrap())
    }

    fn note(&self, op: &str, args: Value) {
        self.trace.lock().unwrap().push(json!({ "op": op, "args": args }));
    }
}

macro_rules! counted {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty => $args:expr;)*) => {
        $(fn $name(&self, $($arg: $ty),*) -> $ret {
            self.note(stringify!($name), $args);
            self.inner.$name($($arg),*)
        })*
    };
}

impl<C: Catalog> Catalog for Counted<C> {
    fn update_run(&self, run_id: &str, f: &mut dyn FnMut(&mut RunRow) -> Result<(), RunError>) -> Result<Option<Result<RunRow, RunError>>, Failure> {
        let out = self.inner.update_run(run_id, f);
        // The row the closure left, which a replay writes back in place of the closure.
        let after = match &out {
            Ok(Some(Ok(row))) => serde_json::to_value(row).unwrap(),
            _ => Value::Null,
        };
        self.note("update_run", json!({ "run_id": run_id, "after": after }));
        out
    }

    fn execution_guard(&self, admission: &ExecutionAdmission) -> Result<Box<dyn ExecutionGuard>, Failure> {
        let owners: Vec<Value> = admission.owners().iter().map(|o| json!({ "scope": o.scope, "execution_id": o.execution_id, "pins": o.pins, "run_id": o.run_id })).collect();
        self.note("execution_guard", json!({ "owners": owners, "leases": admission.leases(), "publication": admission.publication }));
        self.inner.execution_guard(admission)
    }

    counted! {
        supports_strict_execution_guards() -> bool => json!({});
        now() -> Result<Instant, Failure> => json!({});
        acquire(key: &LeaseKey, holder: &str, ttl_secs: u64) -> Result<Option<Lease>, Failure> => json!({ "key": lease_key_json(key), "holder": holder, "ttl_secs": ttl_secs });
        release(lease: &Lease) -> Result<(), Failure> => json!({ "lease": lease });
        renew(lease: &Lease, ttl_secs: u64) -> Result<Option<Lease>, Failure> => json!({ "lease": lease, "ttl_secs": ttl_secs });
        lease_holds(lease: &Lease) -> Result<bool, Failure> => json!({ "lease": lease });
        lease_row(key: &LeaseKey) -> Result<LeaseRow, Failure> => json!({ "key": lease_key_json(key) });
        cursor_at(scope: &OwnerScope) -> Result<CursorRow, Failure> => json!({ "scope": scope });
        cursor_cas_at(scope: &OwnerScope, expected_version: u64, next: CursorRow, fence: Option<&Lease>) -> Result<Cas, Failure> => json!({ "scope": scope, "expected_version": expected_version, "next": next, "fence": fence });
        owner_at(scope: &OwnerScope) -> Result<Option<ExecutionOwner>, Failure> => json!({ "scope": scope });
        put_owner(owner: &ExecutionOwner) -> Result<(), Failure> => json!({ "owner": owner });
        retire_at(scope: &OwnerScope, execution_id: &str, cursor: Option<(CursorRow, u64)>, fence: Option<&Lease>) -> Result<Cas, Failure> => json!({ "scope": scope, "execution_id": execution_id, "cursor": cursor, "fence": fence });
        retired_at(scope: &OwnerScope) -> Result<Option<String>, Failure> => json!({ "scope": scope });
        chunk_at(scope: &OwnerScope) -> Result<Option<ChunkRow>, Failure> => json!({ "scope": scope });
        chunks(pipeline_id: &str, table: &str) -> Result<Vec<ChunkRow>, Failure> => json!({ "pipeline_id": pipeline_id, "table": table });
        put_chunk(row: &ChunkRow) -> Result<(), Failure> => json!({ "row": row });
        delete_chunks(pipeline_id: &str, table: &str) -> Result<Vec<ChunkRow>, Failure> => json!({ "pipeline_id": pipeline_id, "table": table });
        update_chunk(scope: &OwnerScope, retire: Option<&str>, f: &mut dyn FnMut(&mut ChunkRow)) -> Result<Option<ChunkRow>, Failure> => json!({ "scope": scope, "retire": retire });
        cursor(pipeline_id: &str, table: &str) -> Result<CursorRow, Failure> => json!({ "pipeline_id": pipeline_id, "table": table });
        cursor_cas(pipeline_id: &str, table: &str, expected_version: u64, next: CursorRow, fence: Option<&Lease>) -> Result<Cas, Failure> => json!({ "pipeline_id": pipeline_id, "table": table, "expected_version": expected_version, "next": next, "fence": fence });
        owner(pipeline_id: &str, table: &str) -> Result<Option<ExecutionOwner>, Failure> => json!({ "pipeline_id": pipeline_id, "table": table });
        retire(pipeline_id: &str, table: &str, execution_id: &str, cursor: Option<(CursorRow, u64)>, fence: Option<&Lease>) -> Result<Cas, Failure> => json!({ "pipeline_id": pipeline_id, "table": table, "execution_id": execution_id, "cursor": cursor, "fence": fence });
        put_run(row: &RunRow) -> Result<(), Failure> => json!({ "row": row });
        run(run_id: &str) -> Result<Option<RunRow>, Failure> => json!({ "run_id": run_id });
        runs(pipeline_id: Option<&str>) -> Result<Vec<RunRow>, Failure> => json!({ "pipeline_id": pipeline_id });
        history(pipeline_id: Option<&str>, window: &Window) -> Result<HistoryPage, Failure> => json!({ "pipeline_id": pipeline_id, "window": window });
        last_run_start(pipeline_id: &str) -> Result<Option<Instant>, Failure> => json!({ "pipeline_id": pipeline_id });
    }
}
