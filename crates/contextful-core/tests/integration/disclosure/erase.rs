use contextful_core::disclosure::erase::{select_subject, RetainedRows};
use contextful_core::store::declare::TableDecl;
use serde_json::json;
use std::collections::BTreeMap;

#[test]
fn column_scope_recognizes_composite_members_without_admitting_legacy_partial_keys() {
    use contextful_core::disclosure::erase::{declares_erasure_column, select_keys};
    let declarations = TableDecl::parse_pipeline(r#"
[[pipeline.tables]]
name = "events"
primary_key = ["trace_id", "id"]
"#).unwrap();
    assert!(declares_erasure_column(&declarations[0], "trace_id"), "a composite primary member is a declared column");
    let rows = BTreeMap::from([("events".into(), vec![json!({"trace_id":"trace-a","id":"a"}).as_object().unwrap().clone()])]);
    let keys = BTreeMap::from([("events".into(), vec![json!({"trace_id":"trace-a"}).as_object().unwrap().clone()])]);
    assert!(select_keys(&declarations, &rows, &keys).unwrap_err().to_string().starts_with("ErasureScopeUnsupported"), "the table-key format still requires the complete composite key");
}

#[test]
fn keyset_accepts_an_explicitly_declared_column_without_subject_or_erasure_metadata() {
    use contextful_core::disclosure::erase::select_keys;
    let declarations = TableDecl::parse_pipeline(r#"
[[pipeline.tables]]
name = "events"
primary_key = ["id"]
columns = { id = "utf8", trace_id = "utf8" }
"#).unwrap();
    let objects = |value: serde_json::Value| value.as_array().unwrap().iter().map(|row| row.as_object().unwrap().clone()).collect();
    let rows = BTreeMap::from([("events".into(), objects(json!([
        {"id":"a","trace_id":"erase-trace"}, {"id":"b","trace_id":"keep-trace"}
    ])))]);
    let keys = BTreeMap::from([("events".into(), objects(json!([{"trace_id":"erase-trace"}])))]);
    let selected = select_keys(&declarations, &rows, &keys).unwrap();
    assert_eq!(selected.removes("events"), &[true, false]);
    for selector in [json!({"unknown":"erase-trace"}), json!({"trace_id":null})] {
        let keys = BTreeMap::from([("events".into(), vec![selector.as_object().unwrap().clone()])]);
        assert!(select_keys(&declarations, &rows, &keys).unwrap_err().to_string().starts_with("ErasureScopeUnsupported"));
    }
}

#[test]
fn subject_erasure_keeps_shared_references_and_surviving_citations() {
    let declarations = TableDecl::parse_pipeline(r#"
[[pipeline.tables]]
name = "notes"
subject_id = "subject"
erasure_key = "id"
[[pipeline.tables]]
name = "blobs"
erasure_key = "digest"
referenced_by = [{ table = "notes", column = "blob" }]
[[pipeline.tables]]
name = "citations"
on_erase = "survive"
"#).unwrap();
    let mut rows: RetainedRows = BTreeMap::new();
    let objects = |value: serde_json::Value| value.as_array().unwrap().iter().map(|v| v.as_object().unwrap().clone()).collect();
    rows.insert("notes".into(), objects(json!([
        {"id":"a1","subject":"alice","blob":"shared"},
        {"id":"a2","subject":"alice","blob":"unique"},
        {"id":"b1","subject":"bob","blob":"shared"}
    ])));
    rows.insert("blobs".into(), objects(json!([{"digest":"shared"},{"digest":"unique"}])));
    rows.insert("citations".into(), objects(json!([{"id":"c1","source_key":"a2"}])));
    let selected = select_subject(&declarations, &rows, &["notes".to_string()], "alice").unwrap();
    assert_eq!(selected.removes("notes"), &[true, true, false]);
    assert_eq!(selected.removes("blobs"), &[false, true]);
    assert_eq!(selected.removes("citations"), &[false]);
    assert_eq!(selected.affected_tables().collect::<Vec<_>>(), vec!["blobs", "notes"]);
    assert!(!format!("{selected:?}").contains("alice"));
}

#[test]
fn subject_erasure_refuses_an_incomplete_declared_reference_universe() {
    let declarations = TableDecl::parse_pipeline(r#"
[[pipeline.tables]]
name = "notes"
subject_id = "subject"
[[pipeline.tables]]
name = "blobs"
erasure_key = "digest"
referenced_by = [{ table = "missing", column = "blob" }]
"#).unwrap();
    let rows = BTreeMap::from([("notes".to_string(), vec![]), ("blobs".to_string(), vec![])]);
    let error = select_subject(&declarations, &rows, &["notes".to_string()], "alice").unwrap_err();
    assert!(error.to_string().starts_with("ErasureScopeUnsupported"));
    assert!(!error.to_string().contains("alice"));
}

#[test]
fn erasure_selection_refuses_an_undeclared_subject_and_a_cascade_beyond_the_bound() {
    use contextful_core::store::declare::ErasureReference;
    let mut declarations = vec![TableDecl { name: "notes".into(), subject_id: Some("subject".into()), ..TableDecl::default() }];
    let mut rows = BTreeMap::from([("notes".into(), vec![json!({"subject":"alice","reference":"k0"}).as_object().unwrap().clone()])]);
    for depth in 1..=17 {
        let parent = if depth == 1 { "notes".into() } else { format!("d{}", depth-1) };
        let name = format!("d{depth}");
        declarations.push(TableDecl { name: name.clone(), erasure_key: Some("key".into()), referenced_by: Some(vec![ErasureReference { table: parent, column: "reference".into() }]), ..TableDecl::default() });
        rows.insert(name, vec![json!({"key":format!("k{}",depth-1),"reference":format!("k{depth}")}).as_object().unwrap().clone()]);
    }
    let error = select_subject(&declarations, &rows, &["notes".into()], "alice").unwrap_err();
    assert!(error.to_string().starts_with("ErasureCascadeUnbounded"), "{error}");
    declarations.pop(); rows.remove("d17");
    assert!(select_subject(&declarations, &rows, &["notes".into()], "alice").unwrap().removes("d16")[0]);
    declarations[0].subject_id = None;
    let error = select_subject(&declarations, &rows, &["notes".into()], "alice").unwrap_err();
    assert!(error.to_string().starts_with("ErasureSubjectUndeclared"), "{error}");
}

#[test]
fn keyset_erasure_matches_complete_declared_keys_and_preserves_shared_references() {
    use contextful_core::disclosure::erase::select_keys;
    let declarations = TableDecl::parse_pipeline(r#"
[[pipeline.tables]]
name = "notes"
primary_key = ["tenant", "id"]
erasure_key = "id"
[[pipeline.tables]]
name = "blobs"
erasure_key = "digest"
referenced_by = [{ table = "notes", column = "blob" }]
"#).unwrap();
    let objects = |value: serde_json::Value| value.as_array().unwrap().iter().map(|value| value.as_object().unwrap().clone()).collect();
    let rows = BTreeMap::from([
        ("notes".into(), objects(json!([{"tenant":"a","id":"x","blob":"shared"},{"tenant":"b","id":"x","blob":"shared"},{"tenant":"a","id":"y","blob":"unique"}]))),
        ("blobs".into(), objects(json!([{"digest":"shared"},{"digest":"unique"}]))),
    ]);
    let keys = BTreeMap::from([("notes".into(), objects(json!([{"tenant":"a","id":"x"},{"tenant":"a","id":"y"}])))]);
    let selected = select_keys(&declarations, &rows, &keys).unwrap();
    assert_eq!(selected.removes("notes"), &[true, false, true]);
    assert_eq!(selected.removes("blobs"), &[false, true]);
    let partial = BTreeMap::from([("notes".into(), objects(json!([{"tenant":"a"}])))]);
    let error = select_keys(&declarations, &rows, &partial).unwrap_err();
    assert!(error.to_string().starts_with("ErasureScopeUnsupported"));
    assert!(!format!("{selected:?}").contains("shared"));
}
