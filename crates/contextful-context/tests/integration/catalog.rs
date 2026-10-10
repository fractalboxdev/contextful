//! `store.lay-out.derived-catalog`: the derived catalog's rows read off the canonical tree
//! and written through the `DerivedCatalog` port, with no SQLite linked.

use crate::support::{at, decl, Fixture};
use contextful_context::catalog::{derive, rebuild};
use contextful_context::fold::fold;
use contextful_core::run::Failure;
use contextful_core::store::catalog::{DerivedCatalog, DerivedRows, DerivedTable};
use serde_json::json;
use std::path::Path;
use std::sync::Mutex;

/// A host's catalog held in memory.
#[derive(Default)]
struct Rows(Mutex<DerivedRows>);

impl DerivedCatalog for Rows {
    fn replace(&self, rows: &DerivedRows) -> Result<(), Failure> {
        *self.0.lock().unwrap() = rows.clone();
        Ok(())
    }

    fn rows(&self) -> Result<DerivedRows, Failure> {
        Ok(self.0.lock().unwrap().clone())
    }
}

/// Every file under `dir` with its bytes.
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

#[test]
fn a_rebuild_reads_tables_snapshots_and_runs_off_the_tree() {
    let f = Fixture::new();
    let events = decl("name = \"events\"");
    let calls = decl("name = \"calls\"");
    f.land(&events, "run-1", json!([{"e": 1}, {"e": 2}]), "2030-01-01T00:00:00Z").unwrap();
    fold(&f.store, &events, at("2030-01-01T01:00:00Z")).unwrap();
    f.land(&events, "run-2", json!([{"e": 3}]), "2030-01-01T02:00:00Z").unwrap();
    fold(&f.store, &events, at("2030-01-01T03:00:00Z")).unwrap();
    f.land_on(&calls, "run-1", "ingest-b", json!([{"c": "x"}]), "2030-01-01T00:30:00Z", &[]).unwrap();

    let before = tree(f.store.root());
    let catalog = Rows(Mutex::new(DerivedRows {
        tables: vec![DerivedTable { table: "dropped".into(), schema: "{}".into(), snapshot_id: None, fence: None }],
        ..DerivedRows::default()
    }));
    let rows = rebuild(&f.store, &catalog).unwrap();
    assert_eq!(catalog.rows().unwrap(), rows, "the rebuild replaces every row the catalog held");
    assert_eq!(tree(f.store.root()), before, "a rebuild commits nothing to the tree");

    let tables: Vec<(&str, Option<&str>)> = rows.tables.iter().map(|t| (t.table.as_str(), t.snapshot_id.as_deref())).collect();
    let (chain, _) = f.store.chain("events").unwrap();
    let (newest, oldest) = (chain[0].snapshot_id.to_string(), chain[1].snapshot_id.to_string());
    assert_eq!(tables, [("calls", None), ("events", Some(newest.as_str()))]);
    assert!(rows.tables[1].schema.contains("\"e\""), "{}", rows.tables[1].schema);

    let snapshots: Vec<(&str, Option<&str>, u64)> = rows.snapshots.iter().map(|s| (s.snapshot_id.as_str(), s.parent.as_deref(), s.row_count)).collect();
    assert_eq!(snapshots, [(oldest.as_str(), None, 2), (newest.as_str(), Some(oldest.as_str()), 3)]);

    let runs: Vec<(&str, &str, &str)> = rows.runs.iter().map(|r| (r.table.as_str(), r.run_id.as_str(), r.node_id.as_str())).collect();
    assert_eq!(runs, [("calls", "run-1", "ingest-b"), ("events", "run-1", "ingest-a"), ("events", "run-2", "ingest-a")]);
    assert_eq!(rows.runs[0].committed_at, at("2030-01-01T00:30:00Z"));

    // One tree derives one row set, however often it is read.
    assert_eq!(derive(&f.store).unwrap(), rows);
}

#[test]
fn an_empty_store_derives_no_rows() {
    let f = Fixture::new();
    let catalog = Rows::default();
    assert_eq!(rebuild(&f.store, &catalog).unwrap(), DerivedRows::default());
}

/// A sidecar's identity is its one path under {{store.index.paths}}; `derived.sqlite` records the builder and builder version of each sidecar a reachable snapshot holds.
// spec: store.index.identity@a1937925
#[test]
fn the_derived_catalog_records_each_sidecars_builder_under_its_one_path() {
    use contextful_core::store::index::{FULLTEXT_BUILDER, FULLTEXT_BUILDER_VERSION, VECTOR_BUILDER, VECTOR_BUILDER_VERSION};
    use contextful_core::store::reconcile::{ColumnType, FloatItem};
    let f = Fixture::new();
    let d = decl(&format!(
        "name = \"passages\"\nprimary_key = [\"passage_id\"]\n{}[[pipeline.tables.indexes]]\nkind = \"fulltext\"\ncolumn = \"body\"\n",
        crate::index::INDEX
    ));
    let vectors = [("embedding", ColumnType::FixedSizeList(FloatItem::Float32, 3))];
    f.land_typed(&d, "run-1", json!([{"passage_id": "p1", "body": "solar", "embedding": [1.0, 0.0, 0.0]}]), "2030-01-01T00:00:00Z", &vectors).unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    f.land_typed(&d, "run-2", json!([{"passage_id": "p2", "body": "wind", "embedding": [0.0, 1.0, 0.0]}]), "2030-01-01T02:00:00Z", &vectors).unwrap();
    fold(&f.store, &d, at("2030-01-01T03:00:00Z")).unwrap();

    let catalog = Rows::default();
    let rows = rebuild(&f.store, &catalog).unwrap();
    let (chain, _) = f.store.chain("passages").unwrap();
    let mut expected = Vec::new();
    for s in chain.iter().rev() {
        expected.push((s.snapshot_id.to_string(), "indexes/fts-body-unicode".to_string(), FULLTEXT_BUILDER.to_string(), FULLTEXT_BUILDER_VERSION));
        expected.push((s.snapshot_id.to_string(), "indexes/vec-embedding-e5/zone=all".to_string(), VECTOR_BUILDER.to_string(), VECTOR_BUILDER_VERSION));
    }
    let recorded: Vec<(String, String, String, u32)> =
        rows.sidecars.iter().map(|s| (s.snapshot_id.clone(), s.path.clone(), s.builder.clone(), s.builder_version)).collect();
    assert_eq!(recorded, expected);
    assert!(!rows.sidecars.is_empty());
    assert!(rows.sidecars.iter().all(|s| s.table == "passages"));
    assert_eq!(catalog.rows().unwrap().sidecars, rows.sidecars);
    // One path per column and model: no builder segment, so a rebuild under another builder
    // replaces the sidecar at the path it already held.
    assert!(rows.sidecars.iter().all(|s| !s.path.contains(&s.builder)));
}
