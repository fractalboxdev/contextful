//! `store.index`: clustering and partitioning of a snapshot's parts.

use crate::support::{at, decl, query, s, Fixture};
use contextful_context::fold::{escape, fold};
use contextful_core::store::bound_time::Bounds;
use serde_json::json;

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
