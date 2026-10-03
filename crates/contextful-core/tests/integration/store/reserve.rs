//! `store.reserve`: the `_` namespace, injected columns, reserved tables and the ledger path.

use contextful_core::store::reconcile::{Column, ColumnType, Schema};
use contextful_core::store::reserve::{
    check_table_name, ledger_path, optional_value_problem, producer_columns, Injection, INJECTED, OPTIONAL,
};
use contextful_core::store::StoreError;

fn schema(names: &[&str]) -> Schema {
    Schema { columns: names.iter().map(|n| Column::new(*n, ColumnType::Utf8, true)).collect() }
}

/// Column names beginning `_` belong to the engine; the injected set and the reserved optional set are its whole content.
// spec: store.reserve.underscore-namespace@4b90be3f
#[test]
fn the_underscore_namespace_is_the_injected_and_optional_sets() {
    assert!(INJECTED.iter().chain(OPTIONAL.iter()).all(|c| c.starts_with('_')));
    let kept = producer_columns(&schema(&["title", "_modality", "_lang", "_provenance", "_prompt_hash"])).unwrap();
    assert_eq!(kept.columns.len(), 5);
    assert!(producer_columns(&schema(&["_internal"])).is_err());
}

/// The engine injects `_ingested_at` as a non-null Parquet `TIMESTAMP(UTC, NANOS)`, `_run_id`, `_batch_seq` as int32 where a batch scope exists, `_site_id`, and `_authored_by` where an authenticated subject authorized the write, replacing any producer value.
#[test]
fn a_producer_value_in_an_injected_column_is_dropped_for_the_engine_to_replace() {
    let kept = producer_columns(&schema(&["title", "_ingested_at", "_run_id"])).unwrap();
    assert_eq!(kept.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["title"]);
}

/// A path with no batch scope or no authenticated subject omits that column instead of writing nulls.
// spec: store.reserve.no-placeholder@535d67e3
#[test]
fn a_missing_scope_omits_its_column() {
    let full = Injection { run_id: "r".into(), site_id: "s".into(), batch_seq: Some(0), authored_by: Some("user://dana".into()), taint: None };
    let names = |i: &Injection| i.columns().into_iter().map(|c| (c.name, c.ty, c.nullable)).collect::<Vec<_>>();
    assert_eq!(
        names(&full),
        [
            ("_ingested_at".to_string(), ColumnType::Timestamp, false),
            ("_run_id".to_string(), ColumnType::Utf8, false),
            ("_row_seq".to_string(), ColumnType::Int64, false),
            ("_commit_seq".to_string(), ColumnType::Int64, false),
            ("_batch_seq".to_string(), ColumnType::Int32, false),
            ("_site_id".to_string(), ColumnType::Utf8, false),
            ("_authored_by".to_string(), ColumnType::Utf8, false),
        ]
    );
    let bare = Injection { batch_seq: None, authored_by: None, ..full };
    let bare_names: Vec<String> = bare.columns().into_iter().map(|c| c.name).collect();
    assert_eq!(bare_names, ["_ingested_at", "_run_id", "_row_seq", "_commit_seq", "_site_id"]);
}

/// A producer column inside the `_` namespace and outside the optional set raises `StoreReservedColumnName` at reconciliation, before any Parquet.
// spec: store.reserve.column-name@10984555
#[test]
fn a_producer_column_in_the_namespace_is_refused() {
    match producer_columns(&schema(&["title", "_score"])) {
        Err(StoreError::StoreReservedColumnName(m)) => assert!(m.contains("_score"), "{m}"),
        other => panic!("expected StoreReservedColumnName, got {other:?}"),
    }
}

/// `_modality` takes one of text, image, audio, structured or mixed; another value fails validation of its batch.
// spec: store.reserve.modality@f5d94c7f
#[test]
fn modality_takes_five_values() {
    for ok in ["text", "image", "audio", "structured", "mixed"] {
        assert_eq!(optional_value_problem("_modality", ok), None);
    }
    assert!(optional_value_problem("_modality", "video").unwrap().contains("video"));
    assert!(optional_value_problem("_modality", "Text").is_some());
}

/// `_prompt_hash` is `sha256:<hex>` over the prompt template, not the rendered prompt.
#[test]
fn prompt_hash_is_a_prefixed_sha256() {
    let hex = "a".repeat(64);
    assert_eq!(optional_value_problem("_prompt_hash", &format!("sha256:{hex}")), None);
    assert!(optional_value_problem("_prompt_hash", &hex).is_some());
    assert!(optional_value_problem("_prompt_hash", "sha256:abc").is_some());
}

/// The engine reserves two table namespaces: `_runs`, the durable run record, and `_visibility`, under which
/// {{disclosure.mirror.grants-table}} lands each source's mirrored access tables.
// spec: store.reserve.table-namespaces@3198fc8b
#[test]
fn the_visibility_namespace_holds_each_sources_access_tables() {
    for bad in ["_visibility", "_visibility/wiki/grants", "_visibility/drive/resources", "_runs"] {
        match check_table_name(bad) {
            Err(StoreError::StoreReservedTableName(m)) => assert!(m.contains("reserved for"), "{m}"),
            other => panic!("{bad}: expected StoreReservedTableName, got {other:?}"),
        }
    }
    for ok in ["_visibilityx", "access/grants", "visibility/grants", "access_grants"] {
        check_table_name(ok).unwrap();
    }
}

/// A pipeline declaring a table inside a reserved namespace raises `StoreReservedTableName` when its manifest is assembled, naming the reservation.
// spec: store.reserve.table-name@4603a9ec
#[test]
fn a_table_inside_a_reserved_namespace_is_refused() {
    for bad in ["_runs", "_runs/archive", "_visibility/wiki/grants"] {
        match check_table_name(bad) {
            Err(StoreError::StoreReservedTableName(m)) => assert!(m.contains("reserved for"), "{m}"),
            other => panic!("{bad}: expected StoreReservedTableName, got {other:?}"),
        }
    }
    for ok in ["filings", "research/filings", "_runsheet", "accessibility"] {
        check_table_name(ok).unwrap();
    }
}

/// A table name ending in `__requests` is reserved to request-ledger relations and refuses as
/// {{store.reserve.table-name}}.
// spec: store.reserve.ledger-suffix@d94383f5
#[test]
fn a_table_named_like_a_ledger_is_refused() {
    for bad in ["notes__requests", "research/notes__requests"] {
        match check_table_name(bad) {
            Err(StoreError::StoreReservedTableName(m)) => assert!(m.contains("__requests"), "{m}"),
            other => panic!("{bad}: expected StoreReservedTableName, got {other:?}"),
        }
    }
    for ok in ["requests", "research/requests", "notes__request", "notes__requests_archive"] {
        check_table_name(ok).unwrap();
    }
}

/// A run's request ledger is `requests/<run-id>.<node-id>.parquet`, disjoint per run and per writing node.
// spec: store.reserve.ledger-path@bba6cadb
#[test]
fn the_ledger_path_is_disjoint_per_run_and_node() {
    assert_eq!(ledger_path("run-4815", "ingest-a"), "requests/run-4815.ingest-a.parquet");
    assert_ne!(ledger_path("run-4815", "ingest-a"), ledger_path("run-4815", "ingest-b"));
}
