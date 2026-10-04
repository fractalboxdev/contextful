use contextful_core::pipeline::normalize::relational_tables;
use contextful_core::run::ports::Row;

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
    assert_eq!(first["entries"][0]["row_id"], replay["entries"][0]["row_id"]);
    assert_eq!(groups[0]["row_id"], replay["entries_groups"][0]["row_id"]);
    assert_eq!(groups[1]["row_id"], replay["entries_groups"][1]["row_id"]);
}
