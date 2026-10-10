use contextful_core::pipeline::normalize::{check_list_index, native_types, relational_tables, Mode, Normalize, NormalizedGroup};
use contextful_core::run::ports::Row;
use contextful_core::store::reconcile::{ColumnType, StructField};

// spec: run.normalize.field-collision@8ba18d89
#[test]
fn source_fields_cannot_replace_relational_identity_columns() {
    let source: Row = serde_json::from_value(serde_json::json!({
        "row_id": "source-root-id",
        "load_id": "source-load-id",
        "events": [{"parent_id": "source-parent-id", "list_index": 99}]
    })).unwrap();
    let tables = relational_tables(vec![source], "spans", "run-1", 5);
    let root = &tables["spans"][0];
    let child = &tables["spans_events"][0];
    let root_id = root["row_id"].as_str().unwrap();
    assert_eq!(root_id.len(), 64);
    assert_eq!(root["load_id"], "run-1");
    assert_eq!(root["source_row_id"], "source-root-id");
    assert_eq!(root["source_load_id"], "source-load-id");
    assert_eq!(child["parent_id"], root_id);
    assert_eq!(child["list_index"], 0);
    assert_eq!(child["source_parent_id"], "source-parent-id");
    assert_eq!(child["source_list_index"], 99);
}

/// Relational identity columns preserve repeated nested values and replay the same ids.
// spec: run.normalize.identity-columns@303cb038
// spec: run.normalize.row-id@44330c32
#[test]
fn repeated_nested_items_keep_distinct_parent_links_and_stable_ids() {
    let source: Row = serde_json::from_value(serde_json::json!({
        "groups": [
            {"items": ["same"]},
            {"items": ["same"]}
        ]
    })).unwrap();
    let first = relational_tables(vec![source.clone()], "entries", "run-1", 5);
    let replay = relational_tables(vec![source], "entries", "run-2", 5);
    let groups = &first["entries_groups"];
    let items = &first["entries_groups_items"];

    assert_eq!(groups.len(), 2);
    assert_ne!(groups[0]["row_id"], groups[1]["row_id"], "each indexed child has its own identity");
    assert_eq!(groups[0]["list_index"], 0);
    assert_eq!(groups[1]["list_index"], 1);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["parent_id"], groups[0]["row_id"]);
    assert_eq!(items[1]["parent_id"], groups[1]["row_id"]);
    assert_eq!(items[0]["root_id"], first["entries"][0]["row_id"]);
    assert_eq!(items[1]["root_id"], first["entries"][0]["row_id"]);
    assert_eq!(first["entries"][0]["load_id"], "run-1");
    assert_eq!(replay["entries"][0]["load_id"], "run-2");
    assert_eq!(first["entries"][0]["row_id"], replay["entries"][0]["row_id"]);
    assert_eq!(groups[0]["row_id"], replay["entries_groups"][0]["row_id"]);
    assert_eq!(groups[1]["row_id"], replay["entries_groups"][1]["row_id"]);
}

/// A relational child table emitted without the list index that makes its projection reversible raises `PipelineListIndexMissing`, naming the parent and the list.
// spec: run.normalize.list-index-missing@b1fd8277
#[test]
fn a_child_table_without_its_list_index_is_refused_naming_parent_and_list() {
    let source: Row = serde_json::from_value(serde_json::json!({
        "id": "s1",
        "events": [{"name": "start", "attrs": [1, 2]}, {"name": "end"}]
    })).unwrap();
    let tables = relational_tables(vec![source], "spans", "run-1", 5);
    check_list_index(&tables, "spans").unwrap();

    let mut stripped = tables.clone();
    stripped.get_mut("spans_events").unwrap()[1].remove("list_index");
    let err = check_list_index(&stripped, "spans").unwrap_err().to_string();
    assert!(err.starts_with("PipelineListIndexMissing"), "{err}");
    assert!(err.contains("`spans`") && err.contains("`events`"), "{err}");

    let mut nested = tables;
    nested.get_mut("spans_events_attrs").unwrap()[0].remove("list_index");
    let err = check_list_index(&nested, "spans").unwrap_err().to_string();
    assert!(err.contains("`spans_events`") && err.contains("`attrs`"), "{err}");
}

/// Type inference over deferred-typing JSON, struct flattening, list-to-child extraction and id assignment run in engine code; no connector reimplements them and no dataframe library participates.
// spec: run.normalize.host-stage@a36cbdfc
#[test]
fn engine_code_types_flattens_extracts_and_identifies_raw_json_rows() {
    let rows: Vec<Row> = serde_json::from_value(serde_json::json!([
        {"id": "s1", "resource": {"service": "api"}, "events": [{"name": "start"}]}
    ])).unwrap();
    let types = native_types(&rows, 5);
    assert_eq!(types["resource"], ColumnType::Struct(vec![StructField::new("service", ColumnType::Utf8)]));
    assert_eq!(types["events"], ColumnType::list(ColumnType::Struct(vec![StructField::new("name", ColumnType::Utf8)])));
    let tables = relational_tables(rows, "spans", "run-1", 5);
    assert_eq!(tables.keys().collect::<Vec<_>>(), ["spans", "spans_events"]);
    let root = &tables["spans"][0];
    assert_eq!(root["resource_service"], "api", "a struct flattens into parent-child column names");
    assert_eq!(root["row_id"].as_str().unwrap().len(), 64);
    let child = &tables["spans_events"][0];
    assert_eq!((&child["name"], &child["parent_id"], &child["list_index"]), (&serde_json::json!("start"), &root["row_id"], &serde_json::json!(0)));
}

/// The canonical form is nested Arrow structs and lists; relational shredding is a late projection at the sink, so a source landing in two sinks normalizes once.
// spec: run.normalize.normalized-form@2bcf993d
#[test]
fn one_normalized_group_holds_nested_rows_and_shreds_only_when_materialized() {
    let rows: Vec<Row> = serde_json::from_value(serde_json::json!([
        {"id": "s1", "resource": {"service": "api"}, "events": [{"name": "start"}, {"name": "end"}]}
    ])).unwrap();
    let group = NormalizedGroup::new(rows.clone(), "spans", "run-1", 5);
    // The group keeps the nested values a native sink lands as struct and list columns.
    assert_eq!(group.column_values("resource").unwrap(), [serde_json::json!({"service": "api"})]);
    let types = native_types(&rows, 5);
    assert!(!types.is_empty());
    assert!(types.values().all(ColumnType::is_nested));
    // The relational projection is computed from the same group at materialization.
    assert_eq!(group.destinations(), ["spans", "spans_events"]);
    let tables = group.into_tables().unwrap();
    assert_eq!(tables, relational_tables(rows, "spans", "run-1", 5));
    assert_eq!(tables["spans_events"].len(), 2);
}

/// Mode resolves per stream per sink: explicit declaration, then sink capability, then `native`.
// spec: run.normalize.mode-resolution@38ae8c8e
#[test]
fn an_explicit_mode_wins_and_an_undeclared_one_resolves_native() {
    assert_eq!(Normalize::parse(None).unwrap(), Normalize { mode: Mode::Native, depth: 5 });
    assert_eq!(Normalize::parse(Some(&serde_json::json!("relational"))).unwrap().mode, Mode::Relational);
    assert_eq!(Normalize::parse(Some(&serde_json::json!({"depth": 2}))).unwrap(), Normalize { mode: Mode::Native, depth: 2 });
    assert_eq!(Normalize::parse(Some(&serde_json::json!({"mode": "relational", "depth": 3}))).unwrap(), Normalize { mode: Mode::Relational, depth: 3 });
}
