//! `run.land.session` and `store.fold.session-trigger`: one long-lived writer landing
//! micro-batch runs, and the pass its unfolded-run count fires.

use crate::support::{at, decl, Fixture};
use contextful_context::land::{Batch, LandSession, Position};
use contextful_context::error::ContextError;
use contextful_core::store::fold::{scheduled, FoldOutcome};
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
    let first = session.land(&single_row(0), injection("run-0"), tick(0), &Position::default(), &|| Ok(())).unwrap();
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
        let landing = session.land(&single_row(i), injection(&format!("run-{i}")), tick(i), &Position::default(), &|| Ok(())).unwrap();
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
    let again = session.land(&single_row(0), injection("run-0"), tick(9), &Position::default(), &|| Ok(())).unwrap();
    assert!(again.replay);
    assert_eq!(session.unfolded(), 4);
}

/// A single writer's per-commit count matches the table's unfolded runs across passes.
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
        let landing = session.land(&single_row(i), injection(&format!("run-{i}")), tick(i), &Position::default(), &|| Ok(())).unwrap();
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
    assert!(session.land(&single_row(0), injection("run-0"), tick(0), &Position::default(), &|| Ok(())).unwrap().fold.is_none());
    assert!(session.land(&single_row(1), injection("run-1"), tick(6 * 3600 - 1), &Position::default(), &|| Ok(())).unwrap().fold.is_none());
    let late = session.land(&single_row(2), injection("run-2"), tick(6 * 3600), &Position::default(), &|| Ok(())).unwrap();
    assert!(matches!(late.fold, Some(FoldOutcome::Folded { runs: 3, .. })), "{:?}", late.fold);
    assert_eq!(session.unfolded(), 0);
    assert!(session.land(&single_row(3), injection("run-3"), tick(6 * 3600 + 1), &Position::default(), &|| Ok(())).unwrap().fold.is_none());
    assert_eq!(session.unfolded(), 1);
}

/// A landing session counts its table's unfolded runs at open and per commit, recounting when another writer commits or folds, and runs a pass before the land returns once the table's trigger fires.
// spec: store.fold.session-trigger@605f05d6
#[test]
fn a_session_counts_runs_another_writer_commits_or_folds() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\nfold_max_unfolded_runs = 5");
    let other = |run: &str, secs: u64| {
        f.land_on(&d, run, "ingest-b", json!([{ "id": 1000 + secs as i64 }]), &tick(secs as usize).to_string(), &[]).unwrap();
    };
    let mut session = LandSession::open(&f.store, d.clone(), NodeId::parse("ingest-a").unwrap()).unwrap();
    let land = |session: &mut LandSession, i: usize| {
        session.land(&single_row(i), injection(&format!("run-{i}")), tick(i * 10), &Position::default(), &|| Ok(())).unwrap()
    };
    assert!(land(&mut session, 0).fold.is_none());
    other("other-0", 5);
    assert!(land(&mut session, 1).fold.is_none());
    assert_eq!(session.unfolded(), 3, "the other writer's run counts");
    other("other-1", 15);
    other("other-2", 16);
    // Five unfolded runs sit in the table; the session's next land finds six and folds.
    let fired = land(&mut session, 2);
    assert!(matches!(fired.fold, Some(FoldOutcome::Folded { runs: 6, .. })), "{:?}", fired.fold);
    assert_eq!(session.unfolded(), 0);

    assert!(land(&mut session, 3).fold.is_none());
    other("other-3", 35);
    // Another process folds both runs; the session's count drops with it.
    contextful_context::fold::fold(&f.store, &d, tick(36)).unwrap();
    assert!(land(&mut session, 4).fold.is_none());
    assert_eq!(session.unfolded(), 1);
    assert_eq!(f.store.state(&d).unwrap().unfolded_runs().len(), 1);
}

/// A session land whose precommit check refuses commits nothing and leaves the count.
#[test]
fn a_session_land_runs_its_precommit_check() {
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    let mut session = LandSession::open(&f.store, d.clone(), NodeId::parse("ingest-a").unwrap()).unwrap();
    let refuse = || Err(ContextError::Invalid("authored rows cross the boundary".into()));
    assert!(session.land(&single_row(0), injection("run-0"), tick(0), &Position::default(), &refuse).is_err());
    assert_eq!(session.unfolded(), 0);
    assert!(f.store.state(&d).unwrap().runs.is_empty());
}

/// A table declaring `fold_max_unfolded_runs`, a positive integer, fires its pass at that many unfolded runs in place of the 50-run count; zero refuses the declaration at parse.
// spec: store.fold.declared-trigger@9f908b42
#[test]
fn a_scheduled_pass_fires_at_the_declared_run_count() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\nfold_max_unfolded_runs = 4");
    let now = tick(60);
    for i in 0..3 {
        f.land(&d, &format!("run-{i}"), json!([{ "id": i }]), &tick(i).to_string()).unwrap();
    }
    assert!(!scheduled(&f.store.state(&d).unwrap(), &d, now), "three of four runs");
    f.land(&d, "run-3", json!([{ "id": 3 }]), &tick(3).to_string()).unwrap();
    assert!(scheduled(&f.store.state(&d).unwrap(), &d, now), "four runs reach the declared count");
    for invalid in ["0", "-1"] {
        let text = format!("[[pipeline.tables]]\nname = \"events\"\nfold_max_unfolded_runs = {invalid}\n");
        assert!(contextful_core::store::declare::TableDecl::parse_pipeline(&text).is_err(), "{invalid}");
    }
}

/// The file version a reader compares: length, modification instant and file identity.
fn schema_version(f: &Fixture, table: &str) -> (u64, std::time::SystemTime, u64) {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(f.table_dir(table).join("schema.json")).unwrap();
    (meta.len(), meta.modified().unwrap(), meta.ino())
}

/// A landing session parses `schema.json` again only when its version — length, modification instant, file identity — moves; a land adding no column or type replaces no `schema.json` and takes no schema lock.
// spec: run.land.session-schema@732545f1
#[test]
fn a_session_reads_schema_json_once_while_its_version_holds() {
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    f.land(&d, "pre-0", json!([{ "id": -1 }]), "2029-12-31T23:59:00Z").unwrap();
    let held = schema_version(&f, "events");
    let mut session = LandSession::open(&f.store, d.clone(), NodeId::parse("ingest-a").unwrap()).unwrap();
    for i in 0..300 {
        session.land(&single_row(i), injection(&format!("run-{i}")), tick(i), &Position::default(), &|| Ok(())).unwrap();
    }
    assert_eq!(session.schema_reads(), 1, "300 runs and six passes under one schema version");
    assert_eq!(schema_version(&f, "events"), held, "no land replaced schema.json");

    // Another writer adds a column; the session's next land reads the new version once.
    f.land_on(&d, "other-0", "ingest-b", json!([{ "id": 5000, "note": "x" }]), "2030-01-02T00:00:00Z", &[]).unwrap();
    assert_ne!(schema_version(&f, "events"), held);
    for i in 300..310 {
        session.land(&single_row(i), injection(&format!("run-{i}")), tick(i), &Position::default(), &|| Ok(())).unwrap();
    }
    assert_eq!(session.schema_reads(), 2);
    let columns: Vec<String> = f.store.try_schema("events").unwrap().unwrap().columns.into_iter().map(|c| c.name).collect();
    assert!(columns.contains(&"note".to_string()), "{columns:?}");
}

/// A one-shot land whose batch carries no new column leaves `schema.json` untouched.
#[test]
fn a_land_merging_no_new_column_replaces_no_schema_file() {
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    f.land(&d, "run-0", json!([{ "id": 0 }]), "2030-01-01T00:00:00Z").unwrap();
    let held = schema_version(&f, "events");
    f.land(&d, "run-1", json!([{ "id": 1 }]), "2030-01-01T00:01:00Z").unwrap();
    assert_eq!(schema_version(&f, "events"), held);
    f.land(&d, "run-2", json!([{ "id": 2, "note": "x" }]), "2030-01-01T00:02:00Z").unwrap();
    assert_ne!(schema_version(&f, "events"), held, "a new column replaces schema.json");
}
