//! A scratch engine over a temporary directory, a settable clock, a scripted source and
//! an in-memory destination.

use contextful_core::coordinate::Catalog;
use contextful_core::ports::Clock;
use contextful_core::run::own::ConnectorPin;
use contextful_core::run::plan::Plan;
use contextful_core::run::ports::{Cancellation, Commit, Destination, Landed, Marker, Part, PullRequest, Row, Source, Stage};
use contextful_core::run::record::RunRow;
use contextful_core::run::Failure;
use contextful_core::time::Instant;
use contextful_engine::cancel::{Cadence, Keeper};
use contextful_engine::{Engine, EngineError, Journal, LocalCatalog, RunSpec};
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
        let journal = Journal::open(dir.path());
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
}

impl Destination for Sink {
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
