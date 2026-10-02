//! The runner across `run.own`, `run.journal`, `run.advance` and `run.record`: crash,
//! resume, retirement, pins and the position.

use crate::support::{ids, plan, three_pages, Pages, Rig, Sink};
use contextful_core::coordinate::Catalog;
use contextful_core::run::ports::{Cancellation, Landed, PullRequest, Source};
use contextful_core::run::record::RunStatus;
use contextful_core::run::{Failure, FailureTag};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn opaque() -> contextful_core::run::plan::Plan {
    plan("kind = \"opaque-token\"", "")
}

/// A journal hit hands the recorded batch to the land path, so a resumed run lands bytes identical to an
/// uninterrupted one, batch ordinal included.
// spec: run.journal.replay-lands@0cc34691
#[test]
fn a_resumed_run_lands_what_an_uninterrupted_one_lands() {
    let straight = Rig::new();
    let mut sink_a = Sink::default();
    let row = straight.run(&opaque(), "1.0.0", "run-1", &mut Pages::new(three_pages()), &mut sink_a).unwrap();
    assert_eq!(row.status, RunStatus::Success);

    let crashed = Rig::new();
    let mut source = Pages::new(three_pages());
    source.die_after = Some(1);
    let mut sink_b = Sink::default();
    crashed.crash(&opaque(), "run-1", &mut source, &mut sink_b);
    crashed.clock.advance(60);
    crashed.run(&opaque(), "1.0.0", "run-2", &mut source, &mut sink_b).unwrap();

    let (a, b) = (&sink_a.commits[0], &sink_b.commits[0]);
    assert_eq!(serde_json::to_vec(&a.batches).unwrap(), serde_json::to_vec(&b.batches).unwrap());
    assert_eq!(ids(b), [vec!["d1", "d2"], vec!["d3"], vec!["d4"]], "one batch per pull, in pull order");
    assert_eq!(a.cursor, b.cursor);
}

/// Recorded work keys on the execution id; a catalog run id is the provenance of one attempt, so attempts under
/// one owner resolve the same recorded values.
// spec: run.own.execution-id-keys-the-journal@7fcb80d4
#[test]
fn a_second_attempt_under_one_owner_replays_the_firsts_recorded_pulls() {
    let rig = Rig::new();
    let mut source = Pages::new(three_pages());
    source.die_after = Some(2);
    let mut sink = Sink::default();
    rig.crash(&opaque(), "run-1", &mut source, &mut sink);
    let owner = rig.catalog().owner("feed", "filings").unwrap().unwrap();
    assert_eq!(owner.attempts, ["run-1"]);
    rig.clock.advance(60);
    let row = rig.run(&opaque(), "1.0.0", "run-2", &mut source, &mut sink).unwrap();
    assert_eq!(row.status, RunStatus::Success);
    assert_eq!(row.execution_id, rig.row("run-1").execution_id, "both attempts run under one execution");
    // Pages 0 and 1 were recorded under run-1 and resolve for run-2 without a call.
    let calls: Vec<Option<Value>> = source.calls().into_iter().map(|(p, _)| p).collect();
    assert_eq!(calls, [None, Some(json!("p1")), Some(json!("p2")), Some(json!("p2"))]);
}

/// A run that finished retires its owner, caches the position and collects its journal; the next fire runs under a
/// fresh execution even at an identical position.
#[test]
fn success_retires_the_owner_and_the_next_fire_takes_a_fresh_execution() {
    let rig = Rig::new();
    let mut sink = Sink::default();
    let first = rig.run(&opaque(), "1.0.0", "run-1", &mut Pages::new(three_pages()), &mut sink).unwrap();
    assert!(rig.catalog().owner("feed", "filings").unwrap().is_none());
    let cursor = rig.catalog().cursor("feed", "filings").unwrap();
    assert_eq!(cursor.position, Some(json!("p3")));
    assert_eq!(cursor.marker_run_id.as_deref(), Some("run-1"));
    let second = rig.run(&opaque(), "1.0.0", "run-2", &mut Pages::new(three_pages()), &mut sink).unwrap();
    assert_ne!(first.execution_id, second.execution_id);
}

/// Retiring an execution owner deletes its journal rows in the retiring transaction.
// spec: run.journal.collection@3c4f3a13
#[test]
fn retirement_collects_the_executions_journal() {
    let rig = Rig::new();
    let mut source = Pages::new(three_pages());
    source.die_after = Some(2);
    let mut sink = Sink::default();
    rig.crash(&opaque(), "run-1", &mut source, &mut sink);
    let execution = rig.row("run-1").execution_id;
    let dir = rig.dir.path().join("journal").join(&execution);
    assert_eq!(rig.engine.journal.recorded(&execution).unwrap(), 2, "the crash left two recorded pulls");
    rig.clock.advance(60);
    rig.run(&opaque(), "1.0.0", "run-2", &mut source, &mut sink).unwrap();
    assert!(!dir.exists(), "the retired execution's rows are gone");
    assert_eq!(rig.engine.journal.recorded(&execution).unwrap(), 0);
}

/// At run open, a commit marker newer than the catalog's cached position retires the pending owner that produced
/// it before any replay.
// spec: run.own.marker-reconciles@a8a124be
#[test]
fn a_marker_the_catalog_missed_retires_its_owner_before_replay() {
    let rig = Rig::new();
    let mut sink = Sink { die_after_land: true, ..Sink::default() };
    let mut source = Pages::new(three_pages());
    // The process dies between the commit marker and the catalog write.
    rig.crash(&opaque(), "run-1", &mut source, &mut sink);
    assert_eq!(sink.commits.len(), 1);
    assert!(rig.catalog().owner("feed", "filings").unwrap().is_some(), "the owner outlived its commit");
    rig.clock.advance(60);
    let before = source.calls().len();
    let row = rig.run(&opaque(), "1.0.0", "run-2", &mut source, &mut sink).unwrap();
    assert_eq!(row.status, RunStatus::Success);
    // No recorded pull replayed into rows already committed: the resumed run starts at the marker's position.
    assert_eq!(source.calls()[before..].iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(), [Some(json!("p3"))]);
    assert_eq!(sink.commits.len(), 1, "nothing re-landed");
    assert_eq!(row.rows, 0);
}

/// While an owner is pending, a changed pin — connector identity, component world, pipeline `content_hash`, or a
/// host's plan reference or identities — raises `ExecutionPinMismatch` before replay, terminal and non-retryable,
/// naming the scope and both pin sets.
// spec: run.own.pinned-plan-changed@8e47a280
#[test]
fn a_moved_build_under_a_pending_owner_is_refused_before_replay() {
    let rig = Rig::new();
    let mut source = Pages::new(three_pages());
    source.die_after = Some(1);
    let mut sink = Sink::default();
    rig.crash(&opaque(), "run-1", &mut source, &mut sink);
    rig.clock.advance(60);
    let calls = source.calls().len();
    let changed_plan = plan("kind = \"opaque-token\"", "journal = true");
    for (i, (p, version)) in [(opaque(), "2.0.0"), (changed_plan, "1.0.0")].into_iter().enumerate() {
        let row = rig.run(&p, version, &format!("run-x{i}"), &mut source, &mut sink).unwrap();
        assert_eq!(row.status, RunStatus::Failed);
        let m = row.error_message.unwrap();
        assert!(m.starts_with("ExecutionPinMismatch"), "{m}");
        assert!(m.contains("`feed`") && m.contains("vendor@1.0.0"), "{m}");
        assert_eq!(source.calls().len(), calls, "no pull replayed or issued");
    }
    assert!(rig.catalog().owner("feed", "filings").unwrap().is_some(), "the pending owner holds");
}

/// Restoring the recorded build and resuming to completion clears a pending owner.
#[test]
fn restoring_the_recorded_build_resumes_and_clears_the_owner() {
    let rig = Rig::new();
    let mut source = Pages::new(three_pages());
    source.die_after = Some(1);
    let mut sink = Sink::default();
    rig.crash(&opaque(), "run-1", &mut source, &mut sink);
    rig.clock.advance(60);
    assert_eq!(rig.run(&opaque(), "2.0.0", "run-2", &mut source, &mut sink).unwrap().status, RunStatus::Failed);
    assert_eq!(rig.run(&opaque(), "1.0.0", "run-3", &mut source, &mut sink).unwrap().status, RunStatus::Success);
    assert!(rig.catalog().owner("feed", "filings").unwrap().is_none());
}

/// One run produces one atomic commit per table; a crash mid-run leaves parts under that run's own directory, and
/// a resumption continues from the last recorded step.
// spec: run.own.one-commit-per-run@4d8fdd9d
#[test]
fn a_run_commits_once_and_a_crash_commits_nothing() {
    let rig = Rig::new();
    let mut source = Pages::new(three_pages());
    source.die_after = Some(2);
    let mut sink = Sink::default();
    rig.crash(&opaque(), "run-1", &mut source, &mut sink);
    assert!(sink.commits.is_empty(), "the crashed run committed nothing");
    rig.clock.advance(60);
    rig.run(&opaque(), "1.0.0", "run-2", &mut source, &mut sink).unwrap();
    assert_eq!(sink.commits.len(), 1);
    assert_eq!(sink.commits[0].run_id, "run-2");
    assert_eq!(sink.commits[0].batches.len(), 3);
    // The resumption re-entered only the step the crash cut short.
    assert_eq!(source.calls().iter().filter(|(p, _)| p == &Some(json!("p2"))).count(), 2);
    assert_eq!(source.calls().iter().filter(|(p, _)| p.is_none()).count(), 1);
}

/// A table's new position rides the run commit marker, {{store.lay-out.run-manifest}}, that publishes the rows
/// behind it; the catalog cursor is a cache of the newest committed marker.
// spec: run.advance.commit-with-rows@52d475b5
#[test]
fn the_position_commits_with_the_rows_and_the_catalog_caches_it() {
    let rig = Rig::new();
    let mut sink = Sink::default();
    rig.run(&opaque(), "1.0.0", "run-1", &mut Pages::new(three_pages()), &mut sink).unwrap();
    assert_eq!(sink.commits[0].cursor, Some(json!("p3")), "the commit carries the position its rows reach");
    // A lost catalog rebuilds its cursor from the newest marker.
    let fresh = Rig::new();
    let mut source = Pages::new(three_pages());
    fresh.run(&opaque(), "1.0.0", "run-2", &mut source, &mut sink).unwrap();
    assert_eq!(source.calls()[0].0, Some(json!("p3")), "the next run reads from the marker's position");
    assert_eq!(fresh.catalog().cursor("feed", "filings").unwrap().marker_run_id.as_deref(), Some("run-1"));
}

/// A source over a clock field: it serves every stored row whose `updated_at` is at or after the position.
struct Polled {
    rows: Vec<Value>,
    calls: usize,
}

impl Source for Polled {
    fn pull(&mut self, request: &PullRequest, _: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        self.calls += 1;
        let at = request.position.as_ref().and_then(|p| p.get("at")).and_then(Value::as_i64).unwrap_or(i64::MIN);
        let rows: Vec<&Value> = self.rows.iter().filter(|r| r["updated_at"].as_i64().unwrap() >= at).collect();
        Ok(serde_json::to_vec(&json!({ "rows": rows, "more": false })).unwrap())
    }
}

/// A polled load admits a row whose clock value is at or after the stored position, re-landing the boundary
/// instant's rows on every poll.
// spec: run.advance.inclusive-boundary@5a585ac1
#[test]
fn every_poll_re_lands_the_boundary_instant() {
    let rig = Rig::new();
    let p = plan("kind = \"monotonic\"\nfield = \"updated_at\"", "");
    let mut source = Polled { rows: vec![json!({"id": "a", "updated_at": 5}), json!({"id": "b", "updated_at": 9})], calls: 0 };
    let mut sink = Sink::default();
    rig.run(&p, "1.0.0", "run-1", &mut source, &mut sink).unwrap();
    assert_eq!(rig.catalog().cursor("feed", "filings").unwrap().position, Some(json!({"field": "updated_at", "at": 9})));
    // A sibling sharing the boundary value arrives after the first poll.
    source.rows.push(json!({"id": "c", "updated_at": 9}));
    rig.run(&p, "1.0.0", "run-2", &mut source, &mut sink).unwrap();
    rig.run(&p, "1.0.0", "run-3", &mut source, &mut sink).unwrap();
    assert_eq!(ids(&sink.commits[1]), [vec!["b", "c"]]);
    assert_eq!(ids(&sink.commits[2]), [vec!["b", "c"]], "the boundary rows re-land on every poll");
}

/// A source serving every row it holds on each pull, its clock nested inside each row.
struct Nested(Vec<Value>);

impl Source for Nested {
    fn pull(&mut self, _: &PullRequest, _: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        Ok(serde_json::to_vec(&json!({ "rows": self.0, "more": false })).unwrap())
    }
}

/// An `incremental` value opening with `/` is an RFC 6901 pointer read from each fetched row, so a nested clock
/// such as `/commit/committer/date` orders the stream and names its watermark.
// spec: run.declare.incremental-pointer@d55d2b92
#[test]
fn a_pointer_clock_reads_a_nested_value_from_each_row() {
    let rig = Rig::new();
    let p = plan("kind = \"monotonic\"\nfield = \"/commit/committer/date\"", "");
    let commit = |id: &str, date: &str| json!({"id": id, "commit": {"committer": {"date": date}}});
    let mut source = Nested(vec![commit("a", "2011-01-26T19:06:08Z"), commit("b", "2012-03-06T23:06:50Z")]);
    let mut sink = Sink::default();
    rig.run(&p, "1.0.0", "run-1", &mut source, &mut sink).unwrap();
    assert_eq!(
        rig.catalog().cursor("feed", "filings").unwrap().position,
        Some(json!({"field": "/commit/committer/date", "at": "2012-03-06T23:06:50Z"}))
    );
    assert_eq!(ids(&sink.commits[0]), [vec!["a", "b"]]);
    // The next poll admits the boundary row and a later one, and drops the older row.
    source.0.push(commit("c", "2012-04-01T00:00:00Z"));
    rig.run(&p, "1.0.0", "run-2", &mut source, &mut sink).unwrap();
    assert_eq!(ids(&sink.commits[1]), [vec!["b", "c"]]);
    // A row whose pointer names nothing has no orderable clock.
    source.0.push(json!({"id": "d", "commit": {}}));
    assert!(rig.run(&p, "1.0.0", "run-3", &mut source, &mut sink).map_or(true, |r| r.status != RunStatus::Success));
}

/// A `monotonic` position is concurrent-safe and commits the highest value observed. An `opaque-token` or
/// `snapshot-id` position moves only under a single-writer lease. Last-write-wins governs no cursor.
// spec: run.advance.concurrency-by-kind@b06ad45d
#[test]
fn a_token_cursor_moves_under_one_writer_and_a_watermark_never_rewinds() {
    use contextful_core::coordinate::LeaseKey;
    let rig = Rig::new();
    // Another writer holds the table's single-writer lease.
    let held = rig.catalog().acquire(&LeaseKey::Pipeline("feed/filings".into()), "run-elsewhere", 30).unwrap().unwrap();
    let mut source = Pages::new(three_pages());
    let row = rig.run(&opaque(), "1.0.0", "run-1", &mut source, &mut Sink::default()).unwrap();
    assert_eq!(row.status, RunStatus::Failed);
    assert_eq!(row.error_kind, Some(FailureTag::Transient));
    assert!(source.calls().is_empty(), "no pull moves a token cursor without the lease");
    rig.catalog().release(&held).unwrap();
    assert_eq!(rig.run(&opaque(), "1.0.0", "run-2", &mut source, &mut Sink::default()).unwrap().status, RunStatus::Success);

    // A monotonic run takes no lease, and an older window commits the higher stored value.
    let mono = Rig::new();
    mono.catalog().acquire(&LeaseKey::Pipeline("feed/filings".into()), "run-elsewhere", 30).unwrap().unwrap();
    let p = plan("kind = \"monotonic\"\nfield = \"updated_at\"", "");
    let mut newer = Polled { rows: vec![json!({"id": "a", "updated_at": 9})], calls: 0 };
    assert_eq!(mono.run(&p, "1.0.0", "run-1", &mut newer, &mut Sink::default()).unwrap().status, RunStatus::Success);
    let mut older = Polled { rows: vec![json!({"id": "z", "updated_at": 3})], calls: 0 };
    mono.run(&p, "1.0.0", "run-2", &mut older, &mut Sink::default()).unwrap();
    assert_eq!(mono.catalog().cursor("feed", "filings").unwrap().position, Some(json!({"field": "updated_at", "at": 9})));
}

/// A source recording whether the run row existed when it was first called.
struct Probe {
    catalog: Arc<dyn Catalog + Send + Sync>,
    saw: Arc<Mutex<Option<RunStatus>>>,
    fail: Option<Failure>,
}

impl Source for Probe {
    fn pull(&mut self, _: &PullRequest, _: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        *self.saw.lock().unwrap() = self.catalog.run("run-1").unwrap().map(|r| r.status);
        match self.fail.take() {
            Some(f) => Err(f),
            None => Ok(b"{\"rows\": [], \"more\": false}".to_vec()),
        }
    }
}

/// A row is written at run open, before the first pull, so a zero-row `success`, a `failed` row carrying its
/// error kind, and no row at all read apart.
// spec: run.record.row-at-open@d1330544
#[test]
fn the_row_exists_before_the_first_pull() {
    for fail in [None, Some(Failure::new(FailureTag::AuthExpired, "token expired"))] {
        let rig = Rig::new();
        let saw = Arc::new(Mutex::new(None));
        let mut probe = Probe { catalog: rig.engine.catalog.clone(), saw: saw.clone(), fail: fail.clone() };
        assert!(rig.catalog().run("run-1").unwrap().is_none(), "no row before the run");
        let row = rig.run(&opaque(), "1.0.0", "run-1", &mut probe, &mut Sink::default()).unwrap();
        assert_eq!(*saw.lock().unwrap(), Some(RunStatus::Running));
        match fail {
            None => assert_eq!((row.status, row.rows, row.error_kind), (RunStatus::Success, 0, None)),
            Some(_) => assert_eq!((row.status, row.error_kind), (RunStatus::Failed, Some(FailureTag::AuthExpired))),
        }
    }
}

/// Row and byte counts are measured at the destination, not at the source.
// spec: run.record.counts-at-destination@b1a2ff7c
#[test]
fn counts_come_from_the_destination() {
    let rig = Rig::new();
    let mut sink = Sink { report: Some(Landed { rows: 3, bytes: 4096 }), ..Sink::default() };
    let row = rig.run(&opaque(), "1.0.0", "run-1", &mut Pages::new(three_pages()), &mut sink).unwrap();
    assert_eq!(sink.commits[0].batches.iter().map(Vec::len).sum::<usize>(), 4, "the source handed over four rows");
    assert_eq!((row.rows, row.bytes), (3, 4096));
}

#[test]
fn a_component_connector_is_refused_on_a_build_without_a_component_host() {
    let rig = Rig::new();
    let text = "pipeline = \"feed\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\nworld = \"contextful:source/pull@1\"\ncommand = [\"vendor\"]\n";
    let p = contextful_core::run::plan::Plan::compile(text.as_bytes()).unwrap();
    let mut source = Pages::new(three_pages());
    let err = rig.run(&p, "1", "run-1", &mut source, &mut Sink::default()).unwrap_err().to_string();
    assert!(err.starts_with("ComponentHostMissing") && err.contains("vendor"), "{err}");
    assert!(source.calls().is_empty());
    assert!(rig.catalog().run("run-1").unwrap().is_none());
}

/// A component connector runs where a component host is linked: the full profile and the container or worker shapes
/// built from it.
// spec: topology.package.component-host@ea74ba9e
#[test]
fn a_component_connector_runs_on_an_engine_wiring_its_world() {
    let mut rig = Rig::new();
    rig.engine.worlds = vec!["contextful:source/pull@1".to_string()];
    let text = "pipeline = \"feed\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\nworld = \"contextful:source/pull@1\"\ncommand = [\"vendor\"]\n";
    let p = contextful_core::run::plan::Plan::compile(text.as_bytes()).unwrap();
    let row = rig.run(&p, "1", "run-1", &mut Pages::new(three_pages()), &mut Sink::default()).unwrap();
    assert_eq!(row.status, RunStatus::Success);
    assert!(rig.engine.capabilities().hosts("native"), "a wired world adds to the native one");
    let other = p.spec.connector.world.replace("@1", "@2");
    assert!(!rig.engine.capabilities().hosts(&other), "a world the engine does not wire stays refused");
}

/// A vendor whose page `n` declares `skipped[n]` inputs it declined to land, and dies after serving the page
/// named in `die_after`, before the runner records it.
struct Skipping {
    skipped: Vec<u64>,
    die_after: Option<usize>,
    calls: usize,
}

impl Source for Skipping {
    fn pull(&mut self, request: &PullRequest, _cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        self.calls += 1;
        let n = request.position.as_ref().and_then(Value::as_u64).unwrap_or(0) as usize;
        let body = json!({ "rows": [{"id": format!("d{n}")}], "cursor": n + 1, "more": n + 1 < self.skipped.len(), "skipped": self.skipped[n] });
        if self.die_after == Some(n) {
            self.die_after = None;
            panic!("the process dies after the vendor served page {n}");
        }
        Ok(serde_json::to_vec(&body).unwrap())
    }
}

/// A pull's optional `skipped` field counts inputs the source declined to land whole; the run row sums it over the
/// run's pulls, replayed pulls included, beside the destination counts.
// spec: run.record.skipped-count@849043a6
#[test]
fn the_run_row_sums_the_skipped_count_of_every_pull() {
    let rig = Rig::new();
    let row = rig.run(&opaque(), "1.0.0", "run-1", &mut Skipping { skipped: vec![2, 0, 1], die_after: None, calls: 0 }, &mut Sink::default()).unwrap();
    assert_eq!((row.status, row.rows, row.skipped), (RunStatus::Success, 3, 3));
    assert_eq!(rig.row("run-1").skipped, 3, "the catalog holds the count");

    // A pull recorded before a crash counts when the next attempt replays it.
    let crashed = Rig::new();
    let mut source = Skipping { skipped: vec![2, 0, 1], die_after: Some(1), calls: 0 };
    let mut sink = Sink::default();
    crashed.crash(&opaque(), "run-1", &mut source, &mut sink);
    crashed.clock.advance(60);
    let row = crashed.run(&opaque(), "1.0.0", "run-2", &mut source, &mut sink).unwrap();
    assert_eq!((row.status, row.skipped), (RunStatus::Success, 3));
    assert_eq!(source.calls, 4, "page 0 replays from the journal; page 1 is served again");

    // A source declaring no count leaves the row at zero.
    let plain = Rig::new();
    let row = plain.run(&opaque(), "1.0.0", "run-1", &mut Pages::new(three_pages()), &mut Sink::default()).unwrap();
    assert_eq!(row.skipped, 0);
}
