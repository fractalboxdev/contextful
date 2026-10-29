//! `store.lay-out.run-digest`: a re-landing of a run whose manifest a collection removed
//! answers from the folded-runs row its key names, by the digest of the landing.

use crate::support::{at, decl, Fixture};
use contextful_context::fold::{collect, fold};
use contextful_context::land::{commit_parts, land_run, stage_part, Batch, Landing, Position, RunContext};
use contextful_context::ContextError;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::{NodeId, RunManifest};
use contextful_core::store::reserve::Injection;
use contextful_core::store::StoreError;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const TABLE: &str = "events";

fn table() -> TableDecl {
    decl("name = \"events\"\nretain_runs = \"1h\"")
}

fn store_err(e: ContextError) -> StoreError {
    e.store().cloned().unwrap_or_else(|| panic!("expected a store refusal, got {e}"))
}

fn batch(rows: Value) -> Batch {
    Batch { rows: rows.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect(), types: Default::default() }
}

fn injection(run: &str) -> Injection {
    Injection { run_id: run.into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, authored_agent: None, taint: None }
}

fn ctx(injection: Injection, now: &str) -> RunContext {
    RunContext { node: NodeId::parse("ingest-a").unwrap(), injection, committed_at: at(now) }
}

fn relanded(f: &Fixture, d: &TableDecl, run: &str, rows: Value, now: &str) -> contextful_context::Result<Landing> {
    land_run(&f.store, d, &[batch(rows)], &ctx(injection(run), now), &Position::default(), &|| Ok(()))
}

fn run_dir(f: &Fixture, run: &str) -> PathBuf {
    f.table_dir(TABLE).join("data/runs").join(run).join("ingest-a")
}

/// Every file under `dir`, relative and sorted, with its bytes.
fn tree(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push((p.strip_prefix(dir).unwrap().to_string_lossy().into_owned(), std::fs::read(&p).unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// Land `run`, fold it, and collect two hours later, so only the snapshot's folded-runs row names it.
fn folded_and_collected(f: &Fixture, d: &TableDecl, run: &str, rows: Value) -> RunManifest {
    let m = f.land(d, run, rows, "2030-01-01T00:00:00Z").unwrap();
    fold(&f.store, d, at("2030-01-01T00:10:00Z")).unwrap();
    collect(&f.store, d, at("2030-01-01T02:10:00Z")).unwrap();
    assert!(!run_dir(f, run).join("_manifest.json").exists(), "the collection removes the run's manifest");
    m
}

/// A run's manifest carries `batch_digest` over its producer rows, column types and position: the injected
/// columns, `_ingested_at` among them, and the commit instant leave it unchanged.
// spec: store.lay-out.run-digest@5e55dff7
#[test]
fn the_injected_columns_leave_the_digest_unchanged() {
    let f = Fixture::new();
    let d = table();
    let rows = json!([{"id": "a", "n": 1}, {"id": "b", "n": 2}]);
    let first = f.land(&d, "run-1", rows.clone(), "2030-01-01T00:00:00Z").unwrap();
    let other = Injection { site_id: "site-b".into(), batch_seq: Some(7), authored_by: Some("dana".into()), authored_agent: Some("agent-1".into()), ..injection("run-2") };
    let second = land_run(&f.store, &d, &[batch(rows.clone())], &ctx(other, "2030-01-02T05:00:00Z"), &Position::default(), &|| Ok(())).unwrap().into_committed().unwrap();
    assert!(first.batch_digest.as_ref().is_some_and(|d| d.len() == 64), "{:?}", first.batch_digest);
    assert_eq!(first.batch_digest, second.batch_digest);
    assert_ne!(first.committed_at, second.committed_at);

    let changed = f.land(&d, "run-3", json!([{"id": "a", "n": 1}, {"id": "b", "n": 3}]), "2030-01-01T00:00:00Z").unwrap();
    assert_ne!(changed.batch_digest, first.batch_digest);
    let moved = Position { pipeline_id: Some("feed".into()), cursor: Some(json!("p1")), ..Position::default() };
    let positioned = land_run(&f.store, &d, &[batch(rows)], &ctx(injection("run-4"), "2030-01-01T00:00:00Z"), &moved, &|| Ok(())).unwrap().into_committed().unwrap();
    assert_ne!(positioned.batch_digest, first.batch_digest);
}

/// A re-landing of a run whose manifest a collection removed, with an equal digest, writes nothing and replays,
/// carrying the folded-runs row in place of the manifest; it does so from inline rows and from a segment.
// spec: store.lay-out.run-folded-replay@9265556b
#[test]
fn a_retry_after_its_run_folded_and_collected_replays() {
    let f = Fixture::new();
    let d = table();
    let rows = json!([{"id": "a", "n": 1}, {"id": "b", "n": 2}]);
    let first = folded_and_collected(&f, &d, "run-1", rows.clone());
    let before = tree(&f.table_dir(TABLE));

    // A retry after a lost acknowledgement carries a later clock and the same rows.
    let again = relanded(&f, &d, "run-1", rows, "2030-01-01T03:00:00Z").unwrap();
    assert!(again.replay && again.manifest.is_none(), "{again:?}");
    let row = again.folded.unwrap();
    assert_eq!(row.run_key, "run-1/ingest-a");
    assert_eq!(row.batch_digest, first.batch_digest);
    assert_eq!(tree(&f.table_dir(TABLE)), before, "a folded replay writes nothing");

    // Past the inline bound the rows sit in a segment, and a retry replays from it.
    let g = Fixture::new();
    for i in 0..70 {
        g.land(&d, &format!("bulk-{i:02}"), json!([{"id": i}]), "2030-01-01T00:00:00Z").unwrap();
    }
    fold(&g.store, &d, at("2030-01-01T00:10:00Z")).unwrap();
    collect(&g.store, &d, at("2030-01-01T02:10:00Z")).unwrap();
    let (chain, _) = g.store.chain(TABLE).unwrap();
    assert!(chain[0].folded_runs.as_ref().is_some_and(|ix| !ix.segments.is_empty()), "the rows flushed into a segment");
    let again = land_run(&g.store, &d, &[batch(json!([{"id": 3}]))], &ctx(injection("bulk-03"), "2030-01-01T04:00:00Z"), &Position::default(), &|| Ok(())).unwrap();
    assert!(again.replay && again.folded.is_some_and(|r| r.run_key == "bulk-03/ingest-a"));
}

/// A landing under a folded run key with other rows, or under a row recording no digest, raises
/// `StoreRunConflict` naming the key and changes nothing; a staged commit under it conflicts too.
// spec: store.lay-out.run-conflict@d8b7388b
#[test]
fn a_reused_folded_run_key_conflicts() {
    let f = Fixture::new();
    let d = table();
    folded_and_collected(&f, &d, "run-1", json!([{"id": "a"}]));
    let before = tree(&f.table_dir(TABLE));
    for rows in [json!([{"id": "b"}]), json!([{"id": "a"}, {"id": "a"}]), json!([{"id": "a", "extra": 1}])] {
        let e = store_err(relanded(&f, &d, "run-1", rows, "2030-01-01T03:00:00Z").unwrap_err());
        assert!(matches!(&e, StoreError::StoreRunConflict(m) if m.contains("run-1/ingest-a")), "{e}");
        assert_eq!(tree(&f.table_dir(TABLE)), before);
    }

    // A staged commit records no digest, so it never replays a folded run.
    let staged = stage_part(&f.store, &d, &batch(json!([{"id": "a"}])), &NodeId::parse("ingest-a").unwrap(), &injection("run-1"), 0, 0).unwrap();
    let e = commit_parts(&f.store, &d, &[staged.name], &ctx(injection("run-1"), "2030-01-01T03:00:00Z"), &Position::default(), &|| Ok(()), &|_| Ok(())).unwrap_err();
    assert!(matches!(store_err(e), StoreError::StoreRunConflict(m) if m.contains("run-1/ingest-a")));
    assert!(!run_dir(&f, "run-1").join("_manifest.json").exists());

    // A row folded before digests existed answers no retry: the conflict says so.
    let (chain, _) = f.store.chain(TABLE).unwrap();
    let path = f.store.snapshot_dir(TABLE, &chain[0].snapshot_id).unwrap().join("_manifest.json");
    let mut doc: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let inline = doc["folded_runs"]["inline"].as_array_mut().unwrap();
    assert!(!inline.is_empty());
    for row in inline {
        row.as_object_mut().unwrap().remove("digest");
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
    let e = store_err(relanded(&f, &d, "run-1", json!([{"id": "a"}]), "2030-01-01T03:00:00Z").unwrap_err());
    assert!(matches!(&e, StoreError::StoreRunConflict(m) if m.contains("run-1/ingest-a") && m.contains("predates digests")), "{e}");
}

/// `Store::committed_rows` answers a run whose manifest a collection removed from its folded-runs rows, summing
/// their row counts.
// spec: store.lay-out.folded-committed@0b067027
#[test]
fn a_collected_run_reads_committed_with_its_folded_row_count() {
    let f = Fixture::new();
    let d = table();
    folded_and_collected(&f, &d, "run-1", json!([{"id": "a"}, {"id": "b"}, {"id": "c"}]));
    assert_eq!(f.store.committed_rows(TABLE, "run-1").unwrap(), Some(3));
    assert_eq!(f.store.committed_rows(TABLE, "run-2").unwrap(), None);
}
