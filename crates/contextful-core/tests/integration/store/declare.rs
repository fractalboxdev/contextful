//! `store.declare`: the table block, its defaults and its checks.

use super::{run, snapshot};
use contextful_core::store::declare::{TableDecl, WriteMode, DEFAULT_RETAIN_RUNS_SECS};
use contextful_core::store::reconcile::{Column, ColumnType, FloatItem, Schema};
use contextful_core::store::resolve::TableState;
use contextful_core::store::StoreError;

const SPEC_EXAMPLE: &str = r#"
[[pipeline.tables]]
name         = "filings"
primary_key  = ["document_id", "page"]
order_by     = "revised_at"
write_mode   = "replace"
cluster_by   = ["issuer", "revised_at"]
partition_by = ["tenant"]
retain_runs  = "7d"
columns      = { digest = "binary(32)", embedding = "float16[768]" }

[pipeline.tables.valid_time]
from = "effective_from"
to   = "effective_to"
"#;

/// A table block declares any of `primary_key`, `order_by`, `write_mode`, `replicate`, `subject_id`, `class`, `policy`, `visibility`, `valid_time`, `view`, `cluster_by`, `partition_by`, `retain_runs`, `columns`, `indexes`, `agent_description`, `agent_hint` and `example_queries`; an unset key is absent from the canonical serialization.
// spec: store.declare.table-block@598aa993
#[test]
fn a_table_block_parses_its_keys_and_omits_unset_ones() {
    let t = &TableDecl::parse_pipeline(SPEC_EXAMPLE).unwrap()[0];
    assert_eq!(t.primary_key(), ["document_id", "page"]);
    assert_eq!(t.write_mode(), WriteMode::Replace);
    assert_eq!(t.cluster_by(), ["issuer", "revised_at"]);
    assert_eq!(t.partition_by(), ["tenant"]);
    assert_eq!(t.valid_time.as_ref().unwrap().to.as_deref(), Some("effective_to"));
    assert_eq!(t.retain_runs_secs().unwrap(), 7 * 86_400);
    assert_eq!(
        t.column_types(),
        [("digest".to_string(), ColumnType::FixedSizeBinary(32)), ("embedding".to_string(), ColumnType::FixedSizeList(FloatItem::Float16, 768))]
            .into_iter()
            .collect()
    );

    let canonical: serde_json::Value = serde_json::from_str(&t.canonical()).unwrap();
    let keys: Vec<&str> = canonical.as_object().unwrap().keys().map(String::as_str).collect();
    for unset in ["replicate", "subject_id", "class", "policy", "visibility", "view", "agent_description", "agent_hint", "example_queries"] {
        assert!(!keys.contains(&unset), "an unset `{unset}` appears in {keys:?}");
    }
    assert_eq!(TableDecl::named("bare").canonical(), r#"{"name":"bare"}"#);

    let every = r#"
[[pipeline.tables]]
name = "notes"
replicate = false
subject_id = "owner"
class = { owner = "email" }
policy = "owner_only"
visibility = { source = "notes", resource_key = "note_id", fidelity = "mirrored", family = "item-exception" }
view = "SELECT 1"
agent_description = "Meeting notes"
agent_hint = "Filter by owner"
example_queries = ["SELECT count(*) FROM notes"]
"#;
    let n = &TableDecl::parse_pipeline(every).unwrap()[0];
    assert_eq!(n.example_queries.as_ref().unwrap().len(), 1);
    assert!(TableDecl::parse_pipeline("[[pipeline.tables]]\nname = \"x\"\nprimary_keys = [\"id\"]\n").is_err());
    assert!(!TableDecl::named("bare").canonical().contains("columns"));
    assert!(!TableDecl::named("bare").canonical().contains("indexes"));
    let indexed = "[[pipeline.tables]]\nname = \"x\"\n[[pipeline.tables.indexes]]\nkind = \"vector\"\ncolumn = \"e\"\nid_column = \"id\"\nmodel = \"m\"\ndim = 2\n";
    assert_eq!(TableDecl::parse_pipeline(indexed).unwrap()[0].indexes().len(), 1);
    // A column type outside the spellings a landing reads refuses the declaration.
    assert!(TableDecl::parse_pipeline("[[pipeline.tables]]\nname = \"x\"\ncolumns = { digest = \"binary(0)\" }\n").is_err());
    assert!(TableDecl::parse_pipeline("[[pipeline.tables]]\nname = \"x\"\ncolumns = { v = \"float64[3]\" }\n").is_err());
}

/// `order_by` names the column picking the surviving row per key, and defaults to `_ingested_at`.
// spec: store.declare.order-by-default@141fbb95
#[test]
fn order_by_defaults_to_ingested_at() {
    assert_eq!(TableDecl::named("t").order_by(), "_ingested_at");
    assert_eq!(TableDecl::parse_pipeline(SPEC_EXAMPLE).unwrap()[0].order_by(), "revised_at");
}

/// An `order_by` naming a column neither declared nor injected raises `StoreOrderByUnknownColumn` at validation, before the first batch.
// spec: store.declare.order-by-unknown@374f5264
#[test]
fn an_order_by_naming_no_column_is_refused() {
    let schema = Schema { columns: vec![Column::new("revised_at", ColumnType::Int64, true)] };
    let mut t = TableDecl::named("filings");
    t.order_by = Some("revised_on".into());
    match t.validate(&schema) {
        Err(StoreError::StoreOrderByUnknownColumn(m)) => assert!(m.contains("revised_on") && m.contains("filings"), "{m}"),
        other => panic!("expected StoreOrderByUnknownColumn, got {other:?}"),
    }
    t.order_by = Some("revised_at".into());
    t.validate(&schema).unwrap();
    t.order_by = Some("_run_id".into());
    t.validate(&schema).unwrap();
}

/// `write_mode` is `append`, the default, keeping the last write per key and retiring no key, or `replace`.
// spec: store.declare.write-mode@b9f467b9
#[test]
fn write_mode_is_append_by_default_or_replace() {
    assert_eq!(TableDecl::named("t").write_mode(), WriteMode::Append);
    assert!(TableDecl::parse_pipeline("[[pipeline.tables]]\nname = \"x\"\nwrite_mode = \"upsert\"\n").is_err());
    assert_eq!(TableDecl::named("t").retain_runs_secs().unwrap(), DEFAULT_RETAIN_RUNS_SECS);
}

/// A `retain_runs` the grammar does not admit is refused, whatever bytes it carries: the
/// unit is read as a character, so a multi-byte suffix raises `DeclarationMalformed`
/// rather than splitting the value mid-character.
#[test]
fn a_retain_runs_outside_the_grammar_is_refused_not_panicked_on() {
    // `parse_pipeline` reads the window too, so either it or `retain_runs_secs` refuses.
    let secs = |v: &str| {
        TableDecl::parse_pipeline(&format!("[[pipeline.tables]]\nname = \"t\"\nretain_runs = \"{v}\"\n"))
            .map_err(|e| e.to_string())
            .and_then(|mut d| d.remove(0).retain_runs_secs().map_err(|e| e.to_string()))
    };
    assert_eq!(secs("90s").unwrap(), 90);
    assert_eq!(secs("12h").unwrap(), 12 * 3_600);
    for bad in ["7天", "", "d", "7x", "7 d", "seven d", "٧d"] {
        assert!(secs(bad).is_err(), "`{bad}` was accepted as a retain_runs window");
    }
}

/// Under `replace`, a read covers the newest run carrying the source's complete state plus every run committed after it.
// spec: store.declare.replace-frontier@1ddd0474
#[test]
fn replace_covers_the_newest_complete_run_and_what_follows() {
    let s0 = snapshot("2030-01-01T00:30:00Z", None, &["run-1"]);
    let state = TableState {
        table: "filings".into(),
        write_mode: WriteMode::Replace,
        runs: vec![
            run("run-1", "2030-01-01T00:00:00Z", 1),
            run("run-2", "2030-01-01T01:00:00Z", 1),
            run("run-3", "2030-01-01T02:00:00Z", 1),
            run("run-4", "2030-01-01T03:00:00Z", 0),
        ],
        chain: vec![s0],
        history_collected: false,
    };
    let r = state.resolve(None).unwrap();
    assert!(r.snapshot.is_none());
    assert_eq!(r.runs.iter().map(|r| r.run_id.as_str()).collect::<Vec<_>>(), ["run-3", "run-4"]);
    assert_eq!(r.files(), ["data/runs/run-3/ingest-a/part-00000.parquet"]);

    // With every run folded, the snapshot is the complete state.
    let folded = TableState { runs: state.runs[..1].to_vec(), ..state.clone() };
    assert!(folded.resolve(None).unwrap().snapshot.is_some());

    // A zero-row run replaces nothing.
    let empty_only =
        TableState { runs: vec![run("run-1", "2030-01-01T00:00:00Z", 1), run("run-5", "2030-01-01T04:00:00Z", 0)], ..state };
    assert!(empty_only.resolve(None).unwrap().snapshot.is_some());
}
