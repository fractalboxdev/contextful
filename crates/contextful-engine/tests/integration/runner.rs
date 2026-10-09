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
    // The crashed run's two staged parts stay under its own name; the commit names run-2's three.
    assert_eq!(sink.staged.iter().filter(|s| s.run_id == "run-1").count(), 2);
    assert_eq!(sink.staged.iter().filter(|s| s.run_id == "run-2").map(|s| s.ordinal).collect::<Vec<_>>(), [0, 1, 2]);
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

/// A stage carrying one run's staged parts past 1 GiB raises `RunStagedBytesExceeded`, deterministic; the run
/// commits nothing and retires its owner, so the next fire pulls afresh from the stored position.
// spec: run.own.staged-bytes@f0b10952
#[test]
fn a_run_staging_past_the_bound_commits_nothing() {
    let rig = Rig::new();
    let mut source = Pages::new(three_pages());
    let mut sink = Sink { part_bytes: 400 * 1024 * 1024, ..Sink::default() };
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
    let mut sink = Sink { part_bytes: 512 * 1024 * 1024, ..Sink::default() };
    let two = vec![vec![json!({"id": "d1"})], vec![json!({"id": "d2"})]];
    assert_eq!(rig.run(&opaque(), "1.0.0", "run-1", &mut Pages::new(two), &mut sink).unwrap().status, RunStatus::Success);
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
