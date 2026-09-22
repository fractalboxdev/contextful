//! `store.reserve`: the injected columns as a reader sees them, and batch validation.

use crate::support::{decl, query, s, Fixture};
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::StoreError;
use serde_json::json;

/// The engine injects `_ingested_at` as a non-null Parquet `TIMESTAMP(UTC, NANOS)`, `_run_id`, `_batch_seq` as int32 where a batch scope exists, `_site_id`, and `_authored_by` where an authenticated subject authorized the write, replacing any producer value.
// spec: store.reserve.injected@a7ade4f8
#[test]
fn the_engine_injects_provenance_and_replaces_producer_values() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    f.land(
        &d,
        "run-1",
        json!([{"id": "a", "_run_id": "forged", "_ingested_at": "1999-01-01T00:00:00Z", "_site_id": "elsewhere"}]),
        "2030-01-01T00:00:00.123456789Z",
    )
    .unwrap();
    let file = f.store.root().join(&f.scan(&d, Bounds::default()).unwrap().files[0]);
    let described = query(&format!(
        "SELECT name, type, repetition_type, logical_type FROM parquet_schema('{}') WHERE name LIKE '\\_%' ESCAPE '\\' ORDER BY name",
        file.display()
    ));
    let ingested = described.iter().find(|r| r[0] == s("_ingested_at")).unwrap();
    assert_eq!(ingested[1], s("INT64"));
    assert_eq!(ingested[2], s("REQUIRED"));
    let logical = ingested[3].clone().unwrap();
    assert!(logical.contains("NANOS") && logical.contains("isAdjustedToUTC=1"), "{logical}");
    let seq = described.iter().find(|r| r[0] == s("_batch_seq")).unwrap();
    assert_eq!(seq[1], s("INT32"));

    let row = f.query(&d, Bounds::default(), "SELECT _run_id, _site_id, _batch_seq, epoch_ns(_ingested_at) FROM t");
    assert_eq!(row, [[s("run-1"), s("site-a"), s("0"), s("1893456000123456000")]]);
}

/// A producer column inside the `_` namespace and outside the optional set raises `StoreReservedColumnName` at reconciliation, before any Parquet.
#[test]
fn a_reserved_producer_column_refuses_before_any_parquet() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    let err = f.land(&d, "run-1", json!([{"id": "a", "_score": 1}]), "2030-01-01T00:00:00Z").unwrap_err();
    assert!(matches!(err.store(), Some(StoreError::StoreReservedColumnName(_))), "{err}");
    assert!(!f.table_dir("filings").exists());
}

/// A producer sets any of `_modality`, `_lang`, `_provenance` and `_prompt_hash`, and each surfaces in the provenance envelope where present.
#[test]
fn a_producer_sets_the_optional_columns_and_modality_is_checked() {
    let f = Fixture::new();
    let d = decl("name = \"notes\"");
    let hash = format!("sha256:{}", "0".repeat(64));
    f.land(&d, "run-1", json!([{"id": "a", "_modality": "text", "_lang": "en-GB", "_prompt_hash": hash}]), "2030-01-01T00:00:00Z").unwrap();
    assert_eq!(f.query(&d, Bounds::default(), "SELECT _modality, _lang FROM t"), [[s("text"), s("en-GB")]]);
    let err = f.land(&d, "run-2", json!([{"id": "b", "_modality": "video"}]), "2030-01-01T00:01:00Z").unwrap_err();
    assert!(err.to_string().contains("video"), "{err}");
    assert_eq!(f.scan(&d, Bounds::default()).unwrap().files.len(), 1, "the invalid batch landed");
}

/// A pipeline declaring a table inside a reserved namespace raises `StoreReservedTableName` when its manifest is assembled, naming the reservation.
#[test]
fn a_reserved_table_name_refuses_the_landing() {
    let f = Fixture::new();
    let err = f.land(&decl("name = \"_runs\""), "run-1", json!([{"id": "a"}]), "2030-01-01T00:00:00Z").unwrap_err();
    assert!(matches!(err.store(), Some(StoreError::StoreReservedTableName(_))), "{err}");
}
