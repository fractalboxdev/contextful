//! The runner across `run.own`, `run.journal`, `run.advance` and `run.record`: crash,
//! resume, retirement, pins and the position.

use crate::support::{ids, plan, three_pages, Pages, Rig, Sink};
use contextful_core::coordinate::Catalog;
use contextful_core::run::ports::{Cancellation, Landed, PullRequest, Source, Commit, Destination, Marker, Part, Row, Stage, Types};
use contextful_core::run::record::RunStatus;
use contextful_core::run::{Failure, FailureTag};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn opaque() -> contextful_core::run::plan::Plan {
    plan("kind = \"opaque-token\"", "")
}

struct Protected { sink: Sink, authority: String }
impl Destination for Protected {
    fn recording_identity(&self, _: &contextful_core::run::plan::Plan) -> Result<Option<String>, Failure> { Ok(Some(self.authority.clone())) }
    fn prepare_recorded(&mut self, _: &str, mut rows: Vec<Row>, _: Types, _: &str) -> Result<Value, Failure> {
        for row in &mut rows { row.insert("body".into(), json!("prepared-once")); }
        Ok(json!(rows))
    }
    fn stage_recorded(&mut self, mut stage: Stage, payload: &Value) -> Result<Part, Failure> {
        stage.rows = serde_json::from_value(payload.clone()).unwrap();
        self.sink.stage_batch(stage)
    }
    fn stage_batch(&mut self, _: Stage) -> Result<Part, Failure> { panic!("protected recording cannot enter raw staging") }
    fn commit(&mut self, commit: Commit, before: &dyn Fn() -> Result<(), Failure>) -> Result<Landed, Failure> { self.sink.commit(commit, before) }
    fn discard(&mut self, table: &str, run: &str) -> Result<(), Failure> { self.sink.discard(table, run) }
    fn newest_marker(&self, pipeline: &str, table: &str) -> Result<Option<Marker>, Failure> { self.sink.newest_marker(pipeline, table) }
}

#[test]
fn prepared_pulls_bind_authority_before_open_and_replay_only_rewritten_bytes() {
    fn bytes(path: &std::path::Path) -> Vec<u8> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            out.extend(if path.is_dir() { bytes(&path) } else { std::fs::read(path).unwrap() });
        }
        out
    }
    let rig = Rig::new();
    let mut source = Pages::new(vec![vec![json!({"id":"one","body":"private-recording-canary"})], Vec::new()]);
    source.die_after = Some(1);
    let mut dest = Protected { sink:Sink::default(), authority:"canonical-a".into() };
    rig.crash(&opaque(), "run-a", &mut source, &mut dest);
    let execution = rig.row("run-a").execution_id;
    assert_eq!(rig.engine.journal.recorded(&execution).unwrap(), 1);
    let held = bytes(&rig.dir.path().join("journal").join(&execution));
    assert!(!held.is_empty(), "the exclusion below ranges over no element");
    assert!(!held.windows(b"private-recording-canary".len()).any(|part| part == b"private-recording-canary"));
    assert!(String::from_utf8_lossy(&held).contains("prepared-once"));
    rig.clock.advance(60);
    let calls = source.calls().len();
    let staged = dest.sink.staged.len();
    dest.authority = "canonical-b".into();
    let refused = rig.run(&opaque(), "1.0.0", "run-b", &mut source, &mut dest).unwrap();
    assert_eq!(refused.status, RunStatus::Failed);
    let message = refused.error_message.unwrap();
    assert!(message.contains("ExecutionPinMismatch"), "{message}");
    assert_eq!(source.calls().len(), calls);
    assert_eq!(dest.sink.staged.len(), staged);
    dest.authority = "canonical-a".into();
    source.die_after = None;
    assert_eq!(rig.run(&opaque(), "1.0.0", "run-c", &mut source, &mut dest).unwrap().status, RunStatus::Success);
    assert_eq!(source.calls().iter().filter(|(position, _)| position.is_none()).count(), 1);
    assert_eq!(dest.sink.commits[0].batches[0][0]["body"], json!("prepared-once"));
}

#[test]
fn prepared_recording_refuses_late_and_changed_types_before_preparation_or_recording() {
    use contextful_core::run::ports::{Commit, Destination, Marker, Part, Row, Stage, Types};
    struct TypedPages(bool);
    impl Source for TypedPages {
        fn pull(&mut self, request: &PullRequest, _: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
            let first = request.position.is_none();
            let types = if first { if self.0 { json!({"public":"Utf8"}) } else { json!({}) } } else { json!({"public":"Int64"}) };
            Ok(serde_json::to_vec(&json!({"rows":[{"public":if first { json!("one") } else { json!(2) }}],"cursor":if first { "p1" } else { "p2" },"more":first,"types":types})).unwrap())
        }
    }
    struct Counting { prepared: usize }
    impl Destination for Counting {
        fn recording_identity(&self, _: &contextful_core::run::plan::Plan) -> Result<Option<String>, Failure> { Ok(Some("authority".into())) }
        fn prepare_recorded(&mut self, _: &str, rows: Vec<Row>, _: Types, _: &str) -> Result<Value, Failure> { self.prepared += 1; Ok(json!(rows)) }
        fn stage_recorded(&mut self, _: Stage, _: &Value) -> Result<Part, Failure> { Ok(Part { name:"part".into(), rows:1, bytes:0 }) }
        fn stage_batch(&mut self, _: Stage) -> Result<Part, Failure> { unreachable!() }
        fn commit(&mut self, _: Commit, _: &dyn Fn() -> Result<(), Failure>) -> Result<Landed, Failure> { unreachable!() }
        fn discard(&mut self, _: &str, _: &str) -> Result<(), Failure> { Ok(()) }
        fn newest_marker(&self, _: &str, _: &str) -> Result<Option<Marker>, Failure> { Ok(None) }
    }
    for first_declared in [false, true] {
        let rig = Rig::new();
        let mut dest = Counting { prepared:0 };
        let row = rig.run(&opaque(), "1.0.0", "typed-a", &mut TypedPages(first_declared), &mut dest).unwrap();
        assert_eq!(row.status, RunStatus::Failed);
        let error = row.error_message.unwrap();
        assert!(error.contains(if first_declared { "StoreSchemaIncompatible" } else { "PipelineTypeDeclaredLate" }), "{error}");
        assert_eq!(dest.prepared, 1, "the incompatible second batch never reaches canonical preparation");
    }
}

#[test]
fn a_caller_plan_cannot_record_typed_removal_through_an_unadmitted_destination() {
    let rig = Rig::new();
    let declared = plan("kind = \"opaque-token\"", "redaction=[{table='filings',column='body',match='whole',operation='drop'}]");
    let mut source = Pages::new(three_pages());
    let error = rig.run(&declared, "1.0.0", "unbound-a", &mut source, &mut Sink::default()).unwrap_err();
    assert!(error.to_string().contains("JournalRedactionConflict"));
    assert!(source.calls().is_empty());
    assert!(rig.catalog().run("unbound-a").unwrap().is_none());
}

#[test]
fn prepared_replay_binds_shape_identity_and_never_reapplies_the_shape() {
    use contextful_core::run::ports::Shape;
    struct UndeclaredShape;
    impl Shape for UndeclaredShape {
        fn shape(&self, rows: Vec<Row>) -> Result<Vec<Row>, contextful_core::run::RunError> { Ok(rows) }
    }
    struct Shaped { version: &'static str, calls: std::cell::Cell<usize> }
    impl Shape for Shaped {
        fn recording_identity(&self) -> Result<Option<String>, contextful_core::run::RunError> { Ok(Some(self.version.into())) }
        fn shape(&self, mut rows: Vec<Row>) -> Result<Vec<Row>, contextful_core::run::RunError> {
            if !rows.is_empty() { self.calls.set(self.calls.get() + 1); }
            for row in &mut rows { row.insert("body".into(), json!(format!("shaped:{}", self.version))); }
            Ok(rows)
        }
    }
    let rig = Rig::new();
    let plan = opaque();
    let spec = |run: &str| contextful_engine::RunSpec { plan:plan.clone(), connector:plan.connector_pin("artifact-1"), run_id:run.into(), site_id:"site-a".into(), pid:4242, boot_id:"boot-a".into(), trace_id:None };
    let mut source = Pages::new(vec![vec![json!({"id":"one","body":"raw"})], Vec::new()]);
    source.die_after = Some(1);
    let mut dest = Protected { sink:Sink::default(), authority:"canonical".into() };
    let undeclared = rig.engine.run_with(&spec("shape-unbound"), &mut source, &UndeclaredShape, &mut dest).unwrap_err();
    assert!(undeclared.to_string().contains("declared shape identity"));
    assert!(source.calls().is_empty());
    assert!(rig.catalog().run("shape-unbound").unwrap().is_none());
    let mut shape = Shaped { version:"shape-a", calls:std::cell::Cell::new(0) };
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rig.engine.run_with(&spec("shape-a"), &mut source, &shape, &mut dest))).is_err());
    rig.clock.advance(60);
    let calls = source.calls().len();
    shape.version = "shape-b";
    let refused = rig.engine.run_with(&spec("shape-b"), &mut source, &shape, &mut dest).unwrap();
    assert!(refused.error_message.unwrap().contains("ExecutionPinMismatch"));
    assert_eq!(source.calls().len(), calls);
    assert_eq!(shape.calls.get(), 1);
    shape.version = "shape-a";
    source.die_after = None;
    assert_eq!(rig.engine.run_with(&spec("shape-c"), &mut source, &shape, &mut dest).unwrap().status, RunStatus::Success);
    assert_eq!(shape.calls.get(), 1, "recorded prepared rows bypass shape application during replay");
}

/// The run row holds the rows entering the transform chain and the rows it kept, a replayed prepared pull
/// counting what entered its chain when it was recorded.
#[test]
fn the_run_row_counts_rows_before_and_after_the_chain() {
    use contextful_core::run::ports::Shape;
    struct Dropping;
    impl Shape for Dropping {
        fn recording_identity(&self) -> Result<Option<String>, contextful_core::run::RunError> { Ok(Some("dropping-v1".into())) }
        fn shape(&self, rows: Vec<Row>) -> Result<Vec<Row>, contextful_core::run::RunError> {
            Ok(rows.into_iter().filter(|row| row["id"] != "d2").collect())
        }
    }
    let plan = opaque();
    let spec = |run: &str| contextful_engine::RunSpec { plan:plan.clone(), connector:plan.connector_pin("artifact-1"), run_id:run.into(), site_id:"site-a".into(), pid:4242, boot_id:"boot-a".into(), trace_id:None };

    let rig = Rig::new();
    let row = rig.engine.run_with(&spec("plain"), &mut Pages::new(three_pages()), &Dropping, &mut Sink::default()).unwrap();
    assert_eq!((row.status, row.fetched, row.kept), (RunStatus::Success, 4, 3));
    assert_eq!((rig.row("plain").fetched, rig.row("plain").kept), (4, 3), "the catalog holds the counts");

    let rig = Rig::new();
    let mut source = Pages::new(three_pages());
    source.die_after = Some(1);
    let mut dest = Protected { sink:Sink::default(), authority:"canonical".into() };
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rig.engine.run_with(&spec("prepared-a"), &mut source, &Dropping, &mut dest))).is_err());
    rig.clock.advance(60);
    source.die_after = None;
    let row = rig.engine.run_with(&spec("prepared-b"), &mut source, &Dropping, &mut dest).unwrap();
    assert_eq!((row.status, row.fetched, row.kept), (RunStatus::Success, 4, 3), "the replayed first page counts its two fetched rows");
}

#[test]
fn unknown_protected_clock_lineage_refuses_before_source_and_owner_admission() {
    struct UnknownClock;
    impl contextful_core::run::ports::Shape for UnknownClock {
        fn recording_identity(&self) -> Result<Option<String>, contextful_core::run::RunError> { Ok(Some("declared-custom-shape".into())) }
        fn shape(&self, rows: Vec<Row>) -> Result<Vec<Row>, contextful_core::run::RunError> { Ok(rows) }
    }
    for journal in [true, false] {
        let rig = Rig::new();
        let plan = plan("kind='monotonic'\nfield='at'", if journal { "journal=true" } else { "journal=false" });
        let spec = |run: &str| contextful_engine::RunSpec { plan:plan.clone(), connector:plan.connector_pin("artifact-1"), run_id:run.into(), site_id:"site-a".into(), pid:4242, boot_id:"boot-a".into(), trace_id:None };
        let mut source = Pages::new(vec![vec![json!({"at":"private-clock-canary","body":"raw"})]]);
        let mut destination = Protected { sink:Sink::default(), authority:"canonical".into() };
        let error = rig.engine.run_with(&spec("unknown-clock"), &mut source, &UnknownClock, &mut destination).unwrap_err();
        assert!(error.to_string().contains("safe typed progress lineage"));
        assert!(source.calls().is_empty());
        assert!(rig.catalog().run("unknown-clock").unwrap().is_none());
        let unprotected = rig.engine.run_with(&spec("ordinary-clock"), &mut source, &UnknownClock, &mut Sink::default()).unwrap();
        assert_eq!(unprotected.status, RunStatus::Success);
        assert!(!source.calls().is_empty());
    }
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

/// One run produces one atomic commit per table, plus one per checkpoint; a crash mid-run leaves parts under that
/// run's own directory, and a resumption continues from the last recorded step or commit.
// spec: run.own.one-commit-per-run@7976ecb6
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
    // The crashed run's two staged parts stay under its own name; the commit names run-2's three.
    assert_eq!(sink.staged.iter().filter(|s| s.run_id == "run-1").count(), 2);
    assert_eq!(sink.staged.iter().filter(|s| s.run_id == "run-2").map(|s| s.ordinal).collect::<Vec<_>>(), [0, 1, 2]);
}

/// A source that fails its first pull at `p1` with a non-retryable failure, then serves as `Pages` does.
struct FailsSecondPull {
    inner: Pages,
    failed: bool,
}

impl Source for FailsSecondPull {
    fn pull(&mut self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        if !self.failed && request.position == Some(json!("p1")) {
            self.failed = true;
            return Err(Failure::new(FailureTag::Permanent, "the vendor refused page 1"));
        }
        self.inner.pull(request, cancel)
    }
}

/// A run failing after it staged batches commits none of them under {{run.own.one-commit-per-run}} and closes
/// `failed`, holding its owner so the journal keeps the resume point; only {{run.record.orphan-reap}} closes a run
/// `partial_failure`.
// spec: run.retry.partial-failure@2920372d
#[test]
fn a_run_failing_after_a_stage_commits_nothing_and_closes_failed() {
    let rig = Rig::new();
    let mut source = FailsSecondPull { inner: Pages::new(three_pages()), failed: false };
    let mut sink = Sink::default();
    let row = rig.run(&opaque(), "1.0.0", "run-1", &mut source, &mut sink).unwrap();
    assert_eq!((row.status, row.error_kind), (RunStatus::Failed, Some(FailureTag::Permanent)));
    assert_eq!(sink.staged.len(), 1, "the first page staged before the failure");
    assert!(sink.commits.is_empty(), "the failed run commits nothing it staged");
    assert_eq!(rig.catalog().cursor("feed", "filings").unwrap().position, None, "the position stands");
    let held = rig.catalog().owner("feed", "filings").unwrap().expect("the failure holds the owner");
    assert_eq!(held.execution_id, row.execution_id);
    assert_eq!(rig.engine.journal.recorded(&row.execution_id).unwrap(), 1, "the journal keeps the first pull");

    let again = rig.run(&opaque(), "1.0.0", "run-2", &mut source, &mut sink).unwrap();
    assert_eq!(again.status, RunStatus::Success);
    assert_eq!(again.execution_id, row.execution_id, "the next run resumes the held execution");
    assert_eq!(sink.commits.len(), 1);
    assert_eq!(sink.commits[0].batches.len(), 3);
    assert_eq!(source.inner.calls().iter().filter(|(p, _)| p.is_none()).count(), 1, "the recorded first pull replays");
    for run in ["run-1", "run-2"] {
        assert_ne!(rig.row(run).status, RunStatus::PartialFailure, "a closing run never reads partial_failure");
    }
}

/// The runner stages each shaped batch through the destination as one part before its next pull, so a run holds
/// one pulled batch in memory; the commit names the staged parts.
// spec: run.own.backpressure@417ad0e7
#[test]
fn each_batch_stages_before_the_next_pull() {
    let rig = Rig::new();
    let mut source = Pages::new(three_pages());
    let calls = source.calls.clone();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let mut sink = Sink { on_stage: Some(Box::new(move |s| log.lock().unwrap().push((s.ordinal, s.row_offset, calls.lock().unwrap().len())))), ..Sink::default() };
    let row = rig.run(&opaque(), "1.0.0", "run-1", &mut source, &mut sink).unwrap();
    assert_eq!(row.status, RunStatus::Success);
    // (ordinal, rows staged before it, pulls served when it staged)
    assert_eq!(*seen.lock().unwrap(), [(0, 0, 1), (1, 2, 2), (2, 3, 3)]);
    assert_eq!(ids(&sink.commits[0]), [vec!["d1", "d2"], vec!["d3"], vec!["d4"]]);
}

/// A stage carrying the parts a run staged since its last commit past 1 GiB raises `RunStagedBytesExceeded`,
/// deterministic; the run commits nothing more and retires its owner, so the next fire pulls afresh from the stored
/// position. A replacing table takes no checkpoint, so its run reaches the bound.
// spec: run.own.staged-bytes@0ba44514
#[test]
fn a_run_staging_past_the_bound_commits_nothing() {
    let rig = Rig::new();
    let mut source = Pages::new(three_pages());
    let mut sink = Sink { part_bytes: 400 * 1024 * 1024, replace: true, ..Sink::default() };
    let row = rig.run(&opaque(), "1.0.0", "run-1", &mut source, &mut sink).unwrap();
    assert_eq!((row.status, row.error_kind), (RunStatus::Failed, Some(FailureTag::Permanent)));
    let m = row.error_message.unwrap();
    assert!(m.starts_with("RunStagedBytesExceeded") && m.contains("1073741824"), "{m}");
    assert_eq!(sink.staged.len(), 3, "the third part carried the run past 1 GiB");
    assert!(sink.commits.is_empty());
    assert_eq!(sink.discarded, [("filings".to_string(), "run-1".to_string())], "the failed run's staged parts are discarded");
    assert_eq!(rig.catalog().cursor("feed", "filings").unwrap().position, None, "the position stands");
    // The failure retires the owner and collects its journal, so the next fire pulls afresh.
    let failed = rig.row("run-1").execution_id;
    assert!(rig.catalog().owner("feed", "filings").unwrap().is_none(), "the bound releases the owner");
    assert_eq!(rig.engine.journal.recorded(&failed).unwrap(), 0);
    let again = rig.run(&opaque(), "1.0.0", "run-2", &mut source, &mut sink).unwrap();
    assert_ne!(again.execution_id, failed, "the next fire opens a fresh execution");
    assert_eq!(source.calls().len(), 6, "the next fire pulls every page again rather than replaying");

    // Two parts of 512 MiB sit at the bound and commit.
    let rig = Rig::new();
    let mut sink = Sink { part_bytes: 512 * 1024 * 1024, replace: true, ..Sink::default() };
    let two = vec![vec![json!({"id": "d1"})], vec![json!({"id": "d2"})]];
    assert_eq!(rig.run(&opaque(), "1.0.0", "run-1", &mut Pages::new(two), &mut sink).unwrap().status, RunStatus::Success);
}

/// The fire count a backfill reports when it has not landed every row within its fires.
const NOT_DONE: u64 = 4;

/// Sixteen one-row pages, each staging a 128 MiB part, make a 2 GiB window; the window is given three fires
/// to land every row, and reports [`NOT_DONE`] when it has not. Each time the parts staged since the run's last
/// commit reach 512 MiB, the run commits them with the position after its last staged pull and keeps pulling.
// spec: run.own.checkpoint-commit@18a72e19
// spec: run.own.checkpoint-run-id@17b3a540
#[test]
fn a_window_staging_two_gib_completes_in_one_fire() {
    let rig = Rig::new();
    let pages: Vec<Vec<Value>> = (0..16).map(|n| vec![json!({ "id": format!("d{n:02}") })]).collect();
    let mut source = Pages::new(pages);
    let mut sink = Sink { part_bytes: 128 * 1024 * 1024, ..Sink::default() };
    let mut fires = NOT_DONE;
    for fire in 1..=3 {
        rig.run(&opaque(), "1.0.0", &format!("run-{fire}"), &mut source, &mut sink).unwrap();
        let landed: std::collections::BTreeSet<String> = sink.commits.iter().flat_map(ids).flatten().collect();
        if landed.len() == 16 {
            fires = fire;
            break;
        }
        rig.clock.advance(60);
    }
    let committed: Vec<String> = sink.commits.iter().flat_map(ids).flatten().collect();
    let distinct: std::collections::BTreeSet<&String> = committed.iter().collect();
    assert_eq!(distinct.len(), committed.len(), "no row lands twice across fires");
    let parts_min = sink.commits.iter().map(|c| c.batches.len()).min().unwrap_or(0);
    let seed = 0x5eed_0b01;
    contextful_eval::record::emit("backfill-fires-to-complete", fires as f64, 1, seed);
    contextful_eval::record::emit("backfill-window-commits", sink.commits.len() as f64, 1, seed);
    contextful_eval::record::emit("backfill-parts-per-commit-min", parts_min as f64, 1, seed);
    assert_eq!(fires, 1, "one fire lands the window");
    assert_eq!(sink.commits.iter().map(|c| c.batches.len()).collect::<Vec<_>>(), [4, 4, 4, 4], "a commit per 512 MiB staged");
    let cursors: Vec<Option<Value>> = sink.commits.iter().map(|c| c.cursor.clone()).collect();
    assert_eq!(cursors, [Some(json!("p4")), Some(json!("p8")), Some(json!("p12")), Some(json!("p16"))], "each commit carries the position after its last staged pull");
    let runs: Vec<&str> = sink.commits.iter().map(|c| c.run_id.as_str()).collect();
    assert_eq!(runs, ["run-1", "run-1.000001", "run-1.000002", "run-1.000003"], "each commit names its own parts");
    assert_eq!(rig.row("run-1").status, RunStatus::Success);
    assert!(rig.catalog().owner("feed", "filings").unwrap().is_none(), "the run retires its owner once");
    assert_eq!(rig.catalog().cursor("feed", "filings").unwrap().position, Some(json!("p16")));
}

/// A run dying after a checkpoint commit keeps what the checkpoint committed: the catalog caches the checkpoint's
/// position, so the next fire pulls from it and lands every later row once.
// spec: run.own.checkpoint-resume@2e38b3b8
#[test]
fn a_fire_after_a_crash_resumes_from_the_last_checkpoint() {
    let rig = Rig::new();
    let pages: Vec<Vec<Value>> = (0..16).map(|n| vec![json!({ "id": format!("d{n:02}") })]).collect();
    let mut source = Pages::new(pages);
    source.die_after = Some(9);
    let mut sink = Sink { part_bytes: 128 * 1024 * 1024, ..Sink::default() };
    rig.crash(&opaque(), "run-1", &mut source, &mut sink);
    assert_eq!(sink.commits.iter().map(|c| c.cursor.clone()).collect::<Vec<_>>(), [Some(json!("p4")), Some(json!("p8"))]);
    assert_eq!(rig.catalog().cursor("feed", "filings").unwrap().position, Some(json!("p8")), "the catalog caches the checkpoint");
    rig.clock.advance(60);
    let row = rig.run(&opaque(), "1.0.0", "run-2", &mut source, &mut sink).unwrap();
    assert_eq!(row.status, RunStatus::Success);
    let asked: Vec<Option<Value>> = source.calls().iter().skip(10).map(|(p, _)| p.clone()).collect();
    assert_eq!(asked.first(), Some(&Some(json!("p8"))), "the next fire pulls from the checkpoint");
    let committed: Vec<String> = sink.commits.iter().flat_map(ids).flatten().collect();
    let distinct: std::collections::BTreeSet<&String> = committed.iter().collect();
    assert_eq!((committed.len(), distinct.len()), (16, 16), "every row lands once");
}

/// A run whose later commits would land under a run id past the store's 128-char bound refuses before its first
/// checkpoint commits, so no part of its window lands.
// spec: run.own.checkpoint-id-length@fd2d05c6
#[test]
fn a_run_id_too_long_for_a_checkpoint_commits_nothing() {
    let rig = Rig::new();
    let run_id = "r".repeat(122);
    let pages: Vec<Vec<Value>> = (0..16).map(|n| vec![json!({ "id": format!("d{n:02}") })]).collect();
    let mut sink = Sink { part_bytes: 128 * 1024 * 1024, ..Sink::default() };
    let row = rig.run(&opaque(), "1.0.0", &run_id, &mut Pages::new(pages), &mut sink).unwrap();
    assert_eq!((row.status, row.error_kind), (RunStatus::Failed, Some(FailureTag::Permanent)));
    assert!(row.error_message.as_deref().unwrap_or_default().starts_with("RunCheckpointIdTooLong"), "{:?}", row.error_message);
    assert!(sink.commits.is_empty(), "no checkpoint commits: {:?}", sink.commits.iter().map(|c| &c.run_id).collect::<Vec<_>>());
    assert_eq!(rig.catalog().cursor("feed", "filings").unwrap().position, None, "the cursor stays where it stood");
    assert!(rig.catalog().owner("feed", "filings").unwrap().is_none(), "the refusal retires its owner");

    // A run id of 121 chars leaves room for every later commit's segment.
    let rig = Rig::new();
    let pages: Vec<Vec<Value>> = (0..8).map(|n| vec![json!({ "id": format!("d{n:02}") })]).collect();
    let mut sink = Sink { part_bytes: 128 * 1024 * 1024, ..Sink::default() };
    let row = rig.run(&opaque(), "1.0.0", &"r".repeat(121), &mut Pages::new(pages), &mut sink).unwrap();
    assert_eq!(row.status, RunStatus::Success);
    assert_eq!(sink.commits.iter().map(|c| c.run_id.len()).collect::<Vec<_>>(), [121, 128]);
}

/// A source serving [`Pages`] and failing deterministically on its `fail_at`th pull.
struct FailingAt {
    inner: Pages,
    pulls: usize,
    fail_at: usize,
}

impl Source for FailingAt {
    fn pull(&mut self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        self.pulls += 1;
        if self.pulls == self.fail_at {
            return Err(Failure::deterministic(FailureTag::Permanent, "the vendor refused the page"));
        }
        self.inner.pull(request, cancel)
    }
}

/// A run failing after a checkpoint's commit lands records on its row the rows and bytes every landed checkpoint
/// commit carried, including one whose cursor update then fails.
// spec: run.own.checkpoint-landed@855bd346
#[test]
fn a_failed_run_records_what_its_checkpoints_landed() {
    let rig = Rig::new();
    let pages: Vec<Vec<Value>> = (0..16).map(|n| vec![json!({ "id": format!("d{n:02}") })]).collect();
    let mut source = FailingAt { inner: Pages::new(pages), pulls: 0, fail_at: 10 };
    let mut sink = Sink { part_bytes: 128 * 1024 * 1024, ..Sink::default() };
    let row = rig.run(&opaque(), "1.0.0", "run-1", &mut source, &mut sink).unwrap();
    assert_eq!(row.status, RunStatus::Failed);
    assert_eq!(sink.commits.len(), 2, "two checkpoints committed before the failure");
    assert_eq!((row.rows, row.bytes), (8, 8 * 128 * 1024 * 1024), "the row counts what the checkpoints landed");
    assert_eq!(rig.row("run-1").rows, 8);

    // The third checkpoint's commit lands, then the cursor row moves before the run updates it.
    let rig = Rig::new();
    let pages: Vec<Vec<Value>> = (0..16).map(|n| vec![json!({ "id": format!("d{n:02}") })]).collect();
    let mut source = Pages::new(pages);
    let sink = Sink { part_bytes: 128 * 1024 * 1024, ..Sink::default() };
    let mut dest = MovingCursor { inner: sink, catalog: rig.engine.catalog.clone(), at: 3 };
    let row = rig.run(&opaque(), "1.0.0", "run-1", &mut source, &mut dest).unwrap();
    assert_eq!(row.status, RunStatus::Failed);
    assert_eq!(dest.inner.commits.len(), 3, "the third checkpoint's commit landed before its cursor update failed");
    assert_eq!((row.rows, row.bytes), (12, 12 * 128 * 1024 * 1024), "the row counts every commit that landed");
}

/// A [`Sink`] whose `at`-th commit lands, then moves the cursor row's version before the run updates it.
struct MovingCursor {
    inner: Sink,
    catalog: Arc<dyn Catalog + Send + Sync>,
    at: usize,
}

impl Destination for MovingCursor {
    fn replaces(&self, table: &str) -> bool {
        self.inner.replaces(table)
    }

    fn stage_batch(&mut self, stage: Stage) -> Result<Part, Failure> {
        self.inner.stage_batch(stage)
    }

    fn commit(&mut self, commit: Commit, precommit: &dyn Fn() -> Result<(), Failure>) -> Result<Landed, Failure> {
        let landed = self.inner.commit(commit, precommit)?;
        if self.inner.commits.len() == self.at {
            let current = self.catalog.cursor("feed", "filings")?;
            let moved = contextful_core::coordinate::CursorRow { position: Some(json!({ "page": 0 })), ..current.clone() };
            self.catalog.cursor_cas("feed", "filings", current.version, moved, None)?;
        }
        Ok(landed)
    }

    fn discard(&mut self, table: &str, run_id: &str) -> Result<(), Failure> {
        self.inner.discard(table, run_id)
    }

    fn newest_marker(&self, pipeline_id: &str, table: &str) -> Result<Option<Marker>, Failure> {
        self.inner.newest_marker(pipeline_id, table)
    }
}


/// A source serving `pages` of dated rows one page a pull, carrying the next page's index as its position.
struct DatedWalk {
    pages: Vec<Vec<Value>>,
}

impl Source for DatedWalk {
    fn pull(&mut self, request: &PullRequest, _: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        let n = request.position.as_ref().and_then(|p| p.get("next")).and_then(Value::as_u64).unwrap_or(0) as usize;
        let more = n + 1 < self.pages.len();
        let cursor = more.then(|| json!({ "next": n + 1 }));
        Ok(serde_json::to_vec(&json!({ "rows": self.pages[n], "cursor": cursor, "more": more })).unwrap())
    }
}

/// Under a monotonic cursor a pull carrying a position continues its walk, and a checkpoint waits for a pull whose
/// every clock passes the run's frontier, so no commit's position splits the rows of one instant.
// spec: run.own.checkpoint-frontier@f7da4141
#[test]
fn a_monotonic_checkpoint_waits_for_a_strict_frontier_advance() {
    let rig = Rig::new();
    let row = |id: &str, at: i64| json!({ "id": id, "updated_at": at });
    let pages = vec![vec![row("a", 1), row("b", 1)], vec![row("c", 1)], vec![row("d", 2)], vec![row("e", 2)], vec![row("f", 3)]];
    let mut source = DatedWalk { pages };
    let mut sink = Sink { part_bytes: 512 * 1024 * 1024, ..Sink::default() };
    let p = plan("kind = \"monotonic\"\nfield = \"updated_at\"", "");
    assert_eq!(rig.run(&p, "1.0.0", "run-1", &mut source, &mut sink).unwrap().status, RunStatus::Success);
    let at = |n: i64| Some(json!({ "field": "updated_at", "at": n }));
    assert_eq!(sink.commits.iter().map(|c| c.cursor.clone()).collect::<Vec<_>>(), [at(1), at(2), at(3)]);
    assert_eq!(sink.commits.iter().map(ids).collect::<Vec<_>>(), [vec![vec!["a", "b"], vec!["c"]], vec![vec!["d"], vec!["e"]], vec![vec!["f"]]]);
}

/// A source walking `pages` as [`DatedWalk`] does, then answering every later pull with no rows, the position it
/// was asked and more to come, until `cap` pulls end it.
struct StallingWalk {
    pages: Vec<Vec<Value>>,
    pulls: usize,
    cap: usize,
}

impl Source for StallingWalk {
    fn pull(&mut self, request: &PullRequest, _: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        self.pulls += 1;
        let n = request.position.as_ref().and_then(|p| p.get("next")).and_then(Value::as_u64).unwrap_or(0) as usize;
        let body = match self.pages.get(n) {
            Some(rows) => json!({ "rows": rows, "cursor": { "next": n + 1 }, "more": true }),
            None => json!({ "rows": [], "cursor": request.position, "more": self.pulls < self.cap }),
        };
        Ok(serde_json::to_vec(&body).unwrap())
    }
}

/// Under a monotonic cursor a pull answering the position it asked, with no frontier advance, ends the walk, so a
/// source reporting more forever cannot hold the run open.
// spec: run.advance.walk-position@b835b6ce
#[test]
fn a_monotonic_walk_ends_when_the_source_repeats_its_position() {
    let rig = Rig::new();
    let row = |id: &str, at: i64| json!({ "id": id, "updated_at": at });
    let mut source = StallingWalk { pages: vec![vec![row("a", 1)], vec![row("b", 2)]], pulls: 0, cap: 50 };
    let mut sink = Sink::default();
    let p = plan("kind = \"monotonic\"\nfield = \"updated_at\"", "");
    assert_eq!(rig.run(&p, "1.0.0", "run-1", &mut source, &mut sink).unwrap().status, RunStatus::Success);
    assert_eq!(source.pulls, 3, "the walk ends on the pull that repeats its position");
    assert_eq!(sink.commits.iter().flat_map(ids).flatten().collect::<Vec<_>>(), ["a", "b"]);
    assert_eq!(rig.catalog().cursor("feed", "filings").unwrap().position, Some(json!({ "field": "updated_at", "at": 2 })));
}

/// A vendor answering `GET /bars?page=<n>[&since=<date>]` from `pages` pages of `per_page` daily bars, ordered
/// by date then symbol, `symbols` to a date; a page past the last answers no rows.
struct SyntheticVendor {
    pages: usize,
    per_page: usize,
    symbols: usize,
    requests: std::sync::atomic::AtomicUsize,
}

impl SyntheticVendor {
    /// The `n`th bar: its symbol and its date, an instant one day past the previous date.
    fn bar(&self, n: usize) -> Value {
        let date = contextful_core::time::Instant::parse("2000-01-03T00:00:00Z").unwrap().plus_secs(86_400 * (n / self.symbols) as u64);
        json!({ "symbol": format!("S{:02}", n % self.symbols), "date": date.to_rfc3339(), "close": n })
    }
}

impl contextful_outbound::Transport for SyntheticVendor {
    fn resolve(&self, _: &str, port: u16) -> Result<Vec<std::net::SocketAddr>, String> {
        Ok(vec![std::net::SocketAddr::new("93.184.216.34".parse().unwrap(), port)])
    }

    fn send(&self, request: &contextful_outbound::egress::Outbound<'_>) -> Result<contextful_outbound::egress::Inbound, contextful_outbound::egress::TransportFault> {
        self.requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let query = |name: &str| request.url.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned());
        let page: usize = query("page").and_then(|p| p.parse().ok()).unwrap_or(1);
        let since = query("since");
        let total = self.pages * self.per_page;
        // `since` is inclusive: the bars of the stored date are served again.
        let first = since.map_or(0, |s| (0..total).find(|&n| self.bar(n)["date"].as_str().unwrap() >= s.as_str()).unwrap_or(total));
        let start = (first + (page - 1) * self.per_page).min(total);
        let end = (start + self.per_page).min(total);
        let rows: Vec<Value> = (start..end).map(|n| self.bar(n)).collect();
        Ok(contextful_outbound::egress::Inbound { status: 200, headers: vec![], body: serde_json::to_vec(&json!({ "data": rows })).unwrap() })
    }
}

/// A 2000-page vendor of ten bars a page, fifty symbols to a date, read through the generic HTTP source under a
/// `date` watermark, a fresh source each fire as the scheduler builds one; one fire lands it.
#[test]
fn a_2000_page_vendor_lands_every_page_in_one_fire() {
    use contextful_connectors::http::{HttpConfig, HttpSource, Mediation};
    let vendor = Arc::new(SyntheticVendor { pages: 2000, per_page: 10, symbols: 50, requests: Default::default() });
    let config = HttpConfig::parse(&json!({ "endpoint": "https://vendor.example/bars", "records": "/data", "page_param": "page", "start_page": 1, "since_param": "since" })).unwrap();
    let clock = Arc::new(contextful_core::ports::FixedClock(crate::support::at(crate::support::T0)));
    let resolver = Arc::new(contextful_outbound::Resolver::new(Vec::new(), false, clock));
    let rig = Rig::new();
    let watermark = plan("kind = \"monotonic\"\nfield = \"date\"", "");
    let mut sink = Sink::default();
    let mut fires = NOT_DONE;
    let total = vendor.pages * vendor.per_page;
    let landed = |sink: &Sink| -> std::collections::BTreeMap<(String, String), usize> {
        let mut count = std::collections::BTreeMap::new();
        for row in sink.commits.iter().flat_map(|c| c.batches.iter().flatten()) {
            *count.entry((row["symbol"].as_str().unwrap().to_string(), row["date"].as_str().unwrap().to_string())).or_default() += 1;
        }
        count
    };
    for fire in 1..=3 {
        let mediation = Mediation { transport: Some(vendor.clone()), ..Mediation::default() };
        let mut source = HttpSource::mediated(config.clone(), "bars", resolver.clone(), mediation).unwrap().watermarked_for("date");
        rig.run(&watermark, "1.0.0", &format!("run-{fire}"), &mut source, &mut sink).unwrap();
        if landed(&sink).len() == total {
            fires = fire;
            break;
        }
        rig.clock.advance(3600);
    }
    let once = landed(&sink).values().filter(|&&n| n == 1).count();
    let served: std::collections::BTreeSet<(String, String)> =
        (0..total).map(|n| vendor.bar(n)).map(|b| (b["symbol"].as_str().unwrap().to_string(), b["date"].as_str().unwrap().to_string())).collect();
    assert_eq!(landed(&sink).keys().filter(|k| !served.contains(*k)).count(), 0, "every landed bar is one the vendor serves");
    let seed = 0x5eed_0b02;
    contextful_eval::record::emit("backfill-http-fires-to-complete", fires as f64, 1, seed);
    contextful_eval::record::emit("backfill-http-rows-landed", once as f64 / total as f64, total as u64, seed);
    assert_eq!(fires, 1, "one fire walks every page");
    assert_eq!(once, total, "every bar lands exactly once");
    let last = vendor.bar(total - 1)["date"].clone();
    assert_eq!(rig.catalog().cursor("feed", "filings").unwrap().position, Some(json!({ "field": "date", "at": last })), "the watermark advances when the walk ends");
}

/// A vendor answering `GET /items?p=<n>` with the `n`th id of its listing, one to a page, and the next page's URL; it
/// panics once serving the page named in `die_on`, as a process dying mid-walk leaves its state.
struct ListingVendor {
    listing: Mutex<Vec<String>>,
    die_on: Mutex<Option<usize>>,
}

impl ListingVendor {
    fn new(ids: &[&str]) -> Arc<ListingVendor> {
        Arc::new(ListingVendor { listing: Mutex::new(ids.iter().map(|id| id.to_string()).collect()), die_on: Mutex::new(None) })
    }
}

impl contextful_outbound::Transport for ListingVendor {
    fn resolve(&self, _: &str, port: u16) -> Result<Vec<std::net::SocketAddr>, String> {
        Ok(vec![std::net::SocketAddr::new("93.184.216.34".parse().unwrap(), port)])
    }

    fn send(&self, request: &contextful_outbound::egress::Outbound<'_>) -> Result<contextful_outbound::egress::Inbound, contextful_outbound::egress::TransportFault> {
        let page: usize = request.url.query_pairs().find(|(k, _)| k == "p").and_then(|(_, v)| v.parse().ok()).unwrap_or(1);
        if *self.die_on.lock().unwrap() == Some(page) {
            *self.die_on.lock().unwrap() = None;
            panic!("the process dies while the vendor serves page {page}");
        }
        let listing = self.listing.lock().unwrap().clone();
        let rows: Vec<Value> = listing.get(page - 1).map(|id| json!({ "id": id })).into_iter().collect();
        let next = if page < listing.len() { json!(format!("https://vendor.example/items?p={}", page + 1)) } else { Value::Null };
        Ok(contextful_outbound::egress::Inbound { status: 200, headers: vec![], body: serde_json::to_vec(&json!({ "data": rows, "next": next })).unwrap() })
    }
}

/// A generic HTTP source over `vendor` under next-URL pagination, fresh as the scheduler builds one each fire.
fn url_walk(vendor: &Arc<ListingVendor>) -> contextful_connectors::http::HttpSource {
    use contextful_connectors::http::{HttpConfig, HttpSource, Mediation};
    let config = HttpConfig::parse(&json!({ "endpoint": "https://vendor.example/items", "records": "/data", "next_url_path": "/next" })).unwrap();
    let clock = Arc::new(contextful_core::ports::FixedClock(crate::support::at(crate::support::T0)));
    let resolver = Arc::new(contextful_outbound::Resolver::new(Vec::new(), false, clock));
    let transport: Arc<dyn contextful_outbound::Transport> = vendor.clone();
    HttpSource::mediated(config, "items", resolver, Mediation { transport: Some(transport), ..Mediation::default() }).unwrap()
}

/// A next-URL walk lands in one pull and commits once. An appending table's walk dies on its third page and the
/// listing gains a first row; the next fire walks from the first page and lands each listed row once. A replacing
/// table's walk dying on three fires publishes nothing, and the fourth fire succeeds with the whole listing.
// spec: run.own.checkpoint-scope@b041c9d3
#[test]
fn an_interrupted_url_walk_resumes_from_its_first_page() {
    let rig = Rig::new();
    let vendor = ListingVendor::new(&["r1", "r2", "r3", "r4"]);
    *vendor.die_on.lock().unwrap() = Some(3);
    let mut sink = Sink::default();
    rig.crash(&opaque(), "run-1", &mut url_walk(&vendor), &mut sink);
    assert!(sink.commits.is_empty(), "the walk commits nothing before its end");
    vendor.listing.lock().unwrap().insert(0, "r0".into());
    rig.clock.advance(60);
    let row = rig.run(&opaque(), "1.0.0", "run-2", &mut url_walk(&vendor), &mut sink).unwrap();
    assert_eq!((row.status, row.rows), (RunStatus::Success, 5));
    let mut landed: Vec<String> = sink.commits.iter().flat_map(ids).flatten().collect();
    landed.sort();
    assert_eq!(landed, ["r0", "r1", "r2", "r3", "r4"], "each listed row lands once");

    // A replacing table's interrupted walks never succeed over zero rows.
    let rig = Rig::new();
    let vendor = ListingVendor::new(&["r1", "r2", "r3", "r4"]);
    let mut sink = Sink { replace: true, ..Sink::default() };
    for (fire, page) in [(1, 3), (2, 2), (3, 4)] {
        *vendor.die_on.lock().unwrap() = Some(page);
        rig.crash(&opaque(), &format!("run-{fire}"), &mut url_walk(&vendor), &mut sink);
        rig.clock.advance(60);
    }
    assert!(sink.commits.is_empty(), "no interrupted walk publishes");
    let row = rig.run(&opaque(), "1.0.0", "run-4", &mut url_walk(&vendor), &mut sink).unwrap();
    assert_eq!((row.status, row.rows), (RunStatus::Success, 4), "the resumed fire lands the whole listing");
    assert_eq!(sink.commits.len(), 1);
    assert_eq!(ids(&sink.commits[0]).concat(), ["r1", "r2", "r3", "r4"]);
    for fire in 1..=3 {
        let crashed = rig.row(&format!("run-{fire}"));
        assert!(!(crashed.status == RunStatus::Success && crashed.rows == 0), "run-{fire} reports {:?} over {} rows", crashed.status, crashed.rows);
    }
}

/// A source whose every pull moves the clock 5 s on.
struct Ticking {
    inner: Pages,
    clock: crate::support::SetClock,
}

impl Source for Ticking {
    fn pull(&mut self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        self.clock.advance(5);
        self.inner.pull(request, cancel)
    }
}

/// A run stages its parts with no instant and commits them at the instant after its last pull, which every row
/// and the marker carry.
#[test]
fn a_run_commits_its_parts_at_the_instant_after_its_last_pull() {
    let rig = Rig::new();
    let mut source = Ticking { inner: Pages::new(three_pages()), clock: rig.clock.clone() };
    let mut sink = Sink::default();
    rig.run(&opaque(), "1.0.0", "run-1", &mut source, &mut sink).unwrap();
    assert_eq!(sink.staged.len(), 3);
    assert_eq!(sink.commits[0].committed_at, crate::support::at("2030-01-01T00:00:15Z"));
    assert!(sink.discarded.is_empty(), "a committed run discards nothing");
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

/// A commit carries its plan's declared cursor kind, so a destination writing a commit log records which kind
/// reached it.
#[test]
fn a_commit_carries_the_declared_cursor_kind() {
    use contextful_core::run::advance::CursorKind;
    let rig = Rig::new();
    let mut sink = Sink::default();
    rig.run(&opaque(), "1.0.0", "run-1", &mut Pages::new(three_pages()), &mut sink).unwrap();
    let watermark = plan("kind = \"monotonic\"\nfield = \"updated_at\"", "");
    let mut source = Polled { rows: vec![json!({"id": "a", "updated_at": 5})], calls: 0 };
    let mut watermarked = Sink::default();
    Rig::new().run(&watermark, "1.0.0", "run-2", &mut source, &mut watermarked).unwrap();
    let kinds: Vec<CursorKind> = sink.commits.iter().chain(&watermarked.commits).map(|c| c.cursor_kind).collect();
    assert_eq!(kinds, [CursorKind::OpaqueToken, CursorKind::Monotonic]);
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

/// Builds that link a component host run component connectors: the full profile and container or worker deployments
/// built from that profile.
// spec: topology.package.component-host@cb1c5d44
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

/// A source serving fixed pull bodies in order, each continuing to the next.
struct Bodies(Vec<Value>);

impl Source for Bodies {
    fn pull(&mut self, request: &PullRequest, _: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        let n = match &request.position {
            None => 0,
            Some(Value::String(p)) => p.trim_start_matches('p').parse::<usize>().unwrap(),
            Some(other) => panic!("position {other}"),
        };
        let mut body = self.0[n].clone();
        body["cursor"] = json!(format!("p{}", n + 1));
        body["more"] = json!(n + 1 < self.0.len());
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

/// A walk serving its declined inputs by extension over two pulls.
struct Declining {
    declined: Vec<Value>,
}

impl Source for Declining {
    fn pull(&mut self, request: &PullRequest, _cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        let n = request.position.as_ref().and_then(Value::as_u64).unwrap_or(0) as usize;
        let tally = &self.declined[n];
        let skipped: u64 = tally.as_object().map_or(0, |m| m.values().filter_map(Value::as_u64).sum());
        let body = json!({ "rows": [{"id": format!("d{n}")}], "cursor": n + 1, "more": n + 1 < self.declined.len(), "skipped": skipped, "declined": tally });
        Ok(serde_json::to_vec(&body).unwrap())
    }
}

/// A directory walk records on the run record what it declined, tallied by extension.
// spec: connector.source.declined-tally@85761d5b
#[test]
fn the_run_row_holds_every_pulls_declined_tally_by_extension() {
    let rig = Rig::new();
    let mut source = Declining { declined: vec![json!({"docx": 2, "png": 1}), json!({"docx": 1, "": 1})] };
    let row = rig.run(&opaque(), "1.0.0", "run-1", &mut source, &mut Sink::default()).unwrap();
    let want: std::collections::BTreeMap<String, u64> = [("".to_string(), 1), ("docx".to_string(), 3), ("png".to_string(), 1)].into();
    assert_eq!((row.status, row.skipped, &row.declined), (RunStatus::Success, 5, &want));
    assert_eq!(rig.row("run-1").declined, want, "the catalog holds the tally");

    let plain = Rig::new();
    let row = plain.run(&opaque(), "1.0.0", "run-1", &mut Pages::new(three_pages()), &mut Sink::default()).unwrap();
    assert!(row.declined.is_empty(), "a source declaring no tally leaves the row without one");
}

/// The run row holds every pull's audit entries in pull order; a source recording none leaves the row without them.
#[test]
fn the_run_row_holds_every_pulls_audit_entries_in_order() {
    let rig = Rig::new();
    let bodies = vec![
        json!({"rows": [{"id": "d1"}], "audit": ["d1: step `engine`: warming up"]}),
        json!({"rows": [{"id": "d2"}]}),
        json!({"rows": [{"id": "d3"}], "audit": ["d3: DeriveStepExit: step `engine` exited 3: boom", "d3: step `preprocess-0`: resampled"]}),
    ];
    let row = rig.run(&opaque(), "1.0.0", "run-1", &mut Bodies(bodies), &mut Sink::default()).unwrap();
    let want = ["d1: step `engine`: warming up", "d3: DeriveStepExit: step `engine` exited 3: boom", "d3: step `preprocess-0`: resampled"];
    assert_eq!((row.status, row.audit.as_slice()), (RunStatus::Success, want.map(String::from).as_slice()));
    assert_eq!(rig.row("run-1").audit, want, "the catalog holds the entries");

    let plain = Rig::new();
    let row = plain.run(&opaque(), "1.0.0", "run-1", &mut Pages::new(three_pages()), &mut Sink::default()).unwrap();
    assert!(row.audit.is_empty());
    assert!(serde_json::to_value(&row).unwrap().get("audit").is_none(), "an empty audit is not serialized");
}

/// A pull declaring a type for a column an earlier staged batch of its run carried undeclared raises
/// `PipelineTypeDeclaredLate`, deterministic, naming the column; the run commits nothing and retires its owner.
// spec: run.land.late-type@ee345c25
#[test]
fn a_type_declared_after_its_column_staged_refuses() {
    let rig = Rig::new();
    let late = vec![
        json!({"rows": [{"id": "d1", "ts": "2030-01-01T00:00:00Z"}]}),
        json!({"rows": [{"id": "d2", "ts": "2030-01-02T00:00:00Z"}], "types": {"ts": "timestamp"}}),
    ];
    let mut sink = Sink::default();
    let row = rig.run(&opaque(), "1.0.0", "run-1", &mut Bodies(late), &mut sink).unwrap();
    assert_eq!((row.status, row.error_kind), (RunStatus::Failed, Some(FailureTag::SchemaIncompatible)));
    let m = row.error_message.unwrap();
    assert!(m.contains("PipelineTypeDeclaredLate") && m.contains("`ts`"), "{m}");
    assert_eq!(sink.staged.len(), 1, "the refusal fires before the second stage");
    assert!(sink.commits.is_empty());
    assert_eq!(sink.discarded, [("filings".to_string(), "run-1".to_string())]);
    assert!(rig.catalog().owner("feed", "filings").unwrap().is_none(), "the refusal releases the owner");

    // A type first declared for a column no earlier batch carried stages in it.
    let rig = Rig::new();
    let fresh = vec![json!({"rows": [{"id": "d1"}]}), json!({"rows": [{"id": "d2", "ts": "2030-01-02T00:00:00Z"}], "types": {"ts": "timestamp"}})];
    let mut sink = Sink::default();
    assert_eq!(rig.run(&opaque(), "1.0.0", "run-1", &mut Bodies(fresh), &mut sink).unwrap().status, RunStatus::Success);
    assert_eq!(sink.staged[1].types.get("ts").map(|t| t.name()), Some("Timestamp".into()));
}

/// A destination whose second-page integer check can fail before a part is accepted.
struct SourceStageCheck {
    sink: Sink,
    protected: bool,
    prepared: usize,
    crash: bool,
    failure: Option<Failure>,
    on_refusal: Option<Box<dyn FnMut()>>,
}

impl SourceStageCheck {
    fn new(protected: bool) -> Self {
        Self { sink: Sink::default(), protected, prepared: 0, crash: false, failure: Some(Failure::deterministic(FailureTag::SchemaIncompatible, "score must be an integer")), on_refusal: None }
    }

    fn stage(&mut self, stage: Stage) -> Result<Part, Failure> {
        if stage.rows.iter().any(|row| row.get("score").is_some_and(|score| !score.is_i64())) {
            assert!(!self.crash, "the process dies after recording the incompatible source pull");
            if let Some(failure) = &self.failure {
                if let Some(observe) = &mut self.on_refusal { observe(); }
                return Err(failure.clone());
            }
        }
        self.sink.stage_batch(stage)
    }
}

impl Destination for SourceStageCheck {
    fn recording_identity(&self, _: &contextful_core::run::plan::Plan) -> Result<Option<String>, Failure> { Ok(self.protected.then(|| "source-stage-check".into())) }
    fn prepare_recorded(&mut self, _: &str, rows: Vec<Row>, _: Types, _: &str) -> Result<Value, Failure> { self.prepared += 1; Ok(json!(rows)) }
    fn stage_recorded(&mut self, mut stage: Stage, payload: &Value) -> Result<Part, Failure> { stage.rows = serde_json::from_value(payload.clone()).unwrap(); self.stage(stage) }
    fn stage_batch(&mut self, stage: Stage) -> Result<Part, Failure> { assert!(!self.protected); self.stage(stage) }
    fn commit(&mut self, commit: Commit, before: &dyn Fn() -> Result<(), Failure>) -> Result<Landed, Failure> { self.sink.commit(commit, before) }
    fn discard(&mut self, table: &str, run: &str) -> Result<(), Failure> { self.sink.discard(table, run) }
    fn newest_marker(&self, pipeline: &str, table: &str) -> Result<Option<Marker>, Failure> { self.sink.newest_marker(pipeline, table) }
}

fn incompatible_pages() -> Pages {
    Pages::new(vec![vec![json!({"id":"seed","score":0})], vec![json!({"id":"one","score":1})], vec![json!({"id":"two","score":{"invalid":true}})]])
}

#[test]
fn source_stage_schema_refusal_releases_recorded_pulls_without_advancing_the_committed_cursor() {
    for protected in [false, true] {
        let rig = Rig::new();
        let plan = opaque();
        let mut dest = SourceStageCheck::new(protected);
        rig.run(&plan, "1.0.0", "seed", &mut Pages::new(vec![vec![json!({"id":"seed","score":0})]]), &mut dest).unwrap();
        let before = rig.catalog().cursor("feed", "filings").unwrap();
        let mut source = incompatible_pages();
        let failed = rig.run(&plan, "1.0.0", "bad", &mut source, &mut dest).unwrap();
        assert_eq!((failed.status, failed.error_kind), (RunStatus::Failed, Some(FailureTag::SchemaIncompatible)));
        assert_eq!(dest.sink.commits.len(), 1, "no partial publication replaces the seed");
        assert_eq!(rig.catalog().cursor("feed", "filings").unwrap().position, before.position);
        assert!(dest.sink.discarded.contains(&("filings".into(), "bad".into())));
        source.pages[2] = vec![json!({"id":"two","score":2})];
        let repaired = rig.run(&plan, "1.0.0", "repaired", &mut source, &mut dest).unwrap();
        assert_eq!(repaired.status, RunStatus::Success, "the unchanged plan reads the repaired source");
        assert_ne!(repaired.execution_id, failed.execution_id);
        assert_eq!(rig.engine.journal.recorded(&failed.execution_id).unwrap(), 0);
        assert_eq!(ids(&dest.sink.commits[1]), [vec!["one"], vec!["two"]]);
        assert_eq!(source.calls().iter().map(|(position, _)| position.clone()).collect::<Vec<_>>(), [Some(json!("p1")), Some(json!("p2")), Some(json!("p1")), Some(json!("p2"))]);
    }
}

#[test]
fn source_stage_replay_releases_an_existing_poisoned_owner_for_repair() {
    for protected in [false, true] {
        let rig = Rig::new();
        let plan = opaque();
        let mut dest = SourceStageCheck::new(protected);
        dest.crash = true;
        let mut source = incompatible_pages();
        rig.crash(&plan, "old-owner", &mut source, &mut dest);
        let execution = rig.row("old-owner").execution_id;
        // A previous binary closed the failed attempt but retained these journal rows.
        rig.catalog().update_run("old-owner", &mut |row| {
            row.status = RunStatus::Failed;
            row.error_kind = Some(FailureTag::SchemaIncompatible);
            row.owner = None;
            Ok(())
        }).unwrap().unwrap().unwrap();
        assert_eq!(rig.engine.journal.recorded(&execution).unwrap(), 3);
        rig.clock.advance(60);
        let calls = source.calls().len();
        let prepared = dest.prepared;
        source.pages[2] = vec![json!({"id":"two","score":2})];
        dest.crash = false;
        let replay = rig.run(&plan, "1.0.0", "replay", &mut source, &mut dest).unwrap();
        assert_eq!((replay.status, replay.error_kind), (RunStatus::Failed, Some(FailureTag::SchemaIncompatible)));
        assert_eq!(source.calls().len(), calls, "the retained invalid pull is replayed, not fetched again");
        assert_eq!(dest.prepared, prepared, "prepared replay does not canonicalize the repaired source");
        assert!(rig.catalog().owner("feed", "filings").unwrap().is_none(), "replay retires the poisoned owner");
        assert!(dest.sink.commits.is_empty());
        let repaired = rig.run(&plan, "1.0.0", "fresh", &mut source, &mut dest).unwrap();
        assert_eq!(repaired.status, RunStatus::Success);
        assert_ne!(repaired.execution_id, execution);
        assert_eq!(source.calls().len(), calls + 3);
    }
}

#[test]
fn source_stage_non_schema_and_nondeterministic_failures_preserve_recorded_replay() {
    for protected in [false, true] {
        for failure in [Failure::new(FailureTag::Storage, "temporary write failure"), Failure::new(FailureTag::SchemaIncompatible, "schema check unavailable"), Failure::deterministic(FailureTag::Permanent, "another refusal")] {
            let rig = Rig::new();
            let mut dest = SourceStageCheck::new(protected);
            dest.failure = Some(failure);
            let mut source = incompatible_pages();
            let failed = rig.run(&opaque(), "1.0.0", "failed", &mut source, &mut dest).unwrap();
            assert_eq!(failed.status, RunStatus::Failed);
            let calls = source.calls().len();
            let prepared = dest.prepared;
            source.pages.clear();
            dest.failure = None;
            let resumed = rig.run(&opaque(), "1.0.0", "resumed", &mut source, &mut dest).unwrap();
            assert_eq!(resumed.status, RunStatus::Success);
            assert_eq!(resumed.execution_id, failed.execution_id);
            assert_eq!(source.calls().len(), calls);
            assert_eq!(dest.prepared, prepared);
            assert_eq!(dest.sink.commits[0].batches[2][0]["score"], json!({"invalid":true}));
        }
    }
}

#[test]
fn source_stage_schema_retirement_keeps_another_live_attempts_owner_and_journal() {
    let rig = Rig::new();
    let catalog = rig.engine.catalog.clone();
    let mut dest = SourceStageCheck::new(false);
    dest.on_refusal = Some(Box::new(move || {
        let mut row = catalog.run("refused").unwrap().unwrap();
        row.run_id = "other-live-attempt".into();
        catalog.put_run(&row).unwrap();
        let mut owner = catalog.owner("feed", "filings").unwrap().unwrap();
        owner.attempts.push(row.run_id);
        catalog.put_owner(&owner).unwrap();
    }));
    let failed = rig.run(&opaque(), "1.0.0", "refused", &mut incompatible_pages(), &mut dest).unwrap();
    assert_eq!(failed.status, RunStatus::Failed);
    assert_eq!(rig.catalog().owner("feed", "filings").unwrap().unwrap().execution_id, failed.execution_id);
    assert_eq!(rig.engine.journal.recorded(&failed.execution_id).unwrap(), 3);
    assert!(dest.sink.commits.is_empty());
}

/// Rate-limit pressure against one source paces only that source's runs.
// spec: run.retry.pressure-is-local@79bd9164
#[test]
fn a_rate_limited_source_paces_its_own_run_and_no_other() {
    let for_pipeline = |pipeline: &str| {
        contextful_core::run::plan::Plan::compile(
            format!("pipeline = \"{pipeline}\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1.0.0\"\ncommand = [\"vendor\"]\n[cursor]\nkind = \"opaque-token\"\n").as_bytes(),
        )
        .unwrap()
    };
    let rig = Rig::new();
    let engine = rig.engine.clone();
    let paced = for_pipeline("paced");
    let started = std::time::Instant::now();
    let slow = std::thread::spawn(move || {
        let mut source = Pages::new(three_pages());
        // The vendor asks for one second, twice, before it serves.
        source.fail = vec![Failure::new(FailureTag::RateLimited, "429").with_retry_after(1), Failure::new(FailureTag::RateLimited, "429").with_retry_after(1)];
        let spec = contextful_engine::RunSpec {
            connector: paced.connector_pin("artifact-1"),
            plan: paced,
            run_id: "run-paced".into(),
            site_id: "site-a".into(),
            pid: 1,
            boot_id: "boot".into(),
            trace_id: None,
        };
        let row = engine.run(&spec, &mut source, &mut Sink::default()).unwrap();
        (row, started.elapsed(), source.calls().len())
    });
    std::thread::sleep(std::time::Duration::from_millis(100));
    let quick_started = std::time::Instant::now();
    let row = rig.run(&for_pipeline("quick"), "1.0.0", "run-quick", &mut Pages::new(three_pages()), &mut Sink::default()).unwrap();
    let quick = quick_started.elapsed();
    assert_eq!(row.status, RunStatus::Success);
    assert!(quick < std::time::Duration::from_millis(900), "the other source's run waited {quick:?}");
    assert!(!slow.is_finished(), "the paced run still waits out its vendor's hint");

    let (paced_row, paced_took, calls) = slow.join().unwrap();
    assert_eq!(paced_row.status, RunStatus::Success);
    assert!(paced_took >= std::time::Duration::from_secs(2), "the paced run honored both hints: {paced_took:?}");
    assert_eq!(calls, 3 + 2);
}

/// A source answering one page whose cursor is `cursor`, recording the position each pull asked from.
struct Opaque {
    cursor: Value,
    asked: Arc<Mutex<Vec<Option<Value>>>>,
}

impl Source for Opaque {
    fn pull(&mut self, request: &PullRequest, _: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        self.asked.lock().unwrap().push(request.position.clone());
        Ok(serde_json::to_vec(&json!({ "rows": [{ "id": "r" }], "cursor": self.cursor, "more": false })).unwrap())
    }
}

/// A position is opaque bytes the connector owns, and its kind is a manifest fact the engine reads. Each table carries
/// its own position.
// spec: run.advance.cursor-bytes@888988bb
#[test]
fn a_position_round_trips_untouched_and_each_table_keeps_its_own() {
    let for_table = |table: &str, cursor: &str| {
        contextful_core::run::plan::Plan::compile(
            format!("pipeline = \"feed\"\ntable = \"{table}\"\n[connector]\nid = \"vendor\"\nversion = \"1.0.0\"\ncommand = [\"vendor\"]\n{cursor}").as_bytes(),
        )
        .unwrap()
    };
    // The kind is read off the manifest; undeclared, it is an opaque token.
    let filings = for_table("filings", "");
    assert_eq!(filings.cursor_kind, contextful_core::run::advance::CursorKind::OpaqueToken);
    assert_eq!(for_table("filings", "[cursor]\nkind = \"snapshot-id\"\n").cursor_kind, contextful_core::run::advance::CursorKind::SnapshotId);

    let rig = Rig::new();
    let token = json!({ "v": 3, "continuation": "Zm9vYmFy+/=", "nested": [null, 1.5, { "deep": "\u{0}tab\t" }] });
    let asked = Arc::new(Mutex::new(Vec::new()));
    let mut source = Opaque { cursor: token.clone(), asked: asked.clone() };
    rig.run(&filings, "1.0.0", "run-1", &mut source, &mut Sink::default()).unwrap();
    rig.run(&filings, "1.0.0", "run-2", &mut source, &mut Sink::default()).unwrap();
    assert_eq!(*asked.lock().unwrap(), [None, Some(token.clone())], "the second run asks from the token, byte for byte");

    // Another table of the pipeline starts from its own position.
    let other_asked = Arc::new(Mutex::new(Vec::new()));
    let mut other = Opaque { cursor: json!("other-1"), asked: other_asked.clone() };
    rig.run(&for_table("returns", ""), "1.0.0", "run-3", &mut other, &mut Sink::default()).unwrap();
    assert_eq!(*other_asked.lock().unwrap(), [None]);
    assert_eq!(rig.catalog().cursor("feed", "filings").unwrap().position, Some(token));
    assert_eq!(rig.catalog().cursor("feed", "returns").unwrap().position, Some(json!("other-1")));
}

/// A delivered batch runs under the owner scope its batch key fixes: a resend under the key resumes that owner's
/// recorded pull, and another key's batch runs under a fresh execution that leaves it pending.
// spec: connector.source.receive-recorded@99489281
#[test]
fn a_receive_batch_key_resumes_its_own_owner_and_no_other_keys() {
    use contextful_core::run::own::OwnerScope;
    use contextful_core::run::ports::Unshaped;
    let rig = Rig::new();
    let spec = |run_id: &str| contextful_engine::RunSpec {
        connector: opaque().connector_pin("artifact-1"),
        plan: opaque(),
        run_id: run_id.into(),
        site_id: "site-a".into(),
        pid: 4242,
        boot_id: "boot-a".into(),
        trace_id: None,
    };
    let mut first = Pages::new(three_pages());
    first.die_after = Some(1);
    let mut sink = Sink::default();
    let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rig.engine.run_receive(&spec("k1.a"), "k1", &mut first, &Unshaped, &mut sink)));
    assert!(died.is_err(), "the run was expected to die");
    rig.clock.advance(60);
    assert!(rig.catalog().owner("feed", "filings").unwrap().is_none(), "a receive owner leaves the table scope free");
    let pending = rig.catalog().owner_at(&OwnerScope::receive("feed", "filings", "k1")).unwrap().unwrap();
    assert_eq!(pending.attempts, ["k1.a"]);

    let mut other = Pages::new(vec![vec![json!({ "id": "x1" })]]);
    let row = rig.engine.run_receive(&spec("k2.a"), "k2", &mut other, &Unshaped, &mut sink).unwrap();
    assert_eq!(row.status, RunStatus::Success);
    assert_ne!(row.execution_id, pending.execution_id, "another key runs under a fresh execution");
    assert!(rig.catalog().owner_at(&OwnerScope::receive("feed", "filings", "k1")).unwrap().is_some(), "another key leaves the pending owner");

    let row = rig.engine.run_receive(&spec("k1.b"), "k1", &mut first, &Unshaped, &mut sink).unwrap();
    assert_eq!(row.status, RunStatus::Success);
    assert_eq!(row.execution_id, pending.execution_id, "the resend resumes the key's owner");
    let firsts = first.calls().into_iter().filter(|(p, _)| p.is_none()).count();
    assert_eq!(firsts, 1, "the recorded first pull resolves for the resend without a call");
    assert_eq!(ids(sink.commits.last().unwrap()), [vec!["d1", "d2"], vec!["d3"], vec!["d4"]]);
}

/// A receive attempt whose process dies after its commit lands leaves its run row open and its owner pending; settled,
/// the row closes committed, the owner retires, and a lookup under the key replays the attempt instead of landing again.
#[test]
fn a_receive_attempt_torn_after_its_commit_settles_and_replays() {
    use contextful_core::run::own::OwnerScope;
    use contextful_core::run::ports::Unshaped;
    let rig = Rig::new();
    let attempt = contextful_engine::receive_stem("feed", "filings", "k1");
    let spec = contextful_engine::RunSpec {
        connector: opaque().connector_pin("artifact-1"),
        plan: opaque(),
        run_id: attempt.clone(),
        site_id: "site-a".into(),
        pid: 4242,
        boot_id: "boot-a".into(),
        trace_id: None,
    };
    let mut pages = Pages::new(three_pages());
    let mut sink = Sink { die_after_land: true, ..Sink::default() };
    let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rig.engine.run_receive(&spec, "k1", &mut pages, &Unshaped, &mut sink)));
    assert!(died.is_err(), "the run was expected to die after its commit");
    rig.clock.advance(60);
    assert_eq!(sink.commits.len(), 1, "the commit landed before the process died");
    assert!(rig.engine.receive_landed("feed", "filings", "k1").unwrap().is_err(), "the torn attempt's row never closed");

    let row = rig.engine.receive_settle("feed", "filings", &contextful_core::run::own::receive_digest("feed", "k1"), &attempt, 4).unwrap();
    assert_eq!((row.status, row.rows), (RunStatus::Success, 4));
    assert!(rig.catalog().owner_at(&OwnerScope::receive("feed", "filings", "k1")).unwrap().is_none(), "the settled attempt's owner retires");
    assert_eq!(rig.engine.receive_landed("feed", "filings", "k1").unwrap().map(|r| r.run_id), Ok(attempt), "a lookup under the key replays the settled attempt");
    assert_eq!(sink.commits.len(), 1, "no attempt lands the rows again");
}

/// The run record lists every bound hit with its step label, attempt and bound, a hit a later attempt recovered
/// from included.
// spec: run.retry.bound-hit-recorded@a6e7216b
#[test]
fn every_bound_hit_lands_on_the_run_record_even_when_a_retry_recovers() {
    use contextful_core::run::failure::{CALL_DEADLINE, LINEAR_MEMORY};
    let rig = Rig::new();
    let quick = plan("kind = \"opaque-token\"", "[retry]\nbackoff = \"fixed\"\nbase_ms = 1\njitter_ms = 0\nattempts = 5");
    let mut source = Pages::new(three_pages());
    source.fail = vec![
        Failure::new(FailureTag::Transient, "the guest trapped").with_bound(LINEAR_MEMORY),
        Failure::new(FailureTag::Transient, "reset"),
        Failure::new(FailureTag::Transient, "the guest ran past its call deadline").with_bound(CALL_DEADLINE),
    ];
    let row = rig.run(&quick, "1.0.0", "run-1", &mut source, &mut Sink::default()).unwrap();
    assert_eq!(row.status, RunStatus::Success);
    let hits: Vec<(u32, &str)> = row.bound_hits.iter().map(|h| (h.attempt, h.bound.as_str())).collect();
    assert_eq!(hits, [(1, LINEAR_MEMORY), (3, CALL_DEADLINE)], "a hit a later attempt recovered from stays listed; an unbounded failure adds none");
    assert_eq!(row.bound_hits.len(), 2);
    assert!(!row.bound_hits[0].step.is_empty());
    assert_eq!(row.bound_hits[0].step, row.bound_hits[1].step, "both hits struck the one pull step");
    assert_eq!(rig.catalog().run("run-1").unwrap().unwrap().bound_hits, row.bound_hits);
}
