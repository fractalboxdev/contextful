//! A run's commit of several batches: a part per batch, ordinals, and the position on the marker.

use crate::support::{at, decl, s, Fixture};
use contextful_context::land::{land_batches, Batch, Position, RunContext};
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reserve::Injection;
use serde_json::json;

fn batch(rows: serde_json::Value) -> Batch {
    Batch { rows: rows.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect(), types: Default::default() }
}

#[test]
fn a_run_lands_each_batch_as_a_part_and_carries_its_position() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "run-b".into(), site_id: "site-a".into(), batch_seq: None, authored_by: None },
        committed_at: at("2030-01-01T00:01:00Z"),
    };
    let position = Position { pipeline_id: Some("feed".into()), cursor: Some(json!("p3")), fence: Some(4) };
    let batches = [batch(json!([{"id": "d1"}, {"id": "d2"}])), batch(json!([])), batch(json!([{"id": "d3"}]))];
    let m = land_batches(&f.store, &d, &batches, &ctx, &position, &|| Ok(())).unwrap();
    assert_eq!(m.fence, Some(4));
    assert_eq!(m.parts.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["part-00000.parquet", "part-00001.parquet"]);
    assert_eq!((m.pipeline_id.as_deref(), m.cursor.clone()), (Some("feed"), Some(json!("p3"))));
    let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(f.table_dir("filings").join("data/runs/run-b/ingest-a/_manifest.json")).unwrap()).unwrap();
    assert_eq!(raw["cursor"], "p3");
    let rows = f.query(&d, Bounds::default(), "SELECT id, _batch_seq, _row_seq FROM t ORDER BY id");
    // The empty batch keeps its ordinal: the third batch reads 2.
    assert_eq!(rows, [vec![s("d1"), s("0"), s("0")], vec![s("d2"), s("0"), s("1")], vec![s("d3"), s("2"), s("2")]]);
}

#[test]
fn a_refused_precommit_leaves_the_run_uncommitted() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "run-f".into(), site_id: "site-a".into(), batch_seq: None, authored_by: None },
        committed_at: at("2030-01-01T00:01:00Z"),
    };
    let position = Position { pipeline_id: Some("feed".into()), cursor: Some(json!("p3")), fence: Some(1) };
    let refused = land_batches(&f.store, &d, &[batch(json!([{"id": "d1"}]))], &ctx, &position, &|| {
        Err(contextful_context::ContextError::Invalid("LeaseFenced: a later holder took the lease".into()))
    });
    assert!(refused.unwrap_err().to_string().contains("LeaseFenced"));
    assert!(!f.table_dir("filings").join("data/runs/run-f/ingest-a/_manifest.json").exists());
    assert!(f.store.committed_runs("filings").unwrap().is_empty());
}
