use contextful_core::disclosure::erase::{select_subject, RetainedRows};
use contextful_core::store::declare::TableDecl;
use serde_json::json;
use std::collections::BTreeMap;

#[test]
fn canonical_declaration_set_includes_typed_memory_shape_without_inferred_columns() {
    let tables = TableDecl::parse_declaration_set(r#"
[[pipeline.tables]]
name = "notes"
columns = {subject="utf8"}
[[table]]
name = "memory/facts"
shape = "memory_facts"
columns = ["claim_id","subject","predicate","object","scope","tier","confidence","valid_from","valid_to","evidence","superseded_by","grant_id","agent","extra_name"]
"#, &[]).unwrap();
    let memory = tables.iter().find(|table| table.name == "memory/facts").expect("the canonical declaration universe omits a memory shape");
    assert_eq!(memory.primary_key(), &["claim_id"]);
    assert!(memory.column_types().is_empty(), "name-only memory declarations invent column types");
}

#[test]
fn memory_declaration_collisions_refuse_and_ordinary_multiplicity_remains() {
    let memory = |name: &str| format!(r#"[[table]]
name = "{name}"
shape = "memory_preferences"
columns = ["preference_id","subject","key","value","scope"]
"#);
    let ordinary = "[[pipeline.tables]]\nname = \"memory/preferences\"\n";
    for declaration in [format!("{ordinary}{}", memory("memory/preferences")), format!("{}{}", memory("memory/preferences"), memory("memory/preferences"))] {
        let refusal = TableDecl::parse_declaration_set(&declaration, &[]).unwrap_err();
        assert!(refusal.to_string().contains("declared more than once"), "the established duplicate refusal changes: {refusal}");
    }
    for declaration in [memory("memory//preferences"), memory("memory/./preferences")] {
        assert!(TableDecl::parse_declaration_set(&declaration, &[]).is_err(), "an ambiguous memory identity is admitted");
    }
    let tables = TableDecl::parse_declaration_set("[[pipeline.tables]]\nname = \"notes\"\n[[pipeline.tables]]\nname = \"notes\"\n", &[]).unwrap();
    assert_eq!(tables.len(), 2, "the canonical pack replaces existing ordinary multiplicity policy");
}

#[test]
fn canonical_memory_names_reach_column_selection_without_invented_types() {
    use contextful_core::{disclosure::erase::{column_key_set_declared, select_column_keys_declared}, store::declare::DeclarationSet};
    let pack = DeclarationSet::parse(r#"[[table]]
name = "memory/preferences"
shape = "memory_preferences"
columns = ["preference_id","subject","key","value","scope","extra_name"]
"#, &[]).unwrap();
    assert!(pack.tables()[0].column_types().is_empty());
    let values = [json!("erased")];
    assert_eq!(column_key_set_declared(&pack, "extra_name", &values).unwrap().keys().collect::<Vec<_>>(), [&"memory/preferences".to_string()]);
    let rows = BTreeMap::from([("memory/preferences".to_string(), vec![json!({"preference_id":"p1","subject":"erased"}).as_object().unwrap().clone(), json!({"preference_id":"p2","subject":"kept"}).as_object().unwrap().clone()])]);
    let selected = select_column_keys_declared(&pack, &rows, "subject", &values).unwrap();
    assert_eq!(selected.removes("memory/preferences"), [true, false]);
    assert!(contextful_core::disclosure::erase::column_key_set(pack.tables(), "subject", &values).is_err(), "a bare typed table invents name-only columns");
}

#[test]
fn column_scope_uses_structural_declarations() {
    use contextful_core::disclosure::erase::declares_erasure_column;
    for role in [
        "subject_id = \"trace_id\"", "partition_by = [\"trace_id\"]", "order_by = \"trace_id\"",
        "cluster_by = [\"trace_id\"]", "content_hash_column = \"trace_id\"",
        "valid_time = { from = \"trace_id\", to = \"end\" }", "valid_time = { from = \"start\", to = \"trace_id\" }",
        "retain_rows = { column = \"trace_id\", age = \"1d\" }",
        "indexes = [{ kind = \"fulltext\", column = \"trace_id\", id_column = \"id\" }]",
        "indexes = [{ kind = \"fulltext\", column = \"text\", id_column = \"trace_id\" }]",
    ] {
        let declarations = TableDecl::parse_pipeline(&format!("[[pipeline.tables]]\nname = \"events\"\n{role}\n")).unwrap();
        assert!(declares_erasure_column(&declarations[0], "trace_id"), "structural role omits its declared column: {role}");
    }
}

#[test]
fn column_scope_preserves_source_reference_ownership_and_excludes_descriptive_keys() {
    use contextful_core::disclosure::erase::column_key_set;
    let declarations = TableDecl::parse_pipeline(r#"
[[pipeline.tables]]
name = "events"
primary_key = ["id"]
[[pipeline.tables]]
name = "blobs"
erasure_key = "digest"
referenced_by = [{table = "events", column = "trace_id"}]
[[pipeline.tables]]
name = "descriptive"
primary_key = ["id"]
column_hints = { trace_id = "a hint, not a schema declaration" }
policy = { trace_id = "a policy key, not a schema declaration" }
agent_hint = "trace_id"
example_queries = ["SELECT trace_id FROM descriptive"]
"#).unwrap();
    let roots = column_key_set(&declarations, "trace_id", &[json!("erase-trace")]).unwrap();
    assert_eq!(roots.keys().map(String::as_str).collect::<Vec<_>>(), vec!["events"], "the reference names a source column, not a target or descriptive column");
    assert!(column_key_set(&declarations, "_ingested_at", &[json!("erase-trace")]).is_err(), "implicit defaults create no declared column scope");
}

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
fn explicit_column_declaration_does_not_widen_table_key_selection() {
    use contextful_core::disclosure::erase::{declares_erasure_column, select_keys};
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
    assert!(declares_erasure_column(&declarations[0], "trace_id"));
    assert!(select_keys(&declarations, &rows, &keys).is_err(), "explicit column declarations do not widen the table-key format");
    for selector in [json!({"unknown":"erase-trace"}), json!({"trace_id":null})] {
        let keys = BTreeMap::from([("events".into(), vec![selector.as_object().unwrap().clone()])]);
        assert!(select_keys(&declarations, &rows, &keys).unwrap_err().to_string().starts_with("ErasureScopeUnsupported"));
    }
}

fn objects(value: serde_json::Value) -> Vec<serde_json::Map<String, serde_json::Value>> {
    value.as_array().unwrap().iter().map(|v| v.as_object().unwrap().clone()).collect()
}

/// A shared digest survives while any surviving declared reference names it; erasure collects a digest only after its last surviving reference disappears.
// spec: disclosure.erase.shared-references@e6601f5f
#[test]
fn a_shared_digest_survives_while_a_surviving_reference_names_it() {
    let declarations = TableDecl::parse_pipeline(r#"
[[pipeline.tables]]
name = "notes"
subject_id = "subject"
erasure_key = "id"
[[pipeline.tables]]
name = "blobs"
erasure_key = "digest"
referenced_by = [{ table = "notes", column = "blob" }]
"#).unwrap();
    let rows: RetainedRows = BTreeMap::from([
        ("notes".into(), objects(json!([
            {"id":"a1","subject":"alice","blob":"shared"},
            {"id":"a2","subject":"alice","blob":"unique"},
            {"id":"b1","subject":"bob","blob":"shared"}
        ]))),
        ("blobs".into(), objects(json!([{"digest":"shared"},{"digest":"unique"}]))),
    ]);
    let selected = select_subject(&declarations, &rows, &["notes".to_string()], "alice").unwrap();
    assert_eq!(selected.removes("notes"), &[true, true, false]);
    assert_eq!(selected.removes("blobs"), &[false, true], "a digest a surviving row names is collected, or an orphaned one survives");
    assert_eq!(selected.affected_tables().collect::<Vec<_>>(), vec!["blobs", "notes"]);
    assert!(!format!("{selected:?}").contains("alice"));
    let selected = select_subject(&declarations, &rows, &["notes".to_string()], "bob").unwrap();
    assert_eq!(selected.removes("blobs"), &[false, false], "a digest alice's rows still name is collected");
}

/// A citing table carrying the survival declaration of {{store.declare.erasure-metadata}} retains its rows; its references resolve through {{read.reference.erased-verdict}}.
// spec: disclosure.erase.surviving-citations@c7d94ad8
#[test]
fn a_survival_table_keeps_rows_whose_references_reach_erased_rows() {
    let declaration = |survival: &str| TableDecl::parse_pipeline(&format!(r#"
[[pipeline.tables]]
name = "notes"
subject_id = "subject"
erasure_key = "id"
[[pipeline.tables]]
name = "citations"
erasure_key = "id"
referenced_by = [{{ table = "notes", column = "citation" }}]
{survival}
"#)).unwrap();
    let rows: RetainedRows = BTreeMap::from([
        ("notes".into(), objects(json!([{"id":"a1","subject":"alice","citation":"c1"},{"id":"b1","subject":"bob","citation":"c2"}]))),
        ("citations".into(), objects(json!([{"id":"c1","score":0.9},{"id":"c2","score":0.4}]))),
    ]);
    let collected = select_subject(&declaration(""), &rows, &["notes".to_string()], "alice").unwrap();
    assert_eq!(collected.removes("citations"), &[true, false], "the fixture's citation is not exclusively reached by the erased row");
    let survived = select_subject(&declaration("on_erase = \"survive\""), &rows, &["notes".to_string()], "alice").unwrap();
    assert_eq!(survived.removes("notes"), &[true, false]);
    assert_eq!(survived.removes("citations"), &[false, false], "a survival declaration still cascades");
    assert_eq!(survived.affected_tables().collect::<Vec<_>>(), vec!["notes"]);
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

/// A subject table and a chain of `depth` derived tables, each holding one row on alice's chain and
/// one on bob's, each row referenced only by its parent.
fn cascade_chain(depth: usize) -> (Vec<TableDecl>, RetainedRows) {
    use contextful_core::store::declare::ErasureReference;
    let mut declarations = vec![TableDecl { name: "notes".into(), subject_id: Some("subject".into()), ..TableDecl::default() }];
    let mut rows: RetainedRows = BTreeMap::from([("notes".into(), objects(json!([{"subject":"alice","reference":"k0"},{"subject":"bob","reference":"kept0"}])))]);
    for hop in 1..=depth {
        let parent = if hop == 1 { "notes".into() } else { format!("d{}", hop - 1) };
        let name = format!("d{hop}");
        declarations.push(TableDecl { name: name.clone(), erasure_key: Some("key".into()), referenced_by: Some(vec![ErasureReference { table: parent, column: "reference".into() }]), ..TableDecl::default() });
        rows.insert(name, objects(json!([{"key":format!("k{}", hop - 1),"reference":format!("k{hop}")},{"key":format!("kept{}", hop - 1),"reference":format!("kept{hop}")}])));
    }
    (declarations, rows)
}

/// In the same operation the cascade invalidates every derived fact whose provenance reaches the subject key or a tombstoned identifier within 16 hops.
// spec: disclosure.erase.cascade@562ca33f
#[test]
fn the_cascade_reaches_an_exclusively_referenced_fact_16_hops_away() {
    let (declarations, rows) = cascade_chain(16);
    let selected = select_subject(&declarations, &rows, &["notes".into()], "alice").unwrap();
    assert_eq!(selected.removes("notes"), &[true, false]);
    for hop in 1..=16 {
        assert_eq!(selected.removes(&format!("d{hop}")), &[true, false], "the cascade stops before hop {hop} or leaves its provenance");
    }
}

/// A provenance chain deeper than the cascade bound raises `ErasureCascadeUnbounded`, and the erasure commits nothing.
// spec: disclosure.erase.cascade-unbounded@48200744
#[test]
fn a_cascade_deeper_than_16_hops_raises_erasure_cascade_unbounded() {
    let (declarations, rows) = cascade_chain(17);
    let error = select_subject(&declarations, &rows, &["notes".into()], "alice").unwrap_err();
    assert!(error.to_string().starts_with("ErasureCascadeUnbounded"), "{error}");
    assert!(!error.to_string().contains("alice"));
}

/// A table names its subject column as `subject_id = "<column>"` under `[[pipeline.tables]]`. A subject erasure against a table without one raises `ErasureSubjectUndeclared`, naming the table.
// spec: disclosure.erase.subject-column@5265feaf
#[test]
fn a_subject_erasure_against_a_table_without_a_subject_column_raises_erasure_subject_undeclared() {
    let declarations = TableDecl::parse_pipeline("[[pipeline.tables]]\nname = \"notes\"\nsubject_id = \"subject\"\n[[pipeline.tables]]\nname = \"events\"\n").unwrap();
    let rows: RetainedRows = BTreeMap::from([("notes".into(), objects(json!([{"subject":"alice"}]))), ("events".into(), objects(json!([{"subject":"alice"}])))]);
    assert!(select_subject(&declarations, &rows, &["notes".into()], "alice").unwrap().removes("notes")[0]);
    let error = select_subject(&declarations, &rows, &["notes".into(), "events".into()], "alice").unwrap_err();
    assert!(error.to_string().starts_with("ErasureSubjectUndeclared"), "{error}");
    assert!(error.to_string().contains("`events`"), "the refusal omits the undeclared table: {error}");
    assert!(!error.to_string().contains("alice"));
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

fn tenant_pack(declaration: &str) -> Vec<TableDecl> {
    TableDecl::parse_declaration_set(declaration, &[]).unwrap()
}

/// A tenant column of a non-string type raises `PurgeTenantColumnType`.
// spec: disclosure.erase.tenant-column-type@6776f6b7
#[test]
fn a_tenant_column_of_a_non_string_type_raises_purge_tenant_column_type() {
    use contextful_core::disclosure::erase::{tenant_columns, ErasureError};
    use contextful_core::store::reconcile::ColumnType;
    let declared = tenant_pack("[[pipeline.tables]]\nname = \"orders\"\npartition_by = [\"tenant\"]\ncolumns = {tenant=\"int64\"}\n");
    assert_eq!(tenant_columns(&declared, &BTreeMap::new()).unwrap_err(), ErasureError::PurgeTenantColumnType("orders".into()));
    let reconciled = tenant_pack("[[pipeline.tables]]\nname = \"orders\"\npartition_by = [\"tenant\"]\n");
    let types = BTreeMap::from([("orders".to_string(), BTreeMap::from([("tenant".to_string(), ColumnType::Timestamp)]))]);
    assert_eq!(tenant_columns(&reconciled, &types).unwrap_err(), ErasureError::PurgeTenantColumnType("orders".into()));
    let text = BTreeMap::from([("orders".to_string(), BTreeMap::from([("tenant".to_string(), ColumnType::Utf8)]))]);
    assert_eq!(tenant_columns(&reconciled, &text).unwrap(), BTreeMap::from([("orders".to_string(), "tenant".to_string())]));
}

/// A purge finding no model declaring an outermost partition key raises `PurgeTenantUndeclared`.
// spec: disclosure.erase.tenant-undeclared@3e6810bb
#[test]
fn a_purge_finding_no_outermost_partition_key_raises_purge_tenant_undeclared() {
    use contextful_core::disclosure::erase::{tenant_columns, ErasureError};
    let unpartitioned = tenant_pack("[[pipeline.tables]]\nname = \"orders\"\n[[pipeline.tables]]\nname = \"events\"\ncluster_by = [\"tenant\"]\n");
    assert_eq!(tenant_columns(&unpartitioned, &BTreeMap::new()).unwrap_err(), ErasureError::PurgeTenantUndeclared);
    let mixed = tenant_pack("[[pipeline.tables]]\nname = \"orders\"\n[[pipeline.tables]]\nname = \"events\"\npartition_by = [\"org\", \"day\"]\n");
    assert_eq!(tenant_columns(&mixed, &BTreeMap::new()).unwrap(), BTreeMap::from([("events".to_string(), "org".to_string())]));
}

#[test]
fn a_tenant_selection_removes_matching_rows_from_every_partitioned_table() {
    use contextful_core::disclosure::erase::select_tenant;
    let tables = tenant_pack("[[pipeline.tables]]\nname = \"orders\"\npartition_by = [\"tenant\"]\n[[pipeline.tables]]\nname = \"events\"\npartition_by = [\"org\"]\n[[pipeline.tables]]\nname = \"global\"\n");
    let columns = BTreeMap::from([("orders".to_string(), "tenant".to_string()), ("events".to_string(), "org".to_string())]);
    let rows: RetainedRows = BTreeMap::from([
        ("orders".to_string(), objects(json!([{"tenant":"acme"}, {"tenant":"globex"}, {"tenant":"ACME"}]))),
        ("events".to_string(), objects(json!([{"org":"globex"}, {"org":"acme"}]))),
        ("global".to_string(), objects(json!([{"tenant":"acme"}]))),
    ]);
    let selection = select_tenant(&tables, &rows, &columns, "acme").unwrap();
    assert_eq!(selection.removes("orders"), &[true, false, false]);
    assert_eq!(selection.removes("events"), &[false, true]);
    assert_eq!(selection.removes("global"), &[false]);
    assert!(select_tenant(&tables, &rows, &columns, "").is_err(), "an empty tenant identifier selects nothing");
    let surviving = tenant_pack("[[pipeline.tables]]\nname = \"ledger\"\npartition_by = [\"tenant\"]\non_erase = \"survive\"\n");
    let columns = BTreeMap::from([("ledger".to_string(), "tenant".to_string())]);
    let rows: RetainedRows = BTreeMap::from([("ledger".to_string(), objects(json!([{"tenant":"acme"}, {"tenant":"globex"}])))]);
    let selection = select_tenant(&surviving, &rows, &columns, "acme").unwrap();
    assert_eq!(selection.removes("ledger"), &[true, false], "a survival declaration keeps a tenant's own rows through its purge");
}

/// A dead letter citing a removed row joins the selection; one citing a kept row, or
/// citing nothing, stays; evidence that does not decode refuses.
// spec: disclosure.erase.dead-letters@e25d1f2a
#[test]
fn a_dead_letter_citing_a_removed_row_joins_the_selection() {
    use contextful_core::disclosure::erase::{select_dead_letters, ErasureError};
    let notes = TableDecl::parse_pipeline("[[pipeline.tables]]\nname = \"notes\"\nsubject_id = \"subject\"\n").unwrap();
    let row = |value: serde_json::Value| value.as_object().unwrap().clone();
    let cite = |seq: i64| json!([{ "table": "notes", "run": "run-1", "seq": seq }]).to_string();
    let letters = vec!["notes_dead_letter".to_string()];
    let mut rows: RetainedRows = BTreeMap::from([
        ("notes".to_string(), vec![row(json!({"subject": "alice", "_run_id": "run-1", "_row_seq": 0})), row(json!({"subject": "bob", "_run_id": "run-1", "_row_seq": 1}))]),
        ("notes_dead_letter".to_string(), vec![row(json!({"response": "alice's", "evidence": cite(0)})), row(json!({"response": "bob's", "evidence": cite(1)})), row(json!({"response": "uncited"}))]),
    ]);
    let mut selected = select_subject(&notes, &rows, &["notes".to_string()], "alice").unwrap();
    select_dead_letters(&mut selected, &rows, &letters).unwrap();
    assert_eq!(selected.removes("notes_dead_letter"), [true, false, false]);
    assert_eq!(selected.affected_tables().collect::<Vec<_>>(), ["notes", "notes_dead_letter"]);
    rows.get_mut("notes_dead_letter").unwrap().push(row(json!({"response": "garbled", "evidence": "not json"})));
    let mut selected = select_subject(&notes, &rows, &["notes".to_string()], "alice").unwrap();
    assert!(matches!(select_dead_letters(&mut selected, &rows, &letters), Err(ErasureError::ErasureScopeUnsupported(_))));
}

/// A dead letter carrying no `evidence`, absent or null, joins the selection beside the
/// letters a subject's evidence reaches; a letter carrying evidence, and every letter of
/// a table outside the scope, stays. The count holds the evidence-less rows per table.
// spec: disclosure.erase.dead-letters-without-evidence@691172f7
#[test]
fn a_dead_letter_without_evidence_joins_the_selection() {
    use contextful_core::disclosure::erase::{select_dead_letters, select_dead_letters_without_evidence, ErasureSelection};
    let notes = TableDecl::parse_pipeline("[[pipeline.tables]]\nname = \"notes\"\nsubject_id = \"subject\"\n").unwrap();
    let row = |value: serde_json::Value| value.as_object().unwrap().clone();
    let cite = |seq: i64| json!([{ "table": "notes", "run": "run-1", "seq": seq }]).to_string();
    let rows: RetainedRows = BTreeMap::from([
        ("notes".to_string(), vec![row(json!({"subject": "alice", "_run_id": "run-1", "_row_seq": 0})), row(json!({"subject": "bob", "_run_id": "run-1", "_row_seq": 1}))]),
        ("notes_dead_letter".to_string(), vec![row(json!({"response": "alice's", "evidence": cite(0)})), row(json!({"response": "bob's", "evidence": cite(1)})),
            row(json!({"response": "written before evidence"})), row(json!({"response": "null evidence", "evidence": null}))]),
        ("calls_dead_letter".to_string(), vec![row(json!({"key": "input.csv", "reason": "unreadable"}))]),
    ]);
    let scope = vec!["notes_dead_letter".to_string(), "absent_dead_letter".to_string()];

    let mut alone = ErasureSelection::default();
    let counts = select_dead_letters_without_evidence(&mut alone, &rows, &scope);
    assert_eq!(alone.removes("notes_dead_letter"), [false, false, true, true]);
    assert!(alone.removes("calls_dead_letter").is_empty(), "a table outside the scope joins the selection");
    assert_eq!(counts, BTreeMap::from([("notes_dead_letter".to_string(), 2)]));

    let mut beside = select_subject(&notes, &rows, &["notes".to_string()], "alice").unwrap();
    select_dead_letters(&mut beside, &rows, &["notes_dead_letter".to_string()]).unwrap();
    let counts = select_dead_letters_without_evidence(&mut beside, &rows, &scope);
    assert_eq!(beside.removes("notes_dead_letter"), [true, false, true, true], "the purge keeps the letters the subject's evidence selects");
    assert_eq!(beside.removes("notes"), [true, false]);
    assert_eq!(counts, BTreeMap::from([("notes_dead_letter".to_string(), 2)]));
}
