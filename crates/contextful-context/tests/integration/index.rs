//! `store.index`: clustering and partitioning of a snapshot's parts, and the vector and
//! full-text sidecars the fold builds beside them.

use crate::support::{at, decl, s, Fixture};
#[cfg(feature = "read")]
use crate::support::query;
use contextful_context::fold::{escape, fold};
use contextful_core::store::bound_time::Bounds;
use serde_json::json;
use contextful_context::vector::{Sealing, VectorSidecar, GRAPH_FILE};
use contextful_context::fulltext::{FulltextSidecar, POSTINGS_FILE};
use contextful_core::store::index::{FulltextEntry, IndexEntry, IndexKind, Tokenizer, VectorEntry};
use contextful_core::store::lay_out::SnapshotManifest;
use contextful_core::store::reconcile::{ColumnType, FloatItem};
use contextful_core::store::StoreError;
use serde_json::Value;
use std::path::PathBuf;

#[cfg(feature = "read")]
/// `cluster_by` sorts file rows lexicographically by its columns in order, so zone maps skip row groups with no
/// manifest entry or sidecar.
// spec: store.index.clustering@b67f03fd
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

#[cfg(feature = "read")]
/// A table uses partitioning only when the table declares `partition_by`.
// spec: store.index.partitioning@709a10cd
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

#[cfg(feature = "read")]
/// Five index kinds exist: footer zone maps, sorted-Parquet sparse-map primary-key lookups, HNSW vector graphs,
/// positional full-text indexes and opt-in per-column bloom filters.
// spec: store.index.kinds@61af7dd6
#[test]
fn a_snapshot_carries_each_of_the_five_index_kinds() {
    let f = Fixture::new();
    let fulltext = "[[pipeline.tables.indexes]]\nkind = \"fulltext\"\ncolumn = \"body\"\n";
    let d = decl(&format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\nbloom_filter = [\"issuer\"]\n{INDEX}{fulltext}"));
    // Landed out of key order.
    f.land_typed(&d, "run-1", json!([
        {"passage_id": "p3", "issuer": "z", "body": "grid storage", "embedding": [0.0, 0.0, 1.0]},
        {"passage_id": "p1", "issuer": "x", "body": "solar battery", "embedding": [1.0, 0.0, 0.0]},
        {"passage_id": "p2", "issuer": "y", "body": "wind turbine", "embedding": [0.0, 1.0, 0.0]},
    ]), "2030-01-01T00:00:00Z", &f32x3()).unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let (m, dir) = current(&f, "passages");
    let part = dir.join(&m.parts[0].name);
    let meta = |column: &str, select: &str| query(&format!("SELECT {select} FROM parquet_metadata('{}') WHERE path_in_schema = '{column}'", part.display()));
    // Zone maps: every row group's footer carries the column's bounds.
    assert_eq!(meta("issuer", "stats_min_value, stats_max_value"), [[s("x"), s("z")]]);
    // A keyed table's rows sort by key, so the footer bounds of its key column form a sparse map a lookup probes.
    let keys = query(&format!("SELECT passage_id FROM read_parquet('{}')", part.display()));
    assert_eq!(keys, [[s("p1")], [s("p2")], [s("p3")]]);
    assert_eq!(meta("passage_id", "stats_min_value, stats_max_value"), [[s("p1"), s("p3")]]);
    // Bloom filters: on the column the table opts in, and on no other.
    assert_ne!(meta("issuer", "bloom_filter_offset"), [[None]]);
    assert_eq!(meta("body", "bloom_filter_offset"), [[None]]);
    // The graph and the positional full-text index sit beside the parts.
    let kinds: Vec<Option<IndexKind>> = m.indexes.iter().map(|e| e.kind()).collect();
    assert_eq!(kinds, [Some(IndexKind::Vector), Some(IndexKind::Fulltext)]);
    assert!(dir.join(m.indexes[0].path().unwrap()).join(GRAPH_FILE).is_file());
    assert!(dir.join(m.indexes[1].path().unwrap()).join(POSTINGS_FILE).is_file());
}

/// A column a table names in `bloom_filter` carries a bloom filter in every part a run or a
/// fold writes, sized for that part's rows; an unnamed column carries none.
// spec: store.index.bloom-parts@c0f4ed9b
#[test]
fn every_part_a_run_or_fold_writes_carries_the_declared_bloom_filter() {
    use parquet::file::reader::{FileReader, SerializedFileReader};
    let f = Fixture::new();
    let d = decl("name = \"filings\"\nprimary_key = [\"doc_id\"]\nbloom_filter = [\"doc_id\"]");
    let filters = |f: &Fixture| -> Vec<(Option<i32>, Option<i32>)> {
        f.scan(&d, Bounds::default()).unwrap().files.iter().flat_map(|file| {
            let reader = SerializedFileReader::new(std::fs::File::open(f.store.root().join(file)).unwrap()).unwrap();
            let meta = reader.metadata().clone();
            let column = |name: &str| meta.file_metadata().schema_descr().columns().iter().position(|c| c.name() == name).unwrap();
            let (key, title) = (column("doc_id"), column("title"));
            meta.row_groups().iter().map(|g| (g.column(key).bloom_filter_length(), g.column(title).bloom_filter_offset().map(|_| 0))).collect::<Vec<_>>()
        }).collect()
    };
    f.land(&d, "run-1", json!([{"doc_id": "a1", "title": "x"}, {"doc_id": "b2", "title": "y"}]), "2030-01-01T00:00:00Z").unwrap();
    f.land(&d, "run-2", json!([{"doc_id": "c3", "title": "z"}]), "2030-01-02T00:00:00Z").unwrap();
    let landed = filters(&f);
    assert_eq!(landed.len(), 2, "one row group per run part");
    for (key, title) in &landed {
        assert!(key.is_some_and(|bytes| bytes <= 1024), "a run part's key filter is sized for its rows: {key:?}");
        assert_eq!(*title, None, "an unnamed column carries no filter");
    }
    fold(&f.store, &d, at("2030-01-03T00:00:00Z")).unwrap();
    let folded = filters(&f);
    assert_eq!(folded.len(), 1, "one snapshot row group");
    assert!(folded[0].0.is_some_and(|bytes| bytes <= 1024), "the snapshot's key filter is sized for its rows: {:?}", folded[0].0);
}

/// A vintage table naming no `bloom_filter` carries a bloom filter on its key in every part
/// a run or a fold writes; its other columns carry none.
// spec: store.index.bloom-vintage-key@d2e33b7a
#[test]
fn a_vintage_table_carries_a_key_bloom_filter_it_never_declared() {
    use parquet::file::reader::{FileReader, SerializedFileReader};
    let f = Fixture::new();
    let d = decl("name = \"filings\"\nprimary_key = [\"doc_id\"]\ncontent_hash_column = \"digest\"\n[pipeline.tables.available_time]\ncolumn = \"filed_at\"");
    let filters = |f: &Fixture| -> Vec<(bool, bool)> {
        f.scan(&d, Bounds::default()).unwrap().files.iter().flat_map(|file| {
            let reader = SerializedFileReader::new(std::fs::File::open(f.store.root().join(file)).unwrap()).unwrap();
            let meta = reader.metadata().clone();
            let column = |name: &str| meta.file_metadata().schema_descr().columns().iter().position(|c| c.name() == name).unwrap();
            let (key, digest) = (column("doc_id"), column("digest"));
            meta.row_groups().iter().map(|g| (g.column(key).bloom_filter_offset().is_some(), g.column(digest).bloom_filter_offset().is_some())).collect::<Vec<_>>()
        }).collect()
    };
    f.land(&d, "run-1", json!([{"doc_id": "a1", "digest": "x", "filed_at": "2030-01-01T00:00:00Z"}]), "2030-01-01T01:00:00Z").unwrap();
    f.land(&d, "run-2", json!([{"doc_id": "b2", "digest": "y", "filed_at": "2030-01-02T00:00:00Z"}]), "2030-01-02T01:00:00Z").unwrap();
    assert_eq!(filters(&f), [(true, false), (true, false)], "each run part filters its key alone");
    fold(&f.store, &d, at("2030-01-03T00:00:00Z")).unwrap();
    assert_eq!(filters(&f), [(true, false)], "the snapshot filters its key alone");
}

/// A multi-tenant table carries the tenant identifier as its outermost partition column.
// spec: store.index.tenant-outermost@13d7e0b2
#[test]
fn the_tenant_identifier_is_the_outermost_partition_directory() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\npartition_by = [\"tenant\", \"day\"]");
    f.land(&d, "run-1", json!([
        {"tenant": "acme", "day": "d1", "e": 1}, {"tenant": "acme", "day": "d2", "e": 2}, {"tenant": "globex", "day": "d1", "e": 3},
    ]), "2030-01-01T00:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let (m, _) = current(&f, "events");
    let parts: Vec<&str> = m.parts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(parts, ["tenant=acme/day=d1/part-00000.parquet", "tenant=acme/day=d2/part-00000.parquet", "tenant=globex/day=d1/part-00000.parquet"]);
    // One tenant's rows are one directory subtree: dropping the inner predicate still reads that tenant alone.
    let acme: Vec<&str> = parts.iter().copied().filter(|p| p.starts_with("tenant=acme/")).collect();
    assert_eq!(acme.len(), 2);
    assert!(parts.iter().all(|p| p.split('/').next().unwrap().starts_with("tenant=")));
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
        let err = f.land_typed(&d, "run-1", json!([{column: value, "e": 1}]), "2030-01-01T00:00:00Z", &[(column, ty.clone())]).unwrap_err();
        assert!(matches!(err.store(), Some(StoreError::StorePartitionColumnType(_))), "{column}: {err}");
        assert!(!f.table_dir("events").join("data/runs/run-1").exists(), "{column}");

        // A partition declared after the rows landed refuses the fold instead of collapsing values.
        let g = Fixture::new();
        let plain = decl("name = \"events\"");
        g.land_typed(&plain, "run-1", json!([{column: value, "e": 1}]), "2030-01-01T00:00:00Z", &[(column, ty.clone())]).unwrap();
        let err = fold(&g.store, &d, at("2030-01-01T01:00:00Z")).unwrap_err();
        assert!(matches!(err.store(), Some(StoreError::StorePartitionColumnType(_))), "{column}: {err}");
    }
}

#[cfg(feature = "read")]
/// Landing applies no canonical form to tenant values: writes and comparisons keep bytes without trimming, case
/// folding or Unicode normalization, and percent-escaping changes only the directory name.
// spec: store.index.tenant-verbatim@72c280dd
#[test]
fn a_tenant_value_is_kept_byte_for_byte() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\npartition_by = [\"tenant\"]");
    // Composed and decomposed "é", a padded value and a case variant are four tenants.
    let tenants = ["caf\u{e9}", "cafe\u{301}", " acme", "ACME", "a/b"];
    let rows: Vec<_> = tenants.iter().enumerate().map(|(i, t)| json!({"tenant": t, "e": i})).collect();
    f.land(&d, "run-1", json!(rows), "2030-01-01T00:00:00Z").unwrap();
    // Land refuses none and rewrites none.
    let landed = f.query(&d, Bounds::default(), "SELECT tenant FROM t ORDER BY e");
    assert_eq!(landed, tenants.iter().map(|t| vec![s(t)]).collect::<Vec<_>>());
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    assert_eq!(f.scan(&d, Bounds::default()).unwrap().files.len(), 5);
    let read = f.query(&d, Bounds::default(), "SELECT tenant FROM t ORDER BY e");
    assert_eq!(read, tenants.iter().map(|t| vec![s(t)]).collect::<Vec<_>>());
    assert_eq!(escape("a/b"), "a%2Fb");
    assert_eq!(escape(" acme"), "%20acme");
    assert_eq!(escape(".."), "%2E%2E");
}

#[cfg(feature = "read")]
/// Every read passes `read_parquet` an explicit sorted file list of the parts the pointer's manifests name, with no glob, so stray files and
/// `indexes/` stay out.
// spec: store.reconcile.explicit-file-list@38646064
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
    match &m.indexes[0] {
        IndexEntry::Vector(e) => e.clone(),
        other => panic!("a vector entry reads as {other:?}"),
    }
}

/// Fold builds each declared vector sidecar from staged rows with non-null identifiers and non-zero vectors, of the
/// declared model where `embedding_model` exists, and records it in the snapshot manifest.
// spec: store.index.vector-by-fold@8cad3b7e
// spec: read.embed.model-identifier@9c31cf09
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
    let own: IndexEntry = serde_json::from_slice(&std::fs::read(dir.join(&e.path).join("_manifest.json")).unwrap()).unwrap();
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

/// A vector sidecar sits at `indexes/vec-<col>-<model>/zone=<label>/` and a full-text sidecar at
/// `indexes/fts-<col>-<tokenizer>/`, inside the snapshot directory it indexes.
// spec: store.index.paths@320c0117
#[test]
fn each_sidecar_sits_at_its_kind_path_inside_the_snapshot_it_indexes() {
    let f = Fixture::new();
    let fulltext = "[[pipeline.tables.indexes]]\nkind = \"fulltext\"\ncolumn = \"body\"\ntokenizer = \"unicode\"\n";
    let d = decl(&format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\n{INDEX}{fulltext}"));
    f.land_typed(&d, "run-1", json!([{"passage_id": "p1", "body": "solar battery", "embedding": [1.0, 0.0, 0.0]}]), "2030-01-01T00:00:00Z", &f32x3()).unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let (m, dir) = current(&f, "passages");
    let mut paths: Vec<String> = m.indexes.iter().map(|e| e.path().unwrap().to_string()).collect();
    paths.sort();
    assert_eq!(paths, ["indexes/fts-body-unicode", "indexes/vec-embedding-e5/zone=all"]);
    for p in &paths {
        assert!(dir.join(p).join("_manifest.json").is_file(), "{p} sits inside {}", dir.display());
    }
    // Every sidecar file the pass wrote lies under the snapshot's own `indexes/`.
    let table = f.store.table_dir("passages").unwrap();
    let mut stray = Vec::new();
    let mut stack = vec![table.clone()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.to_string_lossy().contains("indexes") && !p.starts_with(dir.join("indexes")) {
                stray.push(p);
            }
        }
    }
    assert!(stray.is_empty(), "{stray:?}");
}

/// Rewrite field `key` of the current snapshot's first sidecar entry, in the snapshot
/// manifest and in the sidecar's own manifest alike.
fn restamp(f: &Fixture, table: &str, key: &str, value: Value) {
    let (m, dir) = current(f, table);
    let path = dir.join("_manifest.json");
    let mut doc: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    doc["indexes"][0][key] = value;
    std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    let own = dir.join(m.indexes[0].path().unwrap()).join("_manifest.json");
    std::fs::write(&own, serde_json::to_vec(&doc["indexes"][0]).unwrap()).unwrap();
}

fn passages(f: &Fixture) -> contextful_core::store::declare::TableDecl {
    let d = decl(&format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\n{INDEX}"));
    f.land_typed(&d, "run-1", json!([
        {"passage_id": "p1", "embedding": [1.0, 0.0, 0.0]}, {"passage_id": "p2", "embedding": [0.0, 1.0, 0.0]},
    ]), "2030-01-01T00:00:00Z", &f32x3()).unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    d
}

/// A pass rebuilds in full, into a new snapshot, a current sidecar whose builder or builder version differs from
/// this build's; readers keep the prior snapshot until pointer publication.
// spec: store.index.rebuild@dc63a277
#[test]
fn a_pass_rebuilds_a_sidecar_another_builder_version_wrote() {
    use contextful_core::store::fold::FoldOutcome;
    use contextful_core::store::index::{VECTOR_BUILDER, VECTOR_BUILDER_VERSION};
    let f = Fixture::new();
    let d = passages(&f);
    // A pass under this build's own builder, with nothing landed, publishes nothing.
    assert_eq!(fold(&f.store, &d, at("2030-01-01T02:00:00Z")).unwrap(), FoldOutcome::NothingLanded);

    // Builder version 2 laid out `Float32` vectors alone; this build rebuilds its sidecars.
    for (key, value) in [("builder_version", json!(VECTOR_BUILDER_VERSION + 1)), ("builder_version", json!(2)), ("builder", json!("another-hnsw"))] {
        restamp(&f, "passages", key, value);
        let (before, before_dir) = current(&f, "passages");
        // Until the pass publishes, readers resolve the snapshot the old builder wrote.
        assert_eq!(f.scan(&d, Bounds::default()).unwrap().files, before.part_files().into_iter().map(|p| format!("tables/passages/{p}")).collect::<Vec<_>>());
        let outcome = fold(&f.store, &d, at("2030-01-01T03:00:00Z")).unwrap();
        assert!(matches!(outcome, FoldOutcome::Folded { runs: 0, rows: 2, .. }), "{outcome:?}");
        let (after, after_dir) = current(&f, "passages");
        assert_eq!(after.parent.as_ref(), Some(&before.snapshot_id));
        assert_eq!((after.row_count, after.includes_runs.len()), (before.row_count, 0));
        let e = entry(&after);
        assert_eq!((e.builder.as_str(), e.builder_version, e.row_count), (VECTOR_BUILDER, VECTOR_BUILDER_VERSION, 2));
        assert!(after_dir.join(&e.path).join(GRAPH_FILE).is_file());
        assert!(before_dir.is_dir(), "the prior snapshot stays on disk for readers that resolved it");
        VectorSidecar::open(&after_dir, "passages", &after.indexes[0], &Sealing::Plaintext).unwrap();
    }
}

/// The vector graph's byte layout carries no compatibility guarantee: a reader opens a sidecar only at its own
/// builder and builder version, else {{read.retrieve.sidecar-falls-back}}.
// spec: store.index.graph-format@e47b277f
#[test]
fn a_reader_opens_only_its_own_builder_version() {
    use contextful_context::vector::Fallback;
    use contextful_core::store::index::VECTOR_BUILDER_VERSION;
    let f = Fixture::new();
    passages(&f);
    let (m, dir) = current(&f, "passages");
    VectorSidecar::open(&dir, "passages", &m.indexes[0], &Sealing::Plaintext).unwrap();
    for (key, value) in [("builder_version", json!(VECTOR_BUILDER_VERSION + 1)), ("builder_version", json!(0)), ("builder", json!("another-hnsw"))] {
        let f = Fixture::new();
        passages(&f);
        restamp(&f, "passages", key, value);
        let (m, dir) = current(&f, "passages");
        assert_eq!(VectorSidecar::open(&dir, "passages", &m.indexes[0], &Sealing::Plaintext).err(), Some(Fallback::ManifestMismatch), "{key}");
    }
}

/// A vector or full-text entry counts in `extensions` the segment-adding passes since its last full build; a pass
/// finding 16 extensions rebuilds in full and records 0.
// spec: store.index.graph-extensions@44610c99
#[test]
fn a_pass_finding_sixteen_extensions_rebuilds_the_graph_in_full() {
    use contextful_core::store::fold::FoldOutcome;
    use contextful_core::store::index::GRAPH_EXTENSIONS_MAX;
    assert_eq!(GRAPH_EXTENSIONS_MAX, 16);
    let f = Fixture::new();
    let d = passages(&f);
    assert_eq!(entry(&current(&f, "passages").0).extensions, 0, "a full build records no extension");
    restamp(&f, "passages", "extensions", json!(GRAPH_EXTENSIONS_MAX - 1));
    assert_eq!(fold(&f.store, &d, at("2030-01-01T02:00:00Z")).unwrap(), FoldOutcome::NothingLanded);
    restamp(&f, "passages", "extensions", json!(GRAPH_EXTENSIONS_MAX));
    let outcome = fold(&f.store, &d, at("2030-01-01T03:00:00Z")).unwrap();
    assert!(matches!(outcome, FoldOutcome::Folded { runs: 0, .. }), "{outcome:?}");
    let e = entry(&current(&f, "passages").0);
    assert_eq!((e.extensions, e.row_count), (0, 2));
    // A pass folding only new identifiers extends the graph by one segment.
    f.land_typed(&d, "run-2", json!([{"passage_id": "p3", "embedding": [0.0, 0.0, 1.0]}]), "2030-01-01T04:00:00Z", &f32x3()).unwrap();
    fold(&f.store, &d, at("2030-01-01T05:00:00Z")).unwrap();
    let e = entry(&current(&f, "passages").0);
    assert_eq!((e.extensions, e.row_count, e.segments.len()), (1, 3, 1));
    // A pass replacing a stored row rebuilds in full and records none.
    f.land_typed(&d, "run-3", json!([{"passage_id": "p3", "embedding": [1.0, 1.0, 0.0]}]), "2030-01-01T06:00:00Z", &f32x3()).unwrap();
    fold(&f.store, &d, at("2030-01-01T07:00:00Z")).unwrap();
    let e = entry(&current(&f, "passages").0);
    assert_eq!((e.extensions, e.row_count, e.segments.len()), (0, 3, 0));
}

/// A sidecar copies only `id_column`, the indexed column, and the tenant column, zone label and availability instant
/// filtered traversal needs; every other filter applies at {{store.index.candidate-ids}}.
// spec: store.index.filter-copies@0744981a
#[test]
fn a_sidecar_copies_no_other_column_value() {
    let f = Fixture::new();
    let fulltext = "[[pipeline.tables.indexes]]\nkind = \"fulltext\"\ncolumn = \"body\"\ntokenizer = \"unicode\"\n";
    let d = decl(&format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\npartition_by = [\"tenant\"]\n{INDEX}{fulltext}"));
    f.land_typed(&d, "run-1", json!([
        {"passage_id": "p1", "tenant": "acme", "body": "solar battery", "region": "canaryregionzq", "embedding": [1.0, 0.0, 0.0]},
        {"passage_id": "p2", "tenant": "acme", "body": "wind farm", "region": "canaryregionzq", "embedding": [0.0, 1.0, 0.0]},
    ]), "2030-01-01T00:00:00Z", &f32x3()).unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let (m, dir) = current(&f, "passages");
    assert_eq!(m.indexes.len(), 2);
    let mut files = Vec::new();
    let mut stack = vec![dir.join("indexes")];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() { stack.push(p) } else { files.push(p) }
        }
    }
    assert!(files.len() >= 4, "{files:?}");
    let has = |bytes: &[u8], needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    let all: Vec<u8> = files.iter().flat_map(|p| std::fs::read(p).unwrap()).collect();
    // The identifiers and the indexed text are the sidecars' own; no other column's value is.
    assert!(has(&all, b"p1") && has(&all, b"solar"));
    assert!(!has(&all, b"canaryregionzq"));
}

/// A vector probe under an availability bound yields the nearest nodes the bound admits,
/// walking the graph through later nodes to reach them.
#[test]
fn a_bounded_vector_probe_yields_the_nearest_nodes_available_by_the_bound() {
    use contextful_core::store::bound_time::Bound;
    let f = Fixture::new();
    let d = decl(&format!("name = \"passages\"\n{INDEX}id_column = \"passage_id\"\n[pipeline.tables.available_time]\ncolumn = \"published_at\"\n"));
    let types = [("embedding", ColumnType::FixedSizeList(FloatItem::Float32, 3)), ("published_at", ColumnType::Timestamp)];
    let mut rows = vec![
        json!({"passage_id": "late-a", "embedding": [1.0, 0.0, 0.0], "published_at": "2016-01-01T00:00:00Z"}),
        json!({"passage_id": "late-b", "embedding": [0.98, 0.02, 0.0], "published_at": "2017-01-01T00:00:00Z"}),
        json!({"passage_id": "early-a", "embedding": [0.9, 0.1, 0.0], "published_at": "2014-01-01T00:00:00Z"}),
        json!({"passage_id": "early-b", "embedding": [0.6, 0.4, 0.0], "published_at": "2013-01-01T00:00:00Z"}),
        json!({"passage_id": "edge", "embedding": [0.7, 0.3, 0.0], "published_at": "2015-01-01T00:00:00Z"}),
    ];
    // Later, farther nodes the walk passes through.
    rows.extend((0..40).map(|i| {
        let t = i as f32 / 40.0;
        json!({"passage_id": format!("later-{i:02}"), "embedding": [t, 1.0 - t, 0.5], "published_at": "2018-01-01T00:00:00Z"})
    }));
    f.land_typed(&d, "run-1", Value::Array(rows), "2030-01-01T00:00:00Z", &types).unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let (m, dir) = current(&f, "passages");
    let sidecar = VectorSidecar::open(&dir, "passages", &m.indexes[0], &Sealing::Plaintext).unwrap();
    let ids = |found: Vec<contextful_context::vector::Candidate>| found.into_iter().map(|c| c.id).collect::<Vec<_>>();
    let query = [1.0, 0.0, 0.0];
    assert_eq!(ids(sidecar.probe(&query, 2).unwrap()), ["late-a", "late-b"]);
    // An inclusive bound admits the node available at it; an exclusive one does not.
    let inclusive = Bound { at: at("2015-01-01T00:00:00Z"), inclusive: true };
    assert_eq!(ids(sidecar.probe_within(&query, 3, Some(inclusive)).unwrap()), ["early-a", "edge", "early-b"]);
    let exclusive = Bound { at: at("2015-01-01T00:00:00Z"), inclusive: false };
    assert_eq!(ids(sidecar.probe_within(&query, 3, Some(exclusive)).unwrap()), ["early-a", "early-b"]);
    // A bound before every node yields none.
    let before = Bound { at: at("2000-01-01T00:00:00Z"), inclusive: true };
    assert!(sidecar.probe_within(&query, 3, Some(before)).unwrap().is_empty());
}

/// Collecting a snapshot collects its sidecars in the same step, save a segment {{store.fold.reference-retention}} keeps.
// spec: store.index.dies-with-snapshot@53018981
#[test]
fn collecting_a_snapshot_collects_its_sidecars() {
    let f = Fixture::new();
    let d = decl(&format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\n{INDEX}"));
    f.land_typed(&d, "run-1", json!([{"passage_id": "p1", "embedding": [1.0, 0.0, 0.0]}]), "2030-01-01T00:00:00Z", &f32x3()).unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let (first, first_dir) = current(&f, "passages");
    let sidecar = first_dir.join(first.indexes[0].path().unwrap());
    assert!(sidecar.join(GRAPH_FILE).is_file());
    // Superseded, inside the window: the snapshot and its sidecar stay together.
    f.land_typed(&d, "run-2", json!([{"passage_id": "p2", "embedding": [0.0, 1.0, 0.0]}]), "2030-01-02T00:00:00Z", &f32x3()).unwrap();
    fold(&f.store, &d, at("2030-01-02T01:00:00Z")).unwrap();
    assert!(first_dir.is_dir() && sidecar.join(GRAPH_FILE).is_file());
    // Past the window, the pass that collects the snapshot keeps the graph segment the current
    // snapshot still carries.
    f.land_typed(&d, "run-3", json!([{"passage_id": "p3", "embedding": [0.0, 0.0, 1.0]}]), "2030-01-09T01:00:00Z", &f32x3()).unwrap();
    fold(&f.store, &d, at("2030-01-09T01:00:00Z")).unwrap();
    let (now, now_dir) = current(&f, "passages");
    assert!(now_dir.join(now.indexes[0].path().unwrap()).join(GRAPH_FILE).is_file());
    assert!(now.indexes[0].segments().iter().any(|s| s.snapshot_id == first.snapshot_id.to_string()));
    assert!(!first_dir.join("_manifest.json").exists() && sidecar.join(GRAPH_FILE).is_file());
    // A replaced row rebuilds the graph whole; once no retained snapshot names the segment,
    // the pass collecting the directory collects the sidecar with it.
    f.land_typed(&d, "run-4", json!([{"passage_id": "p1", "embedding": [0.0, 1.0, 1.0]}]), "2030-01-10T00:00:00Z", &f32x3()).unwrap();
    fold(&f.store, &d, at("2030-01-10T01:00:00Z")).unwrap();
    assert!(current(&f, "passages").0.indexes[0].segments().is_empty());
    f.land_typed(&d, "run-5", json!([{"passage_id": "p4", "embedding": [1.0, 0.0, 1.0]}]), "2030-01-20T00:00:00Z", &f32x3()).unwrap();
    fold(&f.store, &d, at("2030-01-20T01:00:00Z")).unwrap();
    assert!(!first_dir.exists() && !sidecar.exists());
}

/// A vector sidecar is an HNSW graph over unit-length vectors stored as `Float32`, or as `Float16` where its
/// declaration's `storage` is `f16`, its layers seeded from the snapshot id and column, so one staged row set
/// yields a byte-identical graph.
// spec: store.index.graph@fd35f83e
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

/// `n` vectors of `dim` drawn from `seed`, each element in [-1, 1).
fn seeded(n: usize, dim: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut state = seed;
    let mut draw = move || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((state >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0
    };
    (0..n).map(|_| (0..dim).map(|_| draw()).collect()).collect()
}

/// Fold `vectors` into a keyed table whose one vector block carries `extra`, returning the
/// fixture, the current manifest and its directory.
fn fold_vectors(vectors: &[Vec<f32>], extra: &str) -> (Fixture, SnapshotManifest, PathBuf) {
    let dim = vectors[0].len();
    let rows: Vec<Value> = vectors.iter().enumerate().map(|(i, v)| json!({"passage_id": format!("p{i}"), "embedding": v})).collect();
    let f = Fixture::new();
    let d = decl(&format!(
        "name = \"passages\"\nprimary_key = [\"passage_id\"]\n[[pipeline.tables.indexes]]\nkind = \"vector\"\ncolumn = \"embedding\"\nmodel = \"e5\"\ndim = {dim}\n{extra}"
    ));
    f.land_typed(&d, "run-1", Value::Array(rows), "2030-01-01T00:00:00Z", &[("embedding", ColumnType::FixedSizeList(FloatItem::Float32, dim as u32))]).unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let (m, dir) = current(&f, "passages");
    (f, m, dir)
}

/// A vector sidecar is an HNSW graph over unit-length vectors stored as `Float32`, or as `Float16` where its
/// declaration's `storage` is `f16`, its layers seeded from the snapshot id and column, so one staged row set
/// yields a byte-identical graph.
// spec: store.index.graph@fd35f83e
#[test]
fn an_f16_sidecar_stores_its_vectors_in_half_the_bytes_of_its_f32_twin() {
    use contextful_core::store::index::VectorStorage;
    const N: usize = 500;
    const DIM: usize = 32;
    let vectors = seeded(N, DIM, 0x5eed_f016);
    let (_wide_fixture, wide_m, wide_dir) = fold_vectors(&vectors, "");
    let (_half_fixture, half_m, half_dir) = fold_vectors(&vectors, "storage = \"f16\"\n");
    let (wide, half) = (entry(&wide_m), entry(&half_m));
    assert_eq!((wide.storage, half.storage), (VectorStorage::F32, VectorStorage::F16));
    assert_eq!((wide.stored_vector_bytes(), half.stored_vector_bytes()), ((N * DIM * 4) as u64, (N * DIM * 2) as u64));
    let wide_bytes = std::fs::read(wide_dir.join(&wide.path).join(GRAPH_FILE)).unwrap();
    let half_bytes = std::fs::read(half_dir.join(&half.path).join(GRAPH_FILE)).unwrap();
    // One snapshot id seeds both, so the layers match and only the vector section narrows.
    assert_eq!(wide_bytes.len() - half_bytes.len(), N * DIM * 2);
    let own: Value = serde_json::from_slice(&std::fs::read(half_dir.join(&half.path).join("_manifest.json")).unwrap()).unwrap();
    assert_eq!(own["storage"], "f16");

    // Every stored half-width vector is unit length to f16 precision: N rows of DIM halves
    // follow the 32-byte header.
    let section = &half_bytes[32..32 + N * DIM * 2];
    assert_eq!(section.chunks_exact(DIM * 2).count(), N);
    for row in section.chunks_exact(DIM * 2) {
        let v: Vec<f32> = row.chunks_exact(2).map(|b| half::f16::from_le_bytes([b[0], b[1]]).to_f32()).collect();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 2e-3, "{v:?}");
    }
    // Each row, probed with its own vector, is the nearest node of the half-width graph.
    let sidecar = VectorSidecar::open(&half_dir, "passages", &half_m.indexes[0], &Sealing::Plaintext).unwrap();
    for (i, v) in vectors.iter().enumerate() {
        let near = sidecar.probe(v, 1).unwrap();
        assert_eq!(near.first().map(|c| c.id.clone()), Some(format!("p{i}")));
    }
    // One staged row set builds one byte-identical half-width graph.
    let (_again_fixture, again_m, again_dir) = fold_vectors(&vectors, "storage = \"f16\"\n");
    assert!(std::fs::read(again_dir.join(&entry(&again_m).path).join(GRAPH_FILE)).unwrap() == half_bytes);
}

/// A vector block declaring no `storage` stores `Float32`, byte for byte as `storage = "f32"`
/// does, and a declaration changed to `f16` rebuilds the sidecar with no landed run
/// (`store.index.declared-unbuilt`).
#[test]
fn a_vector_sidecar_stores_f32_unless_its_declaration_says_f16() {
    use contextful_core::store::fold::FoldOutcome;
    use contextful_core::store::index::VectorStorage;
    let vectors = seeded(64, 8, 0x5eed_0f32);
    let (f, m, dir) = fold_vectors(&vectors, "");
    let (_explicit_fixture, explicit_m, explicit_dir) = fold_vectors(&vectors, "storage = \"f32\"\n");
    let e = entry(&m);
    assert_eq!(e.storage, VectorStorage::F32);
    let own: Value = serde_json::from_slice(&std::fs::read(dir.join(&e.path).join("_manifest.json")).unwrap()).unwrap();
    assert!(own.get("storage").is_none(), "{own}");
    let bytes = std::fs::read(dir.join(&e.path).join(GRAPH_FILE)).unwrap();
    assert!(bytes == std::fs::read(explicit_dir.join(&entry(&explicit_m).path).join(GRAPH_FILE)).unwrap());
    assert_eq!(e.stored_vector_bytes(), 64 * 8 * 4);

    let half = decl("name = \"passages\"\nprimary_key = [\"passage_id\"]\n[[pipeline.tables.indexes]]\nkind = \"vector\"\ncolumn = \"embedding\"\nmodel = \"e5\"\ndim = 8\nstorage = \"f16\"\n");
    let outcome = fold(&f.store, &half, at("2030-01-01T02:00:00Z")).unwrap();
    assert!(matches!(outcome, FoldOutcome::Folded { runs: 0, rows: 64, .. }), "{outcome:?}");
    let (after, after_dir) = current(&f, "passages");
    assert_eq!((entry(&after).storage, entry(&after).row_count), (VectorStorage::F16, 64));
    let sidecar = VectorSidecar::open(&after_dir, "passages", &after.indexes[0], &Sealing::Plaintext).unwrap();
    assert_eq!(sidecar.probe(&vectors[7], 1).unwrap()[0].id, "p7");
}

/// Two staged snapshot rows sharing an `id_column` value make fold raise `StoreIndexIdNotUnique`, naming the
/// table, column and value, and publish nothing.
// spec: store.index.id-unique@1b4e00c9
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

/// A landing whose identifier or vector reconciles to a type the sidecar does not read
/// refuses before any row lands (`store.index.column-type`).
#[test]
fn a_landing_of_an_unreadable_identifier_or_vector_lands_nothing() {
    let f32x2 = ColumnType::FixedSizeList(FloatItem::Float32, 2);
    for (column, value, types) in [
        ("digest", json!(1.5), vec![("embedding", f32x3()[0].1.clone()), ("digest", ColumnType::Float64)]),
        ("embedding", json!([1.0, 0.0]), vec![("embedding", f32x2)]),
    ] {
        let f = Fixture::new();
        let d = decl(&format!("name = \"passages\"\n{INDEX}id_column = \"digest\"\n"));
        let mut row = json!({"digest": "d1", "embedding": [1.0, 0.0, 0.0]});
        row[column] = value;
        let err = f.land_typed(&d, "run-1", json!([row]), "2030-01-01T00:00:00Z", &types).unwrap_err();
        assert!(matches!(err.store(), Some(StoreError::StoreIndexColumnType(_))), "{column}: {err}");
        assert!(!f.table_dir("passages").join("data/runs/run-1").exists(), "{column}");
    }
}

/// A keyed valid-time table keeps one row per key and line; its sidecar keys on the
/// declared identifier, and every fold publishes.
#[test]
fn a_valid_time_table_folds_its_sidecar_over_a_declared_identifier() {
    let f = Fixture::new();
    let d = decl(&format!(
        "name = \"rates\"\nprimary_key = [\"ccy\"]\n[pipeline.tables.valid_time]\nfrom = \"from_ts\"\n{INDEX}id_column = \"rate_id\"\n"
    ));
    let types = [("embedding", ColumnType::FixedSizeList(FloatItem::Float32, 3)), ("from_ts", ColumnType::Timestamp)];
    f.land_typed(&d, "run-1", json!([{"ccy": "eur", "rate_id": "eur-1", "from_ts": "2030-01-01T00:00:00Z", "embedding": [1.0, 0.0, 0.0]}]), "2030-01-01T00:00:00Z", &types).unwrap();
    f.land_typed(&d, "run-2", json!([{"ccy": "eur", "rate_id": "eur-2", "from_ts": "2030-01-02T00:00:00Z", "embedding": [0.0, 1.0, 0.0]}]), "2030-01-02T00:00:00Z", &types).unwrap();
    fold(&f.store, &d, at("2030-01-02T01:00:00Z")).unwrap();
    let e = entry(&current(&f, "rates").0);
    assert_eq!((e.id_column.as_str(), e.row_count), ("rate_id", 2));
}

const RECALL_N: usize = 2000;
const RECALL_DIM: usize = 32;
const RECALL_QUERIES: usize = 200;
const RECALL_SEED: u64 = 0x5eed_0043;

/// Recall@10 of a sidecar over 2,000 seeded vectors of dim 32, its vector block carrying
/// `extra`, against exact search for 200 seeded queries.
fn recall_at_10(extra: &str) -> f64 {
    let mut drawn = seeded(RECALL_N + RECALL_QUERIES, RECALL_DIM, RECALL_SEED);
    let queries = drawn.split_off(RECALL_N);
    let vectors = drawn;
    let (_f, m, dir) = fold_vectors(&vectors, extra);
    let sidecar = VectorSidecar::open(&dir, "passages", &m.indexes[0], &Sealing::Plaintext).unwrap();
    let cosine = |a: &[f32], b: &[f32]| {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        dot / (a.iter().map(|x| x * x).sum::<f32>().sqrt() * b.iter().map(|x| x * x).sum::<f32>().sqrt())
    };
    let mut hits = 0usize;
    for q in &queries {
        let mut exact: Vec<(f32, usize)> = vectors.iter().enumerate().map(|(i, v)| (cosine(q, v), i)).collect();
        exact.sort_by(|a, b| b.0.total_cmp(&a.0));
        let truth: std::collections::HashSet<String> = exact[..10].iter().map(|(_, i)| format!("p{i}")).collect();
        hits += sidecar.probe(q, 10).unwrap().iter().filter(|c| truth.contains(&c.id)).count();
    }
    hits as f64 / (RECALL_QUERIES * 10) as f64
}

/// Sidecar recall@10 against exact search over seeded vectors (ledger `vector-recall`).
#[test]
fn vector_recall_at_10_holds_against_exact_search() {
    let recall = recall_at_10("");
    contextful_eval::record::emit("vector-recall", recall, RECALL_QUERIES as u64, RECALL_SEED);
    assert!(recall >= 0.95, "recall@10 {recall}");
}

/// A half-width graph keeps recall@10 against exact `Float32` search on the same fixture
/// (`store.index.graph`).
#[test]
fn vector_recall_at_10_holds_on_f16_storage() {
    let recall = recall_at_10("storage = \"f16\"\n");
    assert!(recall >= 0.95, "f16 recall@10 {recall}");
}

pub const FULLTEXT: &str = "[[pipeline.tables.indexes]]\nkind = \"fulltext\"\ncolumn = \"body\"\n";

fn fulltext_entry(m: &SnapshotManifest) -> FulltextEntry {
    m.indexes
        .iter()
        .find_map(|e| match e {
            IndexEntry::Fulltext(e) => Some(e.clone()),
            _ => None,
        })
        .expect("a full-text entry")
}

fn probe_ids(sidecar: &FulltextSidecar, query: &[&str], k: usize) -> Vec<String> {
    let tokens: Vec<String> = query.iter().map(|t| t.to_string()).collect();
    sidecar.probe(&tokens, k).unwrap().candidates.into_iter().map(|c| c.id).collect()
}

/// Fold builds each declared full-text sidecar from staged rows with non-null identifiers and text yielding a term,
/// recording it in the snapshot manifest.
// spec: store.index.fulltext-by-fold@6cc65b1c
#[test]
fn the_fold_builds_each_full_text_sidecar_over_identified_rows_with_terms() {
    let f = Fixture::new();
    let d = decl(&format!("name = \"passages\"\n{FULLTEXT}id_column = \"digest\"\ntokenizer = \"cjk\"\n{INDEX}id_column = \"digest\"\n"));
    f.land_typed(&d, "run-1", json!([
        {"digest": "d1", "body": "Solar battery storage", "embedding": [1.0, 0.0, 0.0]},
        {"digest": "d2", "body": "メニューの設定画面を開く", "embedding": [0.0, 1.0, 0.0]},
        {"digest": null, "body": "battery without an identifier", "embedding": [0.0, 0.0, 1.0]},
        {"digest": "d4", "body": null, "embedding": [1.0, 1.0, 0.0]},
        {"digest": "d5", "body": " -- ", "embedding": [0.0, 1.0, 1.0]},
    ]), "2030-01-01T00:00:00Z", &f32x3()).unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let (m, dir) = current(&f, "passages");
    // Both declared sidecars sit in the one snapshot, each with its own entry.
    assert_eq!(m.indexes.len(), 2);
    let e = fulltext_entry(&m);
    assert_eq!((e.path.as_str(), e.column.as_str(), e.id_column.as_str(), e.tokenizer), ("indexes/fts-body-cjk", "body", "digest", Tokenizer::Cjk));
    assert_eq!((e.table.as_str(), e.snapshot_id.as_str(), e.row_count, e.key_version), ("passages", m.snapshot_id.to_string().as_str(), 2, 0));
    assert!(dir.join(&e.path).join(POSTINGS_FILE).is_file());
    let own: FulltextEntry = serde_json::from_slice(&std::fs::read(dir.join(&e.path).join("_manifest.json")).unwrap()).unwrap();
    assert_eq!(own, e);
    let entry = m.indexes.iter().find(|x| x.kind() == Some(IndexKind::Fulltext)).unwrap();
    let sidecar = FulltextSidecar::open(&dir, "passages", entry, &Sealing::Plaintext).unwrap();
    assert!(sidecar.is_mapped());
    assert_eq!(probe_ids(&sidecar, &["battery"], 10), ["d1"]);
    assert_eq!(probe_ids(&sidecar, &["画面"], 10), ["d2"]);
    // A table declaring no full-text sidecar records none.
    let v = Fixture::new();
    let only_vector = decl(&format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\n{INDEX}"));
    v.land_typed(&only_vector, "run-1", json!([{"passage_id": "p1", "body": "battery", "embedding": [1.0, 0.0, 0.0]}]), "2030-01-01T00:00:00Z", &f32x3()).unwrap();
    fold(&v.store, &only_vector, at("2030-01-01T01:00:00Z")).unwrap();
    assert!(!current(&v, "passages").0.indexes.is_empty(), "the exclusion below ranges over no element");
    assert!(current(&v, "passages").0.indexes.iter().all(|x| x.kind() == Some(IndexKind::Vector)));
    // A full-text sidecar over text the staged rows type otherwise refuses before landing.
    let n = Fixture::new();
    let numeric = decl(&format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\n{FULLTEXT}"));
    let err = n.land(&numeric, "run-1", json!([{"passage_id": "p1", "body": 12}]), "2030-01-01T00:00:00Z").unwrap_err();
    assert!(matches!(err.store(), Some(StoreError::StoreIndexColumnType(_))), "{err}");
}

/// Each full-text segment is one file holding a sorted term dictionary and each term's row identifiers and
/// positions, which readers binary-search mapped or decrypted.
// spec: store.index.postings@324f3dc3
#[test]
fn one_row_set_lays_out_one_postings_file_a_probe_reads_by_term() {
    const N: usize = 2000;
    // `alpha` on every 200th row, `beta` on every 400th, both on rows divisible by 400.
    let rows: Vec<Value> = (0..N)
        .map(|i| {
            let mut body = format!("routine entry {i} gamma");
            if i % 200 == 0 {
                body.push_str(" alpha");
            }
            if i % 400 == 0 {
                body.push_str(" beta");
            }
            json!({"passage_id": format!("p{i:04}"), "body": body})
        })
        .collect();
    let postings = |at_: &str| {
        let f = Fixture::new();
        let d = decl(&format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\n{FULLTEXT}"));
        f.land(&d, "run-1", Value::Array(rows.clone()), "2030-01-01T00:00:00Z").unwrap();
        fold(&f.store, &d, at(at_)).unwrap();
        let (m, dir) = current(&f, "passages");
        let e = fulltext_entry(&m);
        let sidecar = FulltextSidecar::open(&dir, "passages", &m.indexes[0], &Sealing::Plaintext).unwrap();
        let tokens = vec!["alpha".to_string(), "beta".to_string()];
        let probe = sidecar.probe(&tokens, 64).unwrap();
        (std::fs::read(dir.join(&e.path).join(POSTINGS_FILE)).unwrap(), e, probe)
    };
    let (a, e, probe) = postings("2030-01-01T01:00:00Z");
    let (b, _, _) = postings("2030-01-02T01:00:00Z");
    assert!(a == b, "one row set laid out two postings files");
    assert_eq!(&a[..8], b"CFPOST02");
    // Distinct terms: the row numbers, `routine`, `entry`, `gamma`, `alpha`, `beta`.
    assert_eq!((e.row_count, e.term_count), (N as u64, N as u64 + 5));
    // Rows with both terms lead, then rows with `alpha` alone; ties by row order.
    let ids: Vec<&str> = probe.candidates.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids.len(), 10);
    assert_eq!(&ids[..5], ["p0000", "p0400", "p0800", "p1200", "p1600"]);
    assert_eq!(&ids[5..], ["p0200", "p0600", "p1000", "p1400", "p1800"]);
    // The probe decodes the postings of its own terms alone: 10 and 5, of 2000 rows.
    assert_eq!(probe.postings_scored, 15);
    contextful_eval::record::emit("lexical-sidecar-cost", probe.postings_scored as f64, 1, 0);
    // Truncated or foreign bytes do not parse as a layout.
    assert!(contextful_context::fulltext::postings::Layout::parse(&a[..a.len() - 1]).is_none());
    assert!(contextful_context::fulltext::postings::Layout::parse(b"CFHNSW01").is_none());
}

/// A delta replacing and removing no stored row adds a segment to each current sidecar, referencing earlier ones;
/// probes read every segment, scoring full text over their summed statistics.
// spec: store.index.sidecar-segments@4875d648
#[test]
fn a_pass_of_new_identifiers_extends_each_sidecar_by_a_segment() {
    let row = |i: usize| {
        let body = match i % 4 {
            0 => "solar battery storage",
            1 => "battery recycling plant",
            2 => "wind farm permit",
            _ => "grid battery battery tariff",
        };
        json!({"passage_id": format!("p{i:02}"), "body": format!("{body} {i}"), "embedding": [1.0 + i as f32, (i % 3) as f32, 1.0]})
    };
    let d = decl(&format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\n{INDEX}{FULLTEXT}"));
    let segmented = Fixture::new();
    for (k, range) in [(0..8), (8..12), (12..20)].into_iter().enumerate() {
        let rows = Value::Array(range.map(row).collect());
        segmented.land_typed(&d, &format!("run-{k}"), rows, &format!("2030-01-01T0{k}:00:00Z"), &f32x3()).unwrap();
        fold(&segmented.store, &d, at(&format!("2030-01-01T0{k}:30:00Z"))).unwrap();
    }
    let (m, dir) = current(&segmented, "passages");
    let text = fulltext_entry(&m);
    assert_eq!((text.extensions, text.row_count, text.segments.iter().map(|s| s.row_count).collect::<Vec<_>>()), (2, 20, vec![8, 4]));
    let graph = entry(&m);
    assert_eq!((graph.extensions, graph.row_count, graph.segments.len()), (2, 20, 2));
    // The new directory's files cover the last landing alone.
    let own = contextful_context::fulltext::postings::Layout::parse(&std::fs::read(dir.join(&text.path).join(POSTINGS_FILE)).unwrap()).unwrap();
    assert_eq!(own.doc_count, 8);

    // Scores over the segments equal a whole build's over the same rows.
    let whole = Fixture::new();
    whole.land_typed(&d, "run-0", Value::Array((0..20).map(row).collect()), "2030-01-01T00:00:00Z", &f32x3()).unwrap();
    fold(&whole.store, &d, at("2030-01-01T00:30:00Z")).unwrap();
    let scores = |f: &Fixture| {
        let (m, dir) = current(f, "passages");
        let e = m.indexes.iter().find(|x| x.kind() == Some(IndexKind::Fulltext)).unwrap();
        let sidecar = FulltextSidecar::open(&dir, "passages", e, &Sealing::Plaintext).unwrap();
        let mut found: Vec<(String, i64)> = sidecar
            .probe(&["battery".to_string(), "permit".to_string()], 20)
            .unwrap()
            .candidates
            .into_iter()
            .map(|c| (c.id, (c.score * 1e9).round() as i64))
            .collect();
        found.sort();
        found
    };
    let (a, b) = (scores(&segmented), scores(&whole));
    assert_eq!(a.len(), 20);
    assert_eq!(a, b);
    // The vector probe reaches rows in every segment.
    let e = m.indexes.iter().find(|x| x.kind() == Some(IndexKind::Vector)).unwrap();
    let vectors = VectorSidecar::open(&dir, "passages", e, &Sealing::Plaintext).unwrap();
    let near = |q: [f32; 3]| vectors.probe(&q, 1).unwrap()[0].id.clone();
    assert_eq!((near([1.0, 0.0, 1.0]), near([20.0, 1.0, 1.0])), ("p00".to_string(), "p19".to_string()));
}

#[cfg(feature = "read")]
/// A unit's latest `ok` or `empty` landing supersedes rows landed earlier under another key. Those rows answer until
/// that landing. The next fold drops the superseded rows and rebuilds the table's sidecars without them.
// spec: run.emit.stale-supersedes@b1c6c823
#[test]
fn superseded_derived_rows_stop_answering_at_the_replacing_landing_and_leave_the_sidecar_at_the_fold() {
    let f = Fixture::new();
    let d = decl(&format!("name = \"passages\"\nprimary_key = [\"unit_ref\", \"derivation_key\", \"cue_seq\"]\n{FULLTEXT}id_column = \"row_id\"\n"));
    let (k1, k2) = ("1".repeat(64), "2".repeat(64));
    let passage = |unit: &str, key: &str, text: &str| json!({"row_id": format!("{unit}-{}", &key[..1]), "unit_ref": unit, "derivation_key": key, "cue_seq": 0, "kind": "passage", "unit_status": "ok", "body": text});
    let marker = |unit: &str, key: &str, status: &str| json!({"row_id": null, "unit_ref": unit, "derivation_key": key, "cue_seq": -1, "kind": "marker", "unit_status": status, "body": null});
    let probe = |words: &[&str]| {
        let (m, dir) = current(&f, "passages");
        let entry = m.indexes.iter().find(|x| x.kind() == Some(IndexKind::Fulltext)).unwrap().clone();
        let mut ids = probe_ids(&FulltextSidecar::open(&dir, "passages", &entry, &Sealing::Plaintext).unwrap(), words, 10);
        ids.sort();
        ids
    };
    f.land(&d, "run-1", json!([passage("a", &k1, "solar battery"), passage("b", &k1, "wind turbine"), passage("c", &k1, "tidal lagoon")]), "2030-01-01T00:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T00:30:00Z")).unwrap();
    assert_eq!(probe(&["solar"]), ["a-1"]);
    // `a` re-derives, `b` retries under the new key, and `c` settles empty under it.
    f.land(&d, "run-2", json!([passage("a", &k2, "hydro turbine"), marker("b", &k2, "unavailable"), marker("c", &k2, "empty")]), "2030-01-01T01:00:00Z").unwrap();
    let live = || f.query(&d, Bounds::default(), "SELECT unit_ref, left(derivation_key, 1), kind FROM t ORDER BY 1, 2, 3");
    let current = [[s("a"), s("2"), s("passage")], [s("b"), s("1"), s("passage")], [s("b"), s("2"), s("marker")], [s("c"), s("2"), s("marker")]];
    // From the replacing landing on, the table's relation serves no superseded row, though
    // the sidecar still holds it until the fold; a sidecar hit re-joins through the relation.
    assert_eq!(probe(&["solar"]), ["a-1"]);
    assert_eq!(live(), current, "a superseded row stops answering at the replacing landing");
    let rejoined = f.query(&d, Bounds::default(), "SELECT row_id FROM t WHERE row_id IN ('a-1', 'c-1') ORDER BY 1");
    assert!(rejoined.is_empty(), "a sidecar hit on a superseded row re-joins to nothing: {rejoined:?}");
    fold(&f.store, &d, at("2030-01-01T01:30:00Z")).unwrap();
    assert!(probe(&["solar"]).is_empty(), "the superseded passage leaves the sidecar");
    assert!(probe(&["tidal"]).is_empty(), "an empty landing supersedes too");
    assert_eq!(probe(&["turbine"]), ["a-2", "b-1"], "a retry marker supersedes nothing");
    assert_eq!(live(), current);
}
