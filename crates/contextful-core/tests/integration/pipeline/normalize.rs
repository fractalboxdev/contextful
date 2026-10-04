use contextful_core::pipeline::normalize::relational_tables;
use contextful_core::run::ports::Row;

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
