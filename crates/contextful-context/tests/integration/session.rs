//! `run.land.session` and `store.fold.session-trigger`: one long-lived writer landing
//! micro-batch runs, and the pass its unfolded-run count fires.

use crate::support::{at, decl, Fixture};
use contextful_context::land::{Batch, LandSession};
use contextful_core::store::fold::FoldOutcome;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reserve::Injection;
use contextful_core::time::Instant;
use serde_json::json;
use std::collections::HashMap;

fn single_row(i: usize) -> Batch {
    Batch { rows: vec![json!({ "id": i as i64 }).as_object().unwrap().clone()], types: HashMap::new() }
}

fn injection(run: &str) -> Injection {
    Injection { run_id: run.into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint: None }
}

fn tick(i: usize) -> Instant {
    at("2030-01-01T00:00:00Z").plus_secs(i as u64)
}

/// A landing session holds one store, node and table declaration across runs and commits each micro-batch as its own run; a later run rewrites no earlier run's parts or manifest.
// spec: run.land.session@799b983f
#[test]
fn a_session_commits_each_micro_batch_as_its_own_run() {
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    let mut session = LandSession::open(&f.store, d.clone(), NodeId::parse("ingest-a").unwrap()).unwrap();
    let first = session.land(&single_row(0), injection("run-0"), tick(0)).unwrap();
    assert!(!first.replay);
    let run0 = f.table_dir("events").join("data/runs/run-0/ingest-a");
    let held: Vec<(std::path::PathBuf, Vec<u8>)> = std::fs::read_dir(&run0)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "parquet" || x == "json"))
        .map(|p| (p.clone(), std::fs::read(&p).unwrap()))
        .collect();
    assert!(held.iter().any(|(p, _)| p.ends_with("_manifest.json")), "{held:?}");
    assert!(held.iter().any(|(p, _)| p.extension().is_some_and(|x| x == "parquet")), "{held:?}");

    for i in 1..4 {
        let landing = session.land(&single_row(i), injection(&format!("run-{i}")), tick(i)).unwrap();
        assert_eq!(landing.manifest.run_id, format!("run-{i}"));
        assert_eq!(landing.manifest.node_id.as_str(), "ingest-a");
        assert!(landing.fold.is_none(), "four runs fire no 50-run trigger");
    }
    for (path, bytes) in &held {
        assert_eq!(&std::fs::read(path).unwrap(), bytes, "{} was rewritten", path.display());
    }
    let state = f.store.state(&d).unwrap();
    assert_eq!(state.runs.len(), 4);
    assert_eq!(session.unfolded(), 4);

    // Landing a committed run again replays it and adds nothing to the count.
    let again = session.land(&single_row(0), injection("run-0"), tick(9)).unwrap();
    assert!(again.replay);
    assert_eq!(session.unfolded(), 4);
}

/// A landing session counts its table's unfolded runs once at open and then per commit, and runs a pass before the land returns once the table's trigger fires.
// spec: store.fold.session-trigger@93c06d26
#[test]
fn a_session_folds_when_its_unfolded_count_reaches_the_declared_trigger() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\nfold_max_unfolded_runs = 5");
    for i in 0..3 {
        f.land(&d, &format!("pre-{i}"), json!([{ "id": -(i as i64) }]), "2029-12-31T23:59:00Z").unwrap();
    }
    let mut session = LandSession::open(&f.store, d.clone(), NodeId::parse("ingest-a").unwrap()).unwrap();
    assert_eq!(session.unfolded(), 3, "the count opens from the runs already unfolded");

    let mut passes = 0;
    for i in 0..23 {
        let landing = session.land(&single_row(i), injection(&format!("run-{i}")), tick(i)).unwrap();
        if let Some(outcome) = &landing.fold {
            assert!(matches!(outcome, FoldOutcome::Folded { .. }), "{outcome:?}");
            passes += 1;
        }
        let unfolded = f.store.state(&d).unwrap().unfolded_runs().len();
        assert!(unfolded < 5, "run {i} left {unfolded} unfolded runs");
        assert_eq!(session.unfolded(), unfolded, "run {i}");
    }
    // 26 runs at a 5-run trigger: passes after the 5th, 10th, 15th, 20th and 25th.
    assert_eq!(passes, 5);
    assert_eq!(session.unfolded(), 1);
}

/// Six hours after the table's previous pass, the next land runs a pass whatever the count.
#[test]
fn a_session_folds_six_hours_after_the_previous_pass() {
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    let mut session = LandSession::open(&f.store, d.clone(), NodeId::parse("ingest-a").unwrap()).unwrap();
    assert!(session.land(&single_row(0), injection("run-0"), tick(0)).unwrap().fold.is_none());
    assert!(session.land(&single_row(1), injection("run-1"), tick(6 * 3600 - 1)).unwrap().fold.is_none());
    let late = session.land(&single_row(2), injection("run-2"), tick(6 * 3600)).unwrap();
    assert!(matches!(late.fold, Some(FoldOutcome::Folded { runs: 3, .. })), "{:?}", late.fold);
    assert_eq!(session.unfolded(), 0);
    assert!(session.land(&single_row(3), injection("run-3"), tick(6 * 3600 + 1)).unwrap().fold.is_none());
    assert_eq!(session.unfolded(), 1);
}
