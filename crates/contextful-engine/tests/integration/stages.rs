//! The stages a run reports to its wired sink: each pull, landing, commit and journaled step.

use crate::support::{plan, three_pages, Pages, Rig, Sink};
use contextful_core::run::{Failure, FailureTag};
use contextful_engine::stages::{StageKind, StageReport, StageSink};
use contextful_engine::Engine;
use serde_json::json;
use std::sync::{Arc, Mutex};

/// A sink keeping every report in arrival order.
#[derive(Default)]
struct Kept(Mutex<Vec<StageReport>>);

impl StageSink for Kept {
    fn stage(&self, report: StageReport) {
        self.0.lock().unwrap().push(report);
    }
}

impl Kept {
    fn of(&self, kind: StageKind) -> Vec<StageReport> {
        self.0.lock().unwrap().iter().filter(|r| r.kind == kind).cloned().collect()
    }
}

fn traced(rig: &Rig, kept: &Arc<Kept>) -> Engine {
    Engine { stages: Some(kept.clone() as Arc<dyn StageSink>), ..rig.engine.clone() }
}

fn opaque() -> contextful_core::run::plan::Plan {
    plan("kind = \"opaque-token\"", "")
}

fn spec(run: &str) -> contextful_engine::RunSpec {
    let plan = opaque();
    contextful_engine::RunSpec { connector: plan.connector_pin("artifact-1"), plan, run_id: run.into(), site_id: "site-a".into(), pid: 4242, boot_id: "boot-a".into(), trace_id: None }
}

/// A run reports one pull and one journaled step per page, one landing per staged batch with its row and byte
/// counts, and one commit with the rows it landed; every report names the run's table and none a row value.
// spec: disclosure.record.stage-span@eb9dfee1
// spec: disclosure.record.stage-attributes@32230807
// spec: disclosure.record.stage-content@2b811f08
#[test]
fn a_run_reports_each_pull_landing_commit_and_step() {
    let rig = Rig::new();
    let kept = Arc::new(Kept::default());
    let mut source = Pages::new(three_pages());
    let mut dest = Sink { part_bytes: 64, ..Sink::default() };
    let row = traced(&rig, &kept).run(&spec("run-a"), &mut source, &mut dest).unwrap();
    assert_eq!(row.rows, 4);

    let pulls = kept.of(StageKind::Pull);
    let steps = kept.of(StageKind::Step);
    assert!(!pulls.is_empty() && !steps.is_empty(), "the run reports its pulls and steps");
    let labels: Vec<_> = pulls.iter().map(|r| r.step.clone().unwrap_or_default()).collect();
    assert_eq!(labels, ["pull-0", "pull-1", "pull-2"]);
    assert_eq!(steps.iter().map(|r| r.step.clone().unwrap_or_default()).collect::<Vec<_>>(), labels);
    assert!(pulls.iter().all(|r| r.outcome == "ok" && r.bytes.is_some_and(|b| b > 0) && r.error.is_none()), "{pulls:?}");
    assert!(steps.iter().all(|r| r.outcome == "recorded" && r.bytes.is_some_and(|b| b > 0)), "{steps:?}");

    let lands = kept.of(StageKind::Land);
    assert_eq!(lands.iter().map(|r| (r.rows, r.bytes)).collect::<Vec<_>>(), [(Some(2), Some(64)), (Some(1), Some(64)), (Some(1), Some(64))]);
    let commits = kept.of(StageKind::Commit);
    assert_eq!(commits.iter().map(|r| (r.outcome, r.rows, r.bytes)).collect::<Vec<_>>(), [("ok", Some(4), Some(192))]);

    let all = kept.0.lock().unwrap().clone();
    assert!(!all.is_empty());
    assert!(all.iter().all(|r| r.table.as_deref() == Some("filings")), "{all:?}");
    let shown = format!("{all:?}");
    for value in ["d1", "d2", "d3", "d4"] {
        assert!(!shown.contains(&format!("\"{value}\"")), "a report discloses row value `{value}`: {shown}");
    }
}

/// A failed pull reports `failed` with its error identifier; a resumed run's step the journal held reports
/// `replayed` and makes no pull.
// spec: disclosure.record.stage-outcome@af77ac43
#[test]
fn a_failed_pull_and_a_replayed_step_report_their_outcomes() {
    let rig = Rig::new();
    let kept = Arc::new(Kept::default());
    let mut source = Pages::new(vec![vec![json!({"id": "d1"})]]);
    source.fail = vec![Failure::deterministic(FailureTag::Permanent, "the vendor refused the request")];
    let failed = traced(&rig, &kept).run(&spec("run-f"), &mut source, &mut Sink::default()).unwrap();
    assert_eq!(failed.status, contextful_core::run::record::RunStatus::Failed);
    let pulls = kept.of(StageKind::Pull);
    assert_eq!(pulls.iter().map(|r| (r.outcome, r.error.as_deref())).collect::<Vec<_>>(), [("failed", Some("Permanent"))]);
    let steps = kept.of(StageKind::Step);
    assert_eq!(steps.iter().map(|r| (r.outcome, r.error.is_some())).collect::<Vec<_>>(), [("failed", true)]);
    assert!(kept.of(StageKind::Commit).is_empty(), "a failed run commits nothing");

    let rig = Rig::new();
    let mut source = Pages::new(three_pages());
    source.die_after = Some(1);
    rig.crash(&opaque(), "run-a", &mut source, &mut Sink::default());
    rig.clock.advance(60);
    let kept = Arc::new(Kept::default());
    let mut dest = Sink::default();
    traced(&rig, &kept).run(&spec("run-b"), &mut source, &mut dest).unwrap();
    let steps = kept.of(StageKind::Step);
    assert!(!steps.is_empty(), "the resumed run reports its steps");
    assert_eq!(steps[0].step.as_deref(), Some("pull-0"));
    assert_eq!(steps[0].outcome, "replayed");
    let pulled: Vec<_> = kept.of(StageKind::Pull).iter().map(|r| r.step.clone().unwrap_or_default()).collect();
    assert!(!pulled.is_empty());
    assert!(!pulled.iter().any(|label| label == "pull-0"), "a replayed step makes no pull: {pulled:?}");
}

/// An engine wired to no sink reports nothing and runs as one wired to a sink does.
// spec: disclosure.record.stage-unconfigured@682fe4bd
#[test]
fn an_engine_without_a_sink_reports_no_stage() {
    let rig = Rig::new();
    assert!(rig.engine.stages.is_none(), "the default engine wires no stage sink");
    let mut dest = Sink::default();
    let row = rig.engine.run(&spec("run-u"), &mut Pages::new(three_pages()), &mut dest).unwrap();
    assert_eq!(row.rows, 4);
}
