//! Declared output tables are readable before any job has landed its first batch.

use super::*;

fn manifest(columns: &str) -> String {
    format!("{MANIFEST}\n[[pipeline.tables]]\nname = \"research/output\"\nprimary_key = [\"source_run\"]\ncolumns = {{ {columns} }}\n")
}

#[test]
fn cold_declared_columns_support_the_first_output_antijoin_without_a_write() {
    let r = Reads::with_manifest(&manifest("source_run = 'Utf8', observed_at = 'Timestamp', count = 'Int64'"));
    let s = r.session(&["research/*"], None, Some("public-cloud:us-east-1"));
    let empty = r.query(&s, r#"SELECT source_run, observed_at, count, _run_id FROM "research/output""#).unwrap();
    assert!(empty.rows.is_empty() && !empty.truncated);
    assert_eq!(empty.columns, ["source_run", "observed_at", "count", "_run_id"]);
    let pending = r.query(&s, r#"SELECT v.item_id FROM "research/vendor" v WHERE NOT EXISTS (SELECT 1 FROM "research/output" o WHERE o.source_run = v.item_id)"#).unwrap();
    assert_eq!(column(&pending, "item_id"), [json!("v1")]);
    assert!(r.store.try_schema("research/output").unwrap().is_none(), "a read creates no stored schema");
}

#[test]
fn cold_declared_injected_names_keep_the_engine_types() {
    let r = Reads::with_manifest(&manifest("source_run = 'Utf8', _run_id = 'Int64', _site_id = 'Bool'"));
    let s = r.session(&["research/output"], None, None);
    let response = r.query(&s, r#"SELECT length(_run_id), length(_site_id), source_run FROM "research/output""#).unwrap();
    assert!(response.rows.is_empty());
    assert!(r.store.try_schema("research/output").unwrap().is_none());
}

#[test]
fn cold_declared_reserved_producer_columns_refuse() {
    let r = Reads::with_manifest(&manifest("source_run = 'Utf8', _invented_engine_column = 'Utf8'"));
    let authority = r.authority(loop_subject("agent://research-loop"), vec![read(&["research/output"], None)]);
    let result = r.face.session(&authority, &Request::default(), Bounds::default());
    let error = result.err().expect("reserved producer declaration must refuse");
    assert!(error.to_string().contains("StoreReservedColumnName"), "{error}");
}

#[test]
fn cold_declared_columns_hold_masks_to_their_types() {
    let declaration = format!("{}\n[pipeline.tables.policy.columns]\npayload = {{ strategy = 'truncate:3' }}\n", manifest("source_run = 'Utf8', payload = 'Binary'"));
    let r = Reads::with_manifest(&declaration);
    let authority = r.authority(loop_subject("agent://research-loop"), vec![read(&["research/output"], None)]);
    let result = r.face.session(&authority, &Request::default(), Bounds::default());
    let error = result.err().expect("a known binary column cannot carry a text mask");
    assert!(error.to_string().contains("StrategyOutsideType"), "{error}");
}

#[test]
fn a_landed_table_does_not_invent_columns_from_later_declarations() {
    let manifest = MANIFEST.replace("name = \"research/vendor\"", "name = \"research/vendor\"\ncolumns = { future_column = 'Utf8' }");
    let r = Reads::with_manifest(&manifest);
    let s = r.session(&["research/vendor"], None, Some("public-cloud:us-east-1"));
    let rows = r.query(&s, r#"SELECT item_id FROM "research/vendor""#).unwrap();
    assert_eq!(column(&rows, "item_id"), [json!("v1")]);
    assert!(r.query(&s, r#"SELECT future_column FROM "research/vendor""#).is_err());
}
