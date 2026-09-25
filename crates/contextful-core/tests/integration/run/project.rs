//! `run.project`: the wire snapshot, the total reducer, the metadata bound, versions and
//! coalescing.

use contextful_core::run::failure::{Failure, FailureTag};
use contextful_core::run::project::{
    reduce, Applied, Change, Coalescer, Delta, OutputRef, Snapshot, StepPatch, StepStatus, Version, COALESCE_WINDOW_MS,
    METADATA_MAX_BYTES,
};
use contextful_core::run::record::RunStatus;
use contextful_core::run::RunError;
use contextful_core::time::Instant;
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

const EPOCH: &str = "2030-01-01T00:00:00Z";

fn v(counter: u64) -> Version {
    Version { epoch: at(EPOCH), counter }
}

fn delta(counter: u64, change: Change) -> Delta {
    Delta { run_id: "run-1".into(), workflow_id: "filings-feed".into(), version: v(counter), change }
}

fn status(s: RunStatus) -> Change {
    Change::Status { status: s, at: Some(at("2030-01-01T00:00:05Z")), error: None }
}

fn fresh() -> Snapshot {
    Snapshot::new("run-1", "filings-feed", v(0))
}

/// The wire snapshot carries run id, workflow id, a status from {{run.record.status-set}}, an ordered step list, an application metadata map, optional start and end instants, an optional tagged error and a version. Every deploy target emits it identically.
// spec: run.project.wire-snapshot@4ec773ac
#[test]
fn the_wire_snapshot_carries_every_member_and_round_trips() {
    let mut s = fresh();
    reduce(&mut s, &delta(1, status(RunStatus::Running))).unwrap();
    reduce(&mut s, &delta(2, Change::Step(StepPatch::new("pull-0").status(StepStatus::Running)))).unwrap();
    reduce(&mut s, &delta(3, Change::Step(StepPatch::new("pull-1").status(StepStatus::Pending)))).unwrap();
    reduce(&mut s, &delta(4, Change::Metadata { entries: BTreeMap::from([("source".into(), json!("vendor"))]) })).unwrap();
    let failed = Change::Status { status: RunStatus::Failed, at: Some(at("2030-01-01T00:00:09Z")), error: Some(Failure::new(FailureTag::Permanent, "gone")) };
    reduce(&mut s, &delta(5, failed)).unwrap();

    let wire: Value = serde_json::from_str(&s.to_wire()).unwrap();
    assert_eq!(wire["run_id"], "run-1");
    assert_eq!(wire["workflow_id"], "filings-feed");
    assert_eq!(wire["status"], "failed");
    let labels: Vec<&str> = wire["steps"].as_array().unwrap().iter().map(|s| s["label"].as_str().unwrap()).collect();
    assert_eq!(labels, ["pull-0", "pull-1"]);
    assert_eq!(wire["metadata"]["source"], "vendor");
    assert_eq!(wire["started_at"], "2030-01-01T00:00:05Z");
    assert_eq!(wire["ended_at"], "2030-01-01T00:00:09Z");
    assert_eq!(wire["error"]["tag"], "Permanent");
    assert_eq!(wire["version"]["counter"], 5);
    // Every status of the record's set spells identically on the wire.
    for st in RunStatus::ALL {
        let mut one = fresh();
        reduce(&mut one, &delta(1, status(st))).unwrap();
        let w: Value = serde_json::from_str(&one.to_wire()).unwrap();
        assert_eq!(w["status"], st.name());
    }
    // The wire form decodes back to the identical snapshot.
    assert_eq!(serde_json::from_str::<Snapshot>(&s.to_wire()).unwrap(), s);
}

/// A step view carries its label, a status of `pending`, `running`, `completed`, `failed` or `retrying`, its attempt count, optional instants, an optional output reference with byte count, and an optional tagged failure.
// spec: run.project.step-view@f320728f
#[test]
fn a_step_view_carries_label_status_attempts_instants_output_and_failure() {
    let mut s = fresh();
    let mut p = StepPatch::new("pull-0").status(StepStatus::Retrying);
    p.attempts = Some(2);
    p.started_at = Some(at("2030-01-01T00:00:01Z"));
    p.failure = Some(Failure::new(FailureTag::Transient, "503"));
    reduce(&mut s, &delta(1, Change::Step(p))).unwrap();
    let mut done = StepPatch::new("pull-0").status(StepStatus::Completed);
    done.attempts = Some(3);
    done.ended_at = Some(at("2030-01-01T00:00:03Z"));
    done.output = Some(OutputRef { reference: "journal:ab12".into(), bytes: 412 });
    reduce(&mut s, &delta(2, Change::Step(done))).unwrap();

    let w: Value = serde_json::from_str(&s.to_wire()).unwrap();
    let step = &w["steps"][0];
    assert_eq!(step["label"], "pull-0");
    assert_eq!(step["status"], "completed");
    assert_eq!(step["attempts"], 3);
    assert_eq!(step["started_at"], "2030-01-01T00:00:01Z");
    assert_eq!(step["ended_at"], "2030-01-01T00:00:03Z");
    assert_eq!(step["output"], json!({"reference": "journal:ab12", "bytes": 412}));
    assert_eq!(step["failure"]["tag"], "Transient");
    let spellings: Vec<Value> = [StepStatus::Pending, StepStatus::Running, StepStatus::Completed, StepStatus::Failed, StepStatus::Retrying]
        .iter()
        .map(|s| serde_json::to_value(s).unwrap())
        .collect();
    assert_eq!(spellings, [json!("pending"), json!("running"), json!("completed"), json!("failed"), json!("retrying")]);
}

/// A stopped run reports its executing step `failed` with the `Canceled` tag and leaves the run-level error unset.
// spec: run.project.stopped-step@cb7a4167
#[test]
fn a_stopped_run_fails_its_executing_step_as_canceled() {
    let mut s = fresh();
    reduce(&mut s, &delta(1, status(RunStatus::Running))).unwrap();
    reduce(&mut s, &delta(2, Change::Step(StepPatch::new("pull-0").status(StepStatus::Completed)))).unwrap();
    reduce(&mut s, &delta(3, Change::Step(StepPatch::new("pull-1").status(StepStatus::Running)))).unwrap();
    let stop = Change::Status { status: RunStatus::Canceled, at: None, error: Some(Failure::canceled("stop")) };
    reduce(&mut s, &delta(4, stop)).unwrap();
    assert_eq!(s.status, RunStatus::Canceled);
    assert_eq!(s.error, None, "the run-level error stays unset");
    let executing = s.step("pull-1").unwrap();
    assert_eq!(executing.status, StepStatus::Failed);
    assert_eq!(executing.failure.as_ref().unwrap().tag, FailureTag::Canceled);
    let finished = s.step("pull-0").unwrap();
    assert_eq!(finished.status, StepStatus::Completed);
    assert_eq!(finished.failure, None);
}

/// A metadata write whose serialized size exceeds 16384 B raises `RunMetadataTooLarge` naming size and bound, and the snapshot stands unchanged.
// spec: run.project.metadata-too-large@e238a7ab
#[test]
fn a_metadata_write_past_the_bound_is_refused_and_leaves_the_snapshot() {
    assert_eq!(METADATA_MAX_BYTES, 16384);
    let mut s = fresh();
    reduce(&mut s, &delta(1, Change::Metadata { entries: BTreeMap::from([("note".into(), json!("kept"))]) })).unwrap();
    // `{"blob":"<n x>","note":"kept"}` serializes to n + 25 bytes: exactly at the bound is admitted.
    let fits = "x".repeat(METADATA_MAX_BYTES - 25);
    let mut probe = s.clone();
    reduce(&mut probe, &delta(2, Change::Metadata { entries: BTreeMap::from([("blob".into(), json!(fits))]) })).unwrap();
    assert_eq!(serde_json::to_vec(&probe.metadata).unwrap().len(), METADATA_MAX_BYTES);

    let before = s.clone();
    let over = "x".repeat(METADATA_MAX_BYTES - 24);
    let err = reduce(&mut s, &delta(2, Change::Metadata { entries: BTreeMap::from([("blob".into(), json!(over))]) })).unwrap_err();
    let RunError::RunMetadataTooLarge(msg) = &err else { panic!("{err}") };
    assert!(msg.contains("16385 B") && msg.contains("16384 B"), "{msg}");
    assert_eq!(s, before, "the snapshot stands unchanged, version included");
}

/// The snapshot version is `(epoch, counter)`, the epoch being the hub's start instant, compared lexicographically; a client discards any delta at or below the highest version it holds.
// spec: run.project.version@afa2a658
#[test]
fn versions_compare_epoch_first_and_a_delta_at_or_below_is_discarded() {
    let restarted = Version { epoch: at("2030-01-01T00:10:00Z"), counter: 1 };
    assert!(restarted > v(9_999), "a restarted hub's first version sorts above its predecessor's last");
    assert!(v(2) > v(1));

    let mut s = fresh();
    assert_eq!(reduce(&mut s, &delta(3, status(RunStatus::Running))).unwrap(), Applied::Folded);
    let held = s.clone();
    assert_eq!(reduce(&mut s, &delta(3, Change::Step(StepPatch::new("pull-0")))).unwrap(), Applied::Stale);
    assert_eq!(reduce(&mut s, &delta(2, status(RunStatus::Failed))).unwrap(), Applied::Stale);
    assert_eq!(s, held);
    let mut next = delta(0, Change::Step(StepPatch::new("pull-0")));
    next.version = restarted;
    assert_eq!(reduce(&mut s, &next).unwrap(), Applied::Folded);
    assert_eq!(s.version, restarted);
}

/// The reducer is total: nothing overwrites a terminal step or run status, a new step label appends, an existing one merges field-wise, and a stale delta is a no-op.
// spec: run.project.reducer-is-total@55a6b8df
#[test]
fn the_reducer_appends_merges_and_never_overwrites_a_terminal_status() {
    let mut s = fresh();
    reduce(&mut s, &delta(1, status(RunStatus::Running))).unwrap();
    let mut first = StepPatch::new("pull-0").status(StepStatus::Running);
    first.attempts = Some(1);
    first.started_at = Some(at("2030-01-01T00:00:01Z"));
    reduce(&mut s, &delta(2, Change::Step(first))).unwrap();
    reduce(&mut s, &delta(3, Change::Step(StepPatch::new("pull-1")))).unwrap();
    assert_eq!(s.steps.iter().map(|x| x.label.as_str()).collect::<Vec<_>>(), ["pull-0", "pull-1"]);

    // Field-wise: a patch naming only attempts keeps status and start instant.
    let mut attempts = StepPatch::new("pull-0");
    attempts.attempts = Some(2);
    reduce(&mut s, &delta(4, Change::Step(attempts))).unwrap();
    let step = s.step("pull-0").unwrap();
    assert_eq!((step.status, step.attempts, step.started_at), (StepStatus::Running, 2, Some(at("2030-01-01T00:00:01Z"))));

    // A terminal step stays terminal.
    reduce(&mut s, &delta(5, Change::Step(StepPatch::new("pull-0").status(StepStatus::Completed)))).unwrap();
    reduce(&mut s, &delta(6, Change::Step(StepPatch::new("pull-0").status(StepStatus::Running)))).unwrap();
    assert_eq!(s.step("pull-0").unwrap().status, StepStatus::Completed);

    // A terminal run stays terminal, and the step order holds.
    reduce(&mut s, &delta(7, status(RunStatus::Success))).unwrap();
    reduce(&mut s, &delta(8, status(RunStatus::Running))).unwrap();
    reduce(&mut s, &delta(9, status(RunStatus::Failed))).unwrap();
    assert_eq!(s.status, RunStatus::Success);

    // A stale delta moves nothing.
    let held = s.clone();
    assert_eq!(reduce(&mut s, &delta(1, Change::Step(StepPatch::new("pull-9")))).unwrap(), Applied::Stale);
    assert_eq!(s, held);
}

/// A step output appears as a post-redaction reference and a byte count, never inline; fetching the payload is a separately authorized request.
// spec: run.project.outputs-by-reference@e04878aa
#[test]
fn a_step_output_is_a_reference_and_a_byte_count() {
    let out = OutputRef { reference: "journal:9f".into(), bytes: 2_000_000 };
    let wire = serde_json::to_value(&out).unwrap();
    assert_eq!(wire.as_object().unwrap().keys().collect::<Vec<_>>(), ["bytes", "reference"]);
    // An output carrying its payload does not decode into the projection.
    let inline = json!({"reference": "journal:9f", "bytes": 3, "payload": "abc"});
    assert!(serde_json::from_value::<OutputRef>(inline).is_err());
}

/// Snapshot broadcasts coalesce to at most one per 100 ms, keeping only the latest; a terminal transition flushes immediately. The clock is injected.
// spec: run.project.coalescing@0e147155
#[test]
fn broadcasts_coalesce_per_window_and_a_terminal_flushes_at_once() {
    assert_eq!(COALESCE_WINDOW_MS, 100);
    let ms = |n: i128| Instant::from_unix_nanos(at(EPOCH).unix_nanos() + n * 1_000_000).unwrap();
    let snap = |counter: u64, st: RunStatus| {
        let mut s = fresh();
        s.version = v(counter);
        s.status = st;
        s
    };
    let mut c = Coalescer::default();
    assert_eq!(c.offer(snap(1, RunStatus::Running), ms(0)).map(|s| s.version), Some(v(1)));
    assert!(c.offer(snap(2, RunStatus::Running), ms(10)).is_none());
    assert!(c.offer(snap(3, RunStatus::Running), ms(50)).is_none());
    assert!(c.poll(ms(99)).is_none(), "the window is still open");
    assert_eq!(c.poll(ms(100)).map(|s| s.version), Some(v(3)), "only the latest is kept");
    assert!(c.poll(ms(300)).is_none(), "nothing held");

    assert!(c.offer(snap(4, RunStatus::Running), ms(110)).is_none());
    let flushed = c.offer(snap(5, RunStatus::Success), ms(120)).expect("a terminal flushes immediately");
    assert_eq!(flushed.version, v(5));
    assert!(c.poll(ms(500)).is_none(), "the held snapshot was superseded by the terminal one");
}
