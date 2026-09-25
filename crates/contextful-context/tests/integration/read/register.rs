//! `read.register`: connection views, quiet tables, bare names, file listing and preview,
//! and the published limits.

use super::*;
use contextful_context::read::ReadOptions;
use contextful_core::store::bound_time::Bounds;
use parquet::file::reader::{FileReader, SerializedFileReader};
use serde_json::{json, Map};

fn tree(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            out.push(p.strip_prefix(root).unwrap().to_string_lossy().into_owned());
            if p.is_dir() {
                stack.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Ahead of a statement the engine opens a connection and issues one create-or-replace view per table the manifests name, each scanning {{store.reconcile.explicit-file-list}}. No view directory exists on disk.
// spec: read.register.connection-views@d8059ccb
#[test]
fn each_statement_registers_views_over_the_current_file_lists() {
    let r = Reads::new();
    let before = tree(r.store.root());
    let s = r.session(&["research/*"], None, Some("public-cloud:us-east-1"));
    assert_eq!(column(&r.query(&s, r#"SELECT item_id FROM "research/vendor""#).unwrap(), "item_id"), [json!("v1")]);
    assert_eq!(tree(r.store.root()), before, "reading writes nothing under the store root");
    // A run committed after the face opened joins the next statement's view.
    super::land_rows(&r.store, "research/vendor", "run-0002", json!([{ "item_id": "v2", "title": "Second feed" }]));
    let s = r.session(&["research/*"], None, Some("public-cloud:us-east-1"));
    let ids = column(&r.query(&s, r#"SELECT item_id FROM "research/vendor" ORDER BY item_id"#).unwrap(), "item_id");
    assert_eq!(ids, [json!("v1"), json!("v2")]);
}

/// The executor is an embedded columnar SQL engine, linked into every profile that serves reads, reading Parquet natively in standard SQL. An external process reads the same files with the engine uninstalled.
// spec: read.register.engine@54a1a0b1
#[test]
fn the_embedded_engine_reads_the_parquet_an_external_reader_opens() {
    let r = Reads::new();
    let s = r.session(&["research/*"], None, Some("public-cloud:us-east-1"));
    let counted = r.query(&s, r#"SELECT count(*) AS n FROM "research/vendor""#).unwrap();
    assert_eq!(column(&counted, "n"), [json!("1")]);
    let files = r.face.files(&s, Bounds::default()).unwrap();
    let path = column(&files, "path").into_iter().find(|p| p.as_str().unwrap().starts_with("tables/research/vendor/")).unwrap();
    let reader = SerializedFileReader::new(std::fs::File::open(r.store.root().join(path.as_str().unwrap())).unwrap()).unwrap();
    assert_eq!(reader.metadata().file_metadata().num_rows(), 1);
}

/// A quiet table registers as {{store.declare.empty-run}}. A read of it returns an empty result, never a missing-relation fault.
// spec: read.register.quiet-table@ba45ce66
#[test]
fn a_quiet_table_reads_empty() {
    let r = Reads::new();
    let s = r.session(&["research/*"], None, None);
    let quiet = r.query(&s, r#"SELECT * FROM "research/quiet""#).unwrap();
    assert!(quiet.rows.is_empty() && !quiet.truncated);
    assert!(quiet.columns.contains(&"_ingested_at".to_string()), "{:?}", quiet.columns);
}

/// A bare table name in any read — a caller statement, a template body, a ranking arm, a file preview — resolves to the caller's registered relation, which carries the caller's restriction.
// spec: read.register.bare-name@e78fb8b8
#[test]
fn every_bare_name_resolves_to_the_callers_relation() {
    let r = Reads::new();
    let mut grant = read(&["research/*"], Some(("research/notes", "acme")));
    grant.templates = Some(vec!["*".into()]);
    let s = r.session_for(loop_subject("agent://research-loop"), vec![grant], None);
    let acme = [json!("n1"), json!("n2"), json!("n3")];

    let statement = r.query(&s, r#"SELECT note_id FROM "research/notes" ORDER BY note_id"#).unwrap();
    assert_eq!(column(&statement, "note_id"), acme);

    let args: Map<String, Value> = [("tenant".to_string(), json!("acme"))].into_iter().collect();
    let template = r.face.execute_template(&s, "notes_for", &args, ReadOptions::default()).unwrap();
    assert_eq!(column(&template, "note_id"), acme);

    let request = contextful_context::read::RetrieveRequest { prefix: "research/notes".into(), query: "solar battery storage".into(), ..Default::default() };
    let ranked = r.face.retrieve(&s, &request, Bounds::default()).unwrap();
    assert!(column(&ranked, "_row").iter().all(|row| row["tenant"] == json!("acme")), "{:?}", ranked.rows);

    let files = r.face.files(&s, Bounds::default()).unwrap();
    let notes = column(&files, "path").into_iter().find(|p| p.as_str().unwrap().starts_with("tables/research/notes/")).unwrap();
    let preview = r.face.file(&s, notes.as_str().unwrap(), ReadOptions::default()).unwrap();
    let mut previewed: Vec<Value> = column(&preview, "note_id");
    previewed.sort_by_key(|v| v.to_string());
    assert_eq!(previewed, acme);
}

/// `context.files` returns store-root-relative paths for the tables the caller reads, and a table outside that set contributes no path.
// spec: read.register.file-listing@14f34390
#[test]
fn file_listing_covers_the_callers_tables_alone() {
    let r = Reads::new();
    let s = r.session(&["research/*"], None, None);
    let files = r.face.files(&s, Bounds::default()).unwrap();
    let paths: Vec<Value> = column(&files, "path");
    assert!(paths.iter().any(|p| p.as_str().unwrap().starts_with("tables/research/notes/data/runs/run-0001/")), "{paths:?}");
    assert!(paths.iter().all(|p| !p.as_str().unwrap().starts_with('/')));
    assert!(r.store.root().join("tables/hr/salaries").is_dir(), "the ungranted table holds files");
    assert!(paths.iter().all(|p| !p.as_str().unwrap().starts_with("tables/hr/")), "{paths:?}");
}

/// `context.file` resolves a path to its `(table, run_id)` and reads it through that table's registered relation. A snapshot part, a traversal, an absolute path or a ledger file raises `FilePreviewNotATable`.
// spec: read.register.file-preview-target@cbe71acd
#[test]
fn a_preview_reads_a_run_file_through_its_relation() {
    let r = Reads::new();
    let s = r.session(&["research/*"], None, None);
    let preview = r.face.file(&s, "tables/research/contacts/data/runs/run-0001/ingest-a/part-00000.parquet", ReadOptions::default()).unwrap();
    // The relation's row predicate applies: the other agent's contact stays out.
    let mut ids: Vec<Value> = column(&preview, "contact_id");
    ids.sort_by_key(|v| v.to_string());
    assert_eq!(ids, [json!("c1"), json!("c2"), json!("c4")]);
    for bad in [
        "tables/research/notes/data/snapshots/snapshot-01/part-00000.parquet",
        "tables/research/notes/../../hr/salaries/data/runs/run-0001/ingest-a/part-00000.parquet",
        "/tmp/tables/research/notes/data/runs/run-0001/ingest-a/part-00000.parquet",
        "tables/research/notes/requests/run-0001.ingest-a.parquet",
        "config.toml",
    ] {
        refused_with(r.face.file(&s, bad, ReadOptions::default()), "FilePreviewNotATable");
    }
    let hr = "tables/hr/salaries/data/runs/run-0001/ingest-a/part-00000.parquet";
    refused_with(r.face.file(&s, hr, ReadOptions::default()), "EnforceUnknownRelation");
}

/// A table's published `limits` block lists a bound exactly when the engine applies it.
// spec: read.register.advertised-is-enforced@25c52c9e
#[test]
fn the_published_limit_is_the_applied_one() {
    let r = Reads::new();
    let on_prem = r.session(&["research/*"], None, None);
    let notes = r.face.describe(&on_prem, Some("research/notes")).unwrap();
    assert_eq!(notes["limits"], json!({ "max_rows": 3 }));
    let all = r.query(&on_prem, r#"SELECT note_id FROM "research/notes""#).unwrap();
    assert_eq!((all.rows.len(), all.truncated), (3, true));
    let s = r.session(&["research/*"], None, Some("public-cloud:us-east-1"));
    let vendor = r.face.describe(&s, Some("research/vendor")).unwrap();
    assert!(vendor.get("limits").is_none(), "{vendor}");
    super::land_rows(&r.store, "research/vendor", "run-0002", json!([{ "item_id": "v2" }, { "item_id": "v3" }, { "item_id": "v4" }]));
    let s = r.session(&["research/*"], None, Some("public-cloud:us-east-1"));
    let vendor_rows = r.query(&s, r#"SELECT item_id FROM "research/vendor""#).unwrap();
    assert_eq!((vendor_rows.rows.len(), vendor_rows.truncated), (4, false));
}

/// A per-table row ceiling published as `limits.max_rows` bounds rows delivered, applied at execution with an over-fetch of 1 rows. It bounds no work performed.
// spec: read.respond.row-ceiling@fa929ed9
#[test]
fn the_row_ceiling_bounds_delivery_with_one_probe_row() {
    let r = Reads::new();
    let s = r.session(&["research/*"], None, None);
    // The count scans every row; only delivery is bounded.
    let counted = r.query(&s, r#"SELECT count(*) AS n FROM "research/notes""#).unwrap();
    assert_eq!(column(&counted, "n"), [json!("4")]);
    let capped = r.query(&s, r#"SELECT note_id FROM "research/notes" ORDER BY note_id"#).unwrap();
    assert_eq!((column(&capped, "note_id"), capped.truncated), (vec![json!("n1"), json!("n2"), json!("n3")], true));
    let exact = r.query(&s, r#"SELECT note_id FROM "research/notes" WHERE tenant = 'acme'"#).unwrap();
    assert_eq!((exact.rows.len(), exact.truncated), (3, false));
    let asked = r.face.query(&s, r#"SELECT note_id FROM "research/notes""#, ReadOptions { limit: Some(2), internals: true }).unwrap();
    assert_eq!((asked.rows.len(), asked.truncated), (2, true));
    assert_eq!(asked.blocks["contextful.internals"]["limit"], json!(2));
}
