//! `run.journal.ledger-settles-first` and `run.cancel.abandoned-work`: the request ledger
//! a source keeps settles before the journal records its batch, and on the failure path a
//! stop takes.

use crate::support::{plan, three_pages, Pages, Rig, Sink};
use contextful_core::coordinate::CursorRow;
use contextful_core::run::cancel::Scope;
use contextful_core::run::ports::{Cancellation, PullRequest, Source};
use contextful_core::run::record::RunStatus;
use contextful_core::run::{Failure, FailureTag};
use contextful_engine::{Engine, Journal};
use serde_json::json;
use std::time::Duration;

/// Journal rows recorded under every execution the journal holds.
fn recorded(journal: &Journal) -> usize {
    use contextful_core::run::ports::JournalStore;
    journal.row_store().executions().unwrap().iter().map(|e| journal.recorded(e).unwrap()).sum()
}

/// A paged vendor keeping a request ledger: each pull leaves one request unsettled, and a
/// settle writes the open requests to the ledger, noting how many journal rows stood then.
struct Ledgered {
    pages: Pages,
    journal: Journal,
    open: Vec<String>,
    /// Each settled request beside the recorded journal rows standing when it settled.
    ledger: Vec<(String, usize)>,
    /// The settle that refuses, by its one-based count.
    refuse_settle: Option<usize>,
    settles: usize,
}

impl Ledgered {
    fn new(journal: Journal) -> Ledgered {
        Ledgered { pages: Pages::new(three_pages()), journal, open: Vec::new(), ledger: Vec::new(), refuse_settle: None, settles: 0 }
    }
}

impl Source for Ledgered {
    fn pull(&mut self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        self.open.push(format!("GET {}", request.position.clone().unwrap_or(json!("start"))));
        self.pages.pull(request, cancel)
    }

    fn settle(&mut self) -> Result<(), Failure> {
        self.settles += 1;
        if self.refuse_settle == Some(self.settles) {
            return Err(Failure::new(FailureTag::Storage, "the request ledger refused its append"));
        }
        let standing = recorded(&self.journal);
        self.ledger.extend(self.open.drain(..).map(|call| (call, standing)));
        Ok(())
    }
}

/// The outbound request ledger settles durably before the entry recording its batch commits.
// spec: run.journal.ledger-settles-first@401ec2c4
#[test]
fn each_pull_settles_its_requests_before_the_journal_records_its_batch() {
    let rig = Rig::new();
    let p = plan("kind = \"opaque-token\"", "");
    let mut source = Ledgered::new(rig.engine.journal.clone());
    let row = rig.run(&p, "1.0.0", "run-1", &mut source, &mut Sink::default()).unwrap();
    assert_eq!(row.status, RunStatus::Success);
    assert_eq!(source.ledger.len(), 3, "every pull's request settled");
    // Pull n settles while only the n batches before it are recorded: its own entry follows.
    let standing: Vec<usize> = source.ledger.iter().map(|(_, standing)| *standing).collect();
    assert_eq!(standing, [0, 1, 2]);

    // A settle that fails records nothing for its batch, and the run fails instead of
    // recording a batch whose requests the ledger lacks.
    let rig = Rig::new();
    let mut source = Ledgered::new(rig.engine.journal.clone());
    source.refuse_settle = Some(2);
    let row = rig.run(&p, "1.0.0", "run-1", &mut source, &mut Sink::default()).unwrap();
    assert_eq!(row.status, RunStatus::Failed);
    assert_eq!(recorded(&rig.engine.journal), 1, "only the first, settled batch is recorded");
}

/// A ledgered source that marks its own run stopped from inside the pull, then meets the
/// stop in its retry sleep.
struct StopsMidPull {
    inner: Ledgered,
    engine: Engine,
    calls: usize,
}

impl Source for StopsMidPull {
    fn pull(&mut self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        self.calls += 1;
        if self.calls == 1 {
            return self.inner.pull(request, cancel);
        }
        self.inner.open.push("GET stopped".into());
        self.engine.cancel("run-1", Scope::Run, Some("operator asked".into())).unwrap();
        Err(Failure::new(FailureTag::Transient, "reset"))
    }

    fn settle(&mut self) -> Result<(), Failure> {
        self.inner.settle()
    }
}

/// Abandoned work surfaces as the `Canceled` tag through the ordinary failure path, which settles the request
/// ledger, closes the record and leaves the position alone.
// spec: run.cancel.abandoned-work@7727fd69
#[test]
fn a_stop_settles_the_ledger_closes_canceled_and_holds_the_position() {
    let rig = Rig::new();
    rig.catalog().cursor_cas("feed", "filings", 0, CursorRow { position: Some(json!("p0")), ..CursorRow::default() }, None).unwrap();
    let before = rig.catalog().cursor("feed", "filings").unwrap();
    let p = plan("kind = \"opaque-token\"", "[retry]\nbackoff = \"fixed\"\nbase_ms = 60000\njitter_ms = 0");
    let mut source = StopsMidPull { inner: Ledgered::new(rig.engine.journal.clone()), engine: rig.engine.clone(), calls: 0 };
    // The stopped pull's own settle fails, so only the failure path can settle its request.
    source.inner.refuse_settle = Some(2);
    let started = std::time::Instant::now();
    let row = rig.run(&p, "1.0.0", "run-1", &mut source, &mut Sink::default()).unwrap();
    assert!(started.elapsed() < Duration::from_secs(10), "the stop cut the retry sleep short");
    assert_eq!((row.status, row.error_kind), (RunStatus::Canceled, Some(FailureTag::Canceled)));
    assert!(row.ended_at.is_some(), "the record closes");
    let settled: Vec<&str> = source.inner.ledger.iter().map(|(call, _)| call.as_str()).collect();
    assert_eq!(settled, ["GET \"p0\"", "GET stopped"], "the failure path settled the stopped pull's request");
    assert!(source.inner.open.is_empty(), "no request stays unsettled");
    assert_eq!(rig.catalog().cursor("feed", "filings").unwrap().position, before.position, "a stop moves no position");
}
