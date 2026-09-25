//! `run.declare`: the specification, manifest reading, the content hash and table names.

use contextful_core::pipeline::canonical::canonical_json;
use contextful_core::pipeline::declare::{collect, read_manifest, table_name, ManifestFile, OnTableError, PipelineSpec, TableEntry};
use contextful_core::run::RunError;
use contextful_core::store::declare::{TableDecl, WriteMode};
use serde_json::json;

fn manifest(path: &str, text: &str) -> ManifestFile {
    ManifestFile { path: path.into(), text: text.into() }
}

fn spec(text: &str) -> PipelineSpec {
    read_manifest(&manifest("contextful.toml", text)).unwrap().remove(0).spec
}

const BASE: &str = "[[pipeline]]\nid = \"orders\"\n[pipeline.source]\nname = \"http\"\nconfig = { endpoint = \"https://api.example.test/v1\" }\n[[pipeline.tables]]\nname = \"items\"\n";

/// A manifest whose pipeline carries `top` keys and whose one table carries `table` keys.
fn doc(top: &str, table: &str) -> String {
    format!("[[pipeline]]\nid = \"orders\"\n{top}[pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"https://api.example.test/v1\" }}\n[[pipeline.tables]]\nname = \"items\"\n{table}")
}

/// A specification carries `id`, `source` and `tables`, plus the optional `destination`, `schedule`,
/// `incremental`, `transforms`, `redaction`, `normalize`, `backfill`, `seed`, `queries` and `on_table_error`.
// spec: run.declare.pipeline-spec@f5518a9e
#[test]
fn a_specification_carries_three_required_and_eleven_optional_keys() {
    let json = serde_json::json!({
        "id": "orders", "source": {"name": "http", "config": {"endpoint": "https://api.example.test/v1"}}, "tables": ["items"],
        "destination": {"name": "store"}, "schedule": "@hourly", "incremental": "updated_at", "transforms": [{"op": "select", "columns": ["id"]}],
        "redaction": {}, "normalize": {}, "backfill": {}, "seed": {}, "queries": {}, "on_table_error": "continue"
    });
    let s: PipelineSpec = serde_json::from_value(json).unwrap();
    assert_eq!(s.on_table_error(), OnTableError::Continue);
    assert_eq!((s.schedule.as_deref(), s.incremental.as_deref()), (Some("@hourly"), Some("updated_at")));
    assert!(s.redaction.is_some() && s.normalize.is_some() && s.backfill.is_some() && s.seed.is_some() && s.queries.is_some());
    for required in ["id", "source", "tables"] {
        let mut v = serde_json::to_value(&s).unwrap();
        v.as_object_mut().unwrap().remove(required);
        assert!(serde_json::from_value::<PipelineSpec>(v).is_err(), "{required} is required");
    }
    let mut extra = serde_json::to_value(&s).unwrap();
    extra["retries"] = json!(3);
    assert!(serde_json::from_value::<PipelineSpec>(extra).is_err(), "an unknown key refuses");
}

/// A `source` is a connector name beside a free-form JSON config object; a `destination` carries the same two
/// fields and defaults to the store.
// spec: run.declare.source-block@493201a1
#[test]
fn source_and_destination_are_a_name_and_a_config_object() {
    let s = spec(BASE);
    assert_eq!(s.source.name, "http");
    assert_eq!(s.source.config, json!({"endpoint": "https://api.example.test/v1"}));
    assert!(s.destination.is_none());
    assert!(s.validate().is_ok(), "an undeclared destination is the store");
    let with_dest = spec(&doc("destination = { name = \"store\", config = { root = \".\" } }\n", ""));
    assert_eq!(with_dest.destination.unwrap().config, json!({"root": "."}));
    let elsewhere = spec(&doc("destination = { name = \"warehouse\" }\n", ""));
    assert!(matches!(elsewhere.validate(), Err(RunError::PipelineUnknownDestination(_))));
}

/// `content_hash` is the sha256 of the specification's RFC 8785 canonical JSON with every optional field at its
/// default elided, so an explicit default hashes as its absence.
// spec: run.declare.content-hash@3c892df5
#[test]
fn an_explicit_default_hashes_as_its_absence() {
    let bare = spec(BASE);
    let explicit = spec(&doc("on_table_error = \"abort\"\ndestination = { name = \"store\" }\n", "write_mode = \"append\"\norder_by = \"_ingested_at\"\n"));
    assert_eq!(explicit.on_table_error, Some(OnTableError::Abort));
    assert_eq!(bare.content_hash(), explicit.content_hash());
    let moved = spec(&BASE.replace("name = \"items\"", "name = \"items\"\nprimary_key = [\"id\"]"));
    assert_ne!(bare.content_hash(), moved.content_hash());
    // The hash is over canonical JSON: member order is the canonical order, whatever the file's.
    let a: PipelineSpec = serde_json::from_str(r#"{"id":"orders","tables":["items"],"source":{"config":{"b":1,"a":2},"name":"http"}}"#).unwrap();
    let b: PipelineSpec = serde_json::from_str(r#"{"source":{"name":"http","config":{"a":2,"b":1}},"id":"orders","tables":["items"]}"#).unwrap();
    assert_eq!(a.content_hash(), b.content_hash());
    assert_eq!(canonical_json(&json!({"b": [1, 2.5, 1e21, 0.000001], "a": "é"})), r#"{"a":"é","b":[1,2.5,1e+21,0.000001]}"#);
    assert_eq!(a.content_hash(), contextful_core::run::journal::sha256_hex(canonical_json(&a.canonical_value()).as_bytes()));
}

/// A destination table is named `<pipeline id>_<table name>`, each non-alphanumeric character folded to `_` and
/// each ASCII uppercase letter lowered.
// spec: run.declare.table-name@5df3e9a0
#[test]
fn a_destination_table_folds_the_pipeline_and_table_names() {
    assert_eq!(table_name("meta-ads", "Insights"), "meta_ads_insights");
    assert_eq!(table_name("Orders.v2", "line items/eu"), "orders_v2_line_items_eu");
    assert_eq!(spec(BASE).table_name("items"), "orders_items");
    // The store reads the same names off the manifest.
    let decls = TableDecl::parse_pipeline(&BASE.replace("name = \"items\"", "name = \"Line Items\"")).unwrap();
    assert_eq!(decls[0].name, "orders_line_items");
}

/// A `tables` entry is a bare name or an object carrying that table's configuration; the two forms mix in one
/// array.
// spec: run.declare.table-entry@094d89af
#[test]
fn bare_names_and_table_blocks_mix() {
    let s: PipelineSpec = serde_json::from_value(json!({
        "id": "orders", "source": {"name": "http"},
        "tables": ["items", {"name": "refunds", "primary_key": ["id"], "write_mode": "replace"}]
    }))
    .unwrap();
    assert!(matches!(&s.tables[0], TableEntry::Name(n) if n == "items"));
    let refunds = s.tables[1].decl();
    assert_eq!((refunds.name.as_str(), refunds.primary_key(), refunds.write_mode()), ("refunds", &["id".to_string()][..], WriteMode::Replace));
    assert_eq!(s.tables[0].decl(), TableDecl::named("items"));
}

/// `replace` beside a `monotonic` or `opaque-token` cursor, a backfill chunk plan or a seed ceiling raises
/// `PipelineReplaceUnsupported`, naming the table.
// spec: run.declare.replace-unsupported@21e2ae04
#[test]
fn replace_beside_a_windowed_load_is_refused() {
    for extra in ["incremental = \"updated_at\"\n", "backfill = { max_chunks = 4 }\n", "seed = { below = \"2030-01-01T00:00:00Z\" }\n"] {
        let s = spec(&doc(extra, "write_mode = \"replace\"\n"));
        match s.validate() {
            Err(RunError::PipelineReplaceUnsupported(m)) => assert!(m.contains("`items`"), "{m}"),
            other => panic!("{extra}: {other:?}"),
        }
    }
    let full_refresh = spec(&BASE.replace("name = \"items\"", "name = \"items\"\nwrite_mode = \"replace\""));
    assert!(full_refresh.validate().is_ok(), "a full refetch replaces");
}

/// A manifest file the canonical type cannot deserialize raises `PipelineSpecInvalid`, naming the file, the key
/// path and the value found.
// spec: run.declare.spec-invalid@b36376f4
#[test]
fn an_undeserializable_manifest_names_file_key_path_and_value() {
    let bad = BASE.replace("id = \"orders\"", "id = \"orders\"\non_table_error = \"skip\"");
    match read_manifest(&manifest("pipelines/orders.toml", &bad)) {
        Err(RunError::PipelineSpecInvalid(m)) => {
            assert!(m.contains("pipelines/orders.toml"), "{m}");
            assert!(m.contains("pipeline[0].on_table_error"), "{m}");
            assert!(m.contains("skip"), "{m}");
        }
        other => panic!("{other:?}"),
    }
    match read_manifest(&manifest("pipelines/orders.json", r#"{"id": "orders", "source": {"name": 7}, "tables": []}"#)) {
        Err(RunError::PipelineSpecInvalid(m)) => assert!(m.contains("pipelines/orders.json") && m.contains("source.name") && m.contains('7'), "{m}"),
        other => panic!("{other:?}"),
    }
    // A store-only manifest declares no pipeline.
    assert!(collect(&[manifest("contextful.toml", "[[pipeline.tables]]\nname = \"filings\"\n")]).unwrap().is_empty());
}
