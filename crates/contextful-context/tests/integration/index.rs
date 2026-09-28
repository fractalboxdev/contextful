//! `store.index`: clustering and partitioning of a snapshot's parts, and the vector sidecar
//! the fold builds beside them.

use crate::support::{at, decl, query, s, Fixture};
use contextful_context::fold::{escape, fold};
use contextful_core::store::bound_time::Bounds;
use serde_json::json;
use contextful_context::vector::{Sealing, VectorSidecar, GRAPH_FILE};
use contextful_core::store::index::VectorEntry;
use contextful_core::store::lay_out::SnapshotManifest;
use contextful_core::store::reconcile::{ColumnType, FloatItem};
use contextful_core::store::StoreError;
use serde_json::Value;
use std::path::PathBuf;

/// `cluster_by` sorts rows within a file lexicographically over its columns in declared order; zone maps then skip row groups with no manifest entry and no sidecar.
// spec: store.index.clustering@43fe730c
#[test]
fn cluster_by_sorts_rows_within_a_file_in_declared_order() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"\ncluster_by = [\"issuer\", \"year\"]");
    f.land(&d, "run-1", json!([
        {"issuer": "b", "year": 2021}, {"issuer": "a", "year": 2023}, {"issuer": "b", "year": 2020}, {"issuer": "a", "year": 2022}
    ]), "2030-01-01T00:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let path = f.store.root().join(&f.scan(&d, Bounds::default()).unwrap().files[0]);
    let rows = query(&format!("SELECT issuer, year FROM read_parquet('{}')", path.display()));
    assert_eq!(rows, [[s("a"), s("2022")], [s("a"), s("2023")], [s("b"), s("2020")], [s("b"), s("2021")]]);
    // Zone maps: each row group's footer carries the min and max a reader skips on.
    let stats = query(&format!(
        "SELECT stats_min_value, stats_max_value FROM parquet_metadata('{}') WHERE path_in_schema = 'issuer'",
        path.display()
    ));
    assert_eq!(stats, [[s("a"), s("b")]]);
}

/// Partitioning is off unless `partition_by` declares it.
// spec: store.index.partitioning@20bc2134
#[test]
fn partitioning_is_off_unless_declared() {
    let f = Fixture::new();
    let plain = decl("name = \"events\"");
    f.land(&plain, "run-1", json!([{"tenant": "acme", "e": 1}, {"tenant": "globex", "e": 2}]), "2030-01-01T00:00:00Z").unwrap();
    fold(&f.store, &plain, at("2030-01-01T01:00:00Z")).unwrap();
    let files = f.scan(&plain, Bounds::default()).unwrap().files;
    assert_eq!(files.len(), 1);
    assert!(files[0].ends_with("/part-00000.parquet") && !files[0].contains("tenant="), "{files:?}");

    let g = Fixture::new();
    let parted = decl("name = \"events\"\npartition_by = [\"tenant\"]");
    g.land(&parted, "run-1", json!([{"tenant": "acme", "e": 1}, {"tenant": "globex", "e": 2}, {"tenant": null, "e": 3}]), "2030-01-01T00:00:00Z").unwrap();
    fold(&g.store, &parted, at("2030-01-01T01:00:00Z")).unwrap();
    let files = g.scan(&parted, Bounds::default()).unwrap().files;
    let dirs: Vec<&str> = files.iter().map(|p| p.rsplit('/').nth(1).unwrap()).collect();
    assert_eq!(dirs, ["tenant=__HIVE_DEFAULT_PARTITION__", "tenant=acme", "tenant=globex"]);
    assert_eq!(g.query(&parted, Bounds::default(), "SELECT tenant, e FROM t ORDER BY e"), [[s("acme"), s("1")], [s("globex"), s("2")], [None, s("3")]]);
}

/// A `partition_by` column typed binary or vector raises `StorePartitionColumnType` at validation, before any Parquet.
// spec: store.index.partition-type@1e6a72e5
#[test]
fn a_binary_or_vector_partition_column_is_refused() {
    use contextful_core::store::reconcile::{ColumnType, FloatItem};
    use contextful_core::store::StoreError;
    for (column, ty, value) in [
        ("digest", ColumnType::FixedSizeBinary(2), json!("/wA=")),
        ("blob", ColumnType::Binary, json!("/gE=")),
        ("embedding", ColumnType::FixedSizeList(FloatItem::Float32, 2), json!([0.5, 1.0])),
    ] {
        let f = Fixture::new();
        let d = decl(&format!("name = \"events\"\npartition_by = [\"{column}\"]"));
        let err = f.land_typed(&d, "run-1", json!([{column: value, "e": 1}]), "2030-01-01T00:00:00Z", &[(column, ty)]).unwrap_err();
        assert!(matches!(err.store(), Some(StoreError::StorePartitionColumnType(_))), "{column}: {err}");
        assert!(!f.table_dir("events").join("data/runs/run-1").exists(), "{column}");

        // A partition declared after the rows landed refuses the fold instead of collapsing values.
        let g = Fixture::new();
        let plain = decl("name = \"events\"");
        g.land_typed(&plain, "run-1", json!([{column: value, "e": 1}]), "2030-01-01T00:00:00Z", &[(column, ty)]).unwrap();
        let err = fold(&g.store, &d, at("2030-01-01T01:00:00Z")).unwrap_err();
        assert!(matches!(err.store(), Some(StoreError::StorePartitionColumnType(_))), "{column}: {err}");
    }
}

/// A tenant value is written and compared byte for byte, with no trimming, case folding or Unicode normalization; a percent-escaped directory name is representation alone.
// spec: store.index.tenant-verbatim@4f788f5f
#[test]
fn a_tenant_value_is_kept_byte_for_byte() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\npartition_by = [\"tenant\"]");
    // Composed and decomposed "é", a padded value and a case variant are four tenants.
    let tenants = ["caf\u{e9}", "cafe\u{301}", " acme", "ACME", "a/b"];
    let rows: Vec<_> = tenants.iter().enumerate().map(|(i, t)| json!({"tenant": t, "e": i})).collect();
    f.land(&d, "run-1", json!(rows), "2030-01-01T00:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    assert_eq!(f.scan(&d, Bounds::default()).unwrap().files.len(), 5);
    let read = f.query(&d, Bounds::default(), "SELECT tenant FROM t ORDER BY e");
    assert_eq!(read, tenants.iter().map(|t| vec![s(t)]).collect::<Vec<_>>());
    assert_eq!(escape("a/b"), "a%2Fb");
    assert_eq!(escape(" acme"), "%20acme");
    assert_eq!(escape(".."), "%2E%2E");
}

/// `indexes/` joins no table's file set; a snapshot reader lists only the parts its manifest names.
// spec: store.index.not-in-file-set@dfccb5dd
#[test]
fn a_sidecar_directory_joins_no_file_set() {
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    f.land(&d, "run-1", json!([{"e": 1}]), "2030-01-01T00:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let files = f.scan(&d, Bounds::default()).unwrap().files;
    let snapshot = f.store.root().join(&files[0]).parent().unwrap().to_path_buf();
    let sidecar = snapshot.join("indexes/fts-e");
    std::fs::create_dir_all(&sidecar).unwrap();
    std::fs::copy(snapshot.join("part-00000.parquet"), sidecar.join("part-00000.parquet")).unwrap();
    assert_eq!(f.scan(&d, Bounds::default()).unwrap().files, files);
    assert_eq!(f.query(&d, Bounds::default(), "SELECT count(*) FROM t"), [[s("1")]]);
}


pub const INDEX: &str = "[[pipeline.tables.indexes]]\nkind = \"vector\"\ncolumn = \"embedding\"\nmodel = \"e5\"\ndim = 3\nm = 4\nef_construction = 32\n";

pub fn f32x3() -> [(&'static str, ColumnType); 1] {
    [("embedding", ColumnType::FixedSizeList(FloatItem::Float32, 3))]
}

/// The current snapshot's manifest and directory.
pub fn current(f: &Fixture, table: &str) -> (SnapshotManifest, PathBuf) {
    let (chain, _) = f.store.chain(table).unwrap();
    let m = chain.into_iter().next().expect("a published snapshot");
    let dir = f.store.snapshot_dir(table, &m.snapshot_id).unwrap();
    (m, dir)
}

fn entry(m: &SnapshotManifest) -> VectorEntry {
    serde_json::from_value(m.indexes[0].clone()).unwrap()
}

/// The fold builds each declared vector sidecar over the staged rows holding a non-null identifier and a non-zero vector, from the declared model where the table carries `embedding_model`, and records its entry in the snapshot manifest.
// spec: store.index.vector-by-fold@6ab64801
#[test]
fn the_fold_builds_each_declared_sidecar_over_identified_nonzero_vectors_of_its_model() {
    let f = Fixture::new();
    let d = decl(&format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\n{INDEX}"));
    f.land_typed(&d, "run-1", json!([
        {"passage_id": "p1", "embedding": [1.0, 0.0, 0.0], "embedding_model": "e5"},
        {"passage_id": "p2", "embedding": [0.0, 1.0, 0.0], "embedding_model": "e5"},
        {"passage_id": "p3", "embedding": [0.0, 0.0, 0.0], "embedding_model": "e5"},
        {"passage_id": "p4", "embedding": null, "embedding_model": "e5"},
        {"passage_id": "p5", "embedding": [0.0, 0.0, 1.0], "embedding_model": "other"},
    ]), "2030-01-01T00:00:00Z", &f32x3()).unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let (m, dir) = current(&f, "passages");
    let e = entry(&m);
    assert_eq!((e.path.as_str(), e.id_column.as_str(), e.row_count), ("indexes/vec-embedding-e5/zone=all", "passage_id", 2));
    assert_eq!((e.table.as_str(), e.snapshot_id.as_str(), e.dim, e.m, e.ef_construction), ("passages", m.snapshot_id.to_string().as_str(), 3, 4, 32));
    assert!(dir.join(&e.path).join(GRAPH_FILE).is_file());
    let own: Value = serde_json::from_slice(&std::fs::read(dir.join(&e.path).join("_manifest.json")).unwrap()).unwrap();
    assert_eq!(own, m.indexes[0]);
    let sidecar = VectorSidecar::open(&dir, "passages", &m.indexes[0], &Sealing::Plaintext).unwrap();
    let near: Vec<String> = sidecar.probe(&[0.1, 0.9, 0.0], 5).unwrap().into_iter().map(|c| c.id).collect();
    assert_eq!(near, ["p2", "p1"]);

    // An unkeyed append table takes a sidecar over its declared identifier, and rows
    // without one stay out of the graph.
    let g = Fixture::new();
    let u = decl(&format!("name = \"passages\"\n{INDEX}id_column = \"digest\"\n"));
    g.land_typed(&u, "run-1", json!([
        {"digest": "d1", "embedding": [1.0, 0.0, 0.0]},
        {"digest": null, "embedding": [0.0, 1.0, 0.0]},
        {"digest": "d3", "embedding": [0.0, 0.0, 2.0]},
    ]), "2030-01-01T00:00:00Z", &f32x3()).unwrap();
    fold(&g.store, &u, at("2030-01-01T01:00:00Z")).unwrap();
    let (m, dir) = current(&g, "passages");
    assert_eq!((entry(&m).id_column.as_str(), entry(&m).row_count), ("digest", 2));
    let sidecar = VectorSidecar::open(&dir, "passages", &m.indexes[0], &Sealing::Plaintext).unwrap();
    let top = sidecar.probe(&[0.0, 0.0, 1.0], 1).unwrap();
    assert_eq!(top[0].id, "d3");
    assert!((top[0].similarity - 1.0).abs() < 1e-6, "{top:?}");
    // A table declaring no sidecar records none.
    let plain = decl("name = \"events\"");
    g.land(&plain, "run-1", json!([{"e": 1}]), "2030-01-01T00:00:00Z").unwrap();
    fold(&g.store, &plain, at("2030-01-01T01:00:00Z")).unwrap();
    assert!(current(&g, "events").0.indexes.is_empty());
}

/// A vector sidecar is an HNSW graph over unit-length `Float32` vectors whose layers draw from a seed of the snapshot id and column, so one staged row set builds one byte-identical graph.
// spec: store.index.graph@ab0cf2ff
#[test]
fn one_staged_row_set_builds_one_byte_identical_graph() {
    let rows: Vec<Value> = (0..300)
        .map(|i| {
            let t = i as f32 * 0.37;
            json!({"passage_id": format!("p{i:03}"), "embedding": [t.sin() * 3.0, t.cos() * 3.0, (i % 7) as f32]})
        })
        .collect();
    let graph = |at_: &str| {
        let f = Fixture::new();
        let d = decl(&format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\n{INDEX}"));
        f.land_typed(&d, "run-1", Value::Array(rows.clone()), "2030-01-01T00:00:00Z", &f32x3()).unwrap();
        fold(&f.store, &d, at(at_)).unwrap();
        let (m, dir) = current(&f, "passages");
        // Every row is reachable: probed with its own vector, it is among the nearest ten.
        let sidecar = VectorSidecar::open(&dir, "passages", &m.indexes[0], &Sealing::Plaintext).unwrap();
        for row in &rows {
            let v: Vec<f32> = row["embedding"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
            let near = sidecar.probe(&v, 10).unwrap();
            assert!(near.iter().any(|c| c.id == row["passage_id"]), "{} unreachable: {near:?}", row["passage_id"]);
        }
        (m.snapshot_id.to_string(), std::fs::read(dir.join(entry(&m).path).join(GRAPH_FILE)).unwrap())
    };
    let (id_a, a) = graph("2030-01-01T01:00:00Z");
    let (id_b, b) = graph("2030-01-01T01:00:00Z");
    assert_eq!(id_a, id_b);
    assert!(a == b, "one staged row set and snapshot id built two graphs");
    let (id_c, c) = graph("2030-01-02T01:00:00Z");
    assert_ne!(id_a, id_c);
    assert!(a != c, "another snapshot id draws other layers");
    // Every stored vector is unit length: 300 rows of 3 floats follow the 32-byte header.
    for row in a[32..32 + 300 * 12].chunks_exact(12) {
        let v: Vec<f32> = row.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "{v:?}");
    }
}

/// A fold meeting one `id_column` value on two rows of the snapshot it stages raises `StoreIndexIdNotUnique`, naming the table, the column and the value, and publishes nothing.
// spec: store.index.id-unique@bb65805a
#[test]
fn a_repeated_identifier_refuses_the_pass() {
    let f = Fixture::new();
    let d = decl(&format!("name = \"passages\"\n{INDEX}id_column = \"digest\"\n"));
    f.land_typed(&d, "run-1", json!([{"digest": "d1", "embedding": [1.0, 0.0, 0.0]}]), "2030-01-01T00:00:00Z", &f32x3()).unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let before = f.store.pointer("passages").unwrap().unwrap().0;
    // The same digest lands again in an append table: the union repeats it.
    f.land_typed(&d, "run-2", json!([{"digest": "d1", "embedding": [0.0, 1.0, 0.0]}]), "2030-01-02T00:00:00Z", &f32x3()).unwrap();
    let err = fold(&f.store, &d, at("2030-01-02T01:00:00Z")).unwrap_err();
    match err.store() {
        Some(StoreError::StoreIndexIdNotUnique(m)) => assert!(m.contains("passages") && m.contains("digest") && m.contains("d1"), "{m}"),
        other => panic!("expected StoreIndexIdNotUnique, got {other:?}"),
    }
    assert_eq!(f.store.pointer("passages").unwrap().unwrap().0, before, "the pass published");
    // The identifier the table keys on stays unique through the fold's dedupe.
    let k = decl(&format!("name = \"keyed\"\nprimary_key = [\"passage_id\"]\n{INDEX}"));
    for (run, now) in [("run-1", "2030-01-01T00:00:00Z"), ("run-2", "2030-01-02T00:00:00Z")] {
        f.land_typed(&k, run, json!([{"passage_id": "p1", "embedding": [1.0, 0.0, 0.0]}]), now, &f32x3()).unwrap();
    }
    fold(&f.store, &k, at("2030-01-02T01:00:00Z")).unwrap();
    assert_eq!(entry(&current(&f, "keyed").0).row_count, 1);
}

/// Sidecar recall@10 against exact search over seeded vectors (ledger `vector-recall`).
#[test]
fn vector_recall_at_10_holds_against_exact_search() {
    const N: usize = 2000;
    const DIM: usize = 32;
    const QUERIES: usize = 200;
    const SEED: u64 = 0x5eed_0043;
    let mut state = SEED;
    let mut draw = move || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((state >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0
    };
    let vectors: Vec<Vec<f32>> = (0..N).map(|_| (0..DIM).map(|_| draw()).collect()).collect();
    let rows: Vec<Value> = vectors.iter().enumerate().map(|(i, v)| json!({"passage_id": format!("p{i}"), "embedding": v})).collect();
    let f = Fixture::new();
    let d = decl(&format!(
        "name = \"passages\"\nprimary_key = [\"passage_id\"]\n[[pipeline.tables.indexes]]\nkind = \"vector\"\ncolumn = \"embedding\"\nmodel = \"e5\"\ndim = {DIM}\n"
    ));
    f.land_typed(&d, "run-1", Value::Array(rows), "2030-01-01T00:00:00Z", &[("embedding", ColumnType::FixedSizeList(FloatItem::Float32, DIM as u32))]).unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let (m, dir) = current(&f, "passages");
    let sidecar = VectorSidecar::open(&dir, "passages", &m.indexes[0], &Sealing::Plaintext).unwrap();
    let cosine = |a: &[f32], b: &[f32]| {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        dot / (a.iter().map(|x| x * x).sum::<f32>().sqrt() * b.iter().map(|x| x * x).sum::<f32>().sqrt())
    };
    let mut hits = 0usize;
    for _ in 0..QUERIES {
        let q: Vec<f32> = (0..DIM).map(|_| draw()).collect();
        let mut exact: Vec<(f32, usize)> = vectors.iter().enumerate().map(|(i, v)| (cosine(&q, v), i)).collect();
        exact.sort_by(|a, b| b.0.total_cmp(&a.0));
        let truth: std::collections::HashSet<String> = exact[..10].iter().map(|(_, i)| format!("p{i}")).collect();
        hits += sidecar.probe(&q, 10).unwrap().iter().filter(|c| truth.contains(&c.id)).count();
    }
    let recall = hits as f64 / (QUERIES * 10) as f64;
    contextful_eval::record::emit("vector-recall", recall, QUERIES as u64, SEED);
    assert!(recall >= 0.95, "recall@10 {recall}");
}
