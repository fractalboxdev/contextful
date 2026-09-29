//! `store.reserve`: the injected columns as a reader sees them, and batch validation.

use crate::support::{at, decl, query, s, Fixture};
use contextful_context::land::{land, Batch, RunContext};
use contextful_core::connector::infer::Provenance;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reserve::Injection;
use contextful_core::store::StoreError;
use serde_json::{json, Value};
use std::collections::HashMap;

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

/// The engine injects `_taint`, a label under {{connector.infer.provenance-order}}, on each row a model's output lands as, replacing any producer value; a row no model produced omits it.
// spec: store.reserve.taint@bdb810ae
#[test]
fn a_model_output_row_carries_the_engine_taint_and_no_other_row_does() {
    let f = Fixture::new();
    let d = decl("name = \"claims\"");
    let context = |run: &str, taint: Option<Provenance>| RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: run.into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint },
        committed_at: at("2030-01-01T00:00:00Z"),
    };
    let batch = |rows: Value| Batch { rows: rows.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect(), types: HashMap::new() };
    land(&f.store, &d, &batch(json!([{"id": "a", "_taint": "operator"}])), &context("run-1", Some(Provenance::ThirdParty))).unwrap();
    land(&f.store, &d, &batch(json!([{"id": "b", "_taint": "operator"}])), &context("run-2", None)).unwrap();
    assert_eq!(
        f.query(&d, Bounds::default(), "SELECT id, _taint FROM t ORDER BY id"),
        [[s("a"), s("ingested:third-party")], [s("b"), None]],
        "the engine's label replaces the producer's, and a row no model produced carries none"
    );
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

/// An optional column's value is held to its vocabulary whatever JSON type it arrives as:
/// a number is read as its text and refused like the same text would be, and a null is a
/// value the producer did not set.
#[test]
fn an_optional_column_is_checked_whatever_json_type_it_arrives_as() {
    let f = Fixture::new();
    let d = decl("name = \"notes\"");
    for (col, value) in [("_modality", json!(7)), ("_prompt_hash", json!(12345)), ("_modality", json!(true))] {
        let rows = json!([{"id": "a", col.to_string(): value}]);
        let err = f.land(&d, "run-1", rows, "2030-01-01T00:00:00Z").unwrap_err();
        assert!(err.to_string().contains(col), "a non-string `{col}` landed unchecked: {err}");
    }
    // A null is absence, not a bad value.
    f.land(&d, "run-1", json!([{"id": "a", "_modality": null}]), "2030-01-01T00:00:00Z").unwrap();
}

/// A pipeline declaring a table inside a reserved namespace raises `StoreReservedTableName` when its manifest is assembled, naming the reservation.
#[test]
fn a_reserved_table_name_refuses_the_landing() {
    let f = Fixture::new();
    let err = f.land(&decl("name = \"_runs\""), "run-1", json!([{"id": "a"}]), "2030-01-01T00:00:00Z").unwrap_err();
    assert!(matches!(err.store(), Some(StoreError::StoreReservedTableName(_))), "{err}");
}

/// The injected columns every landing carries stay non-null in `schema.json`, whichever
/// batch reached the table first and whatever columns later batches add.
#[test]
fn injected_columns_stay_non_null_in_the_merged_schema() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    let nullability = |f: &Fixture| -> Vec<(String, bool)> {
        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(f.table_dir("filings").join("schema.json")).unwrap()).unwrap();
        doc["fields"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| ["_ingested_at", "_run_id", "_site_id"].contains(&c["name"].as_str().unwrap()))
            .map(|c| (c["name"].as_str().unwrap().to_string(), c["nullable"].as_bool().unwrap()))
            .collect()
    };
    let expected = [("_ingested_at".to_string(), false), ("_run_id".to_string(), false), ("_site_id".to_string(), false)];

    f.land(&d, "run-1", json!([{"id": "a"}]), "2030-01-01T00:00:00Z").unwrap();
    assert_eq!(nullability(&f), expected, "after the first landing");
    f.land(&d, "run-2", json!([{"id": "b", "title": "t"}]), "2030-01-01T00:01:00Z").unwrap();
    assert_eq!(nullability(&f), expected, "after a landing adding a column");

    // A table whose first run landed zero rows merges the injected columns into an empty schema.
    let g = Fixture::new();
    g.land(&d, "run-1", json!([]), "2030-01-01T00:00:00Z").unwrap();
    g.land(&d, "run-2", json!([{"id": "a"}]), "2030-01-01T00:01:00Z").unwrap();
    assert_eq!(nullability(&g), expected, "after an empty first run");
}
