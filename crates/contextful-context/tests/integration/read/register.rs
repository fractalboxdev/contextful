//! `read.register`: connection views, quiet tables, bare names, file listing and preview,
//! and the published limits.

use super::*;
use contextful_context::read::ReadOptions;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::ledger::RequestRecord;
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
    // Its declared mask names a column no batch has landed yet; the table registers all
    // the same, and every other covered table with it.
    assert!(s.reads("research/quiet") && s.reads("research/notes"));
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

    let request = contextful_context::read::RetrieveRequest::new("research/notes", "solar battery storage", at("2030-02-01T00:00:00Z"));
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

/// Regression: a preview reads exactly the named file, and a path naming no committed
/// file of its table is refused rather than answered with the run's other rows.
#[test]
fn a_preview_reads_the_named_file_alone() {
    use contextful_context::land::{land_batches, Batch, Position, RunContext};
    use contextful_core::store::lay_out::NodeId;
    use contextful_core::store::reserve::Injection;
    let r = Reads::new();
    let batch = |ids: &[&str]| Batch {
        rows: ids.iter().map(|id| json!({ "item_id": id, "title": "batch" }).as_object().unwrap().clone()).collect(),
        types: Default::default(),
    };
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "run-0002".into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None },
        committed_at: at("2030-01-11T00:00:00Z"),
    };
    let decl = TableDecl::named("research/vendor");
    let m = land_batches(&r.store, &decl, &[batch(&["v2", "v3"]), batch(&["v4"])], &ctx, &Position::default(), &|| Ok(())).unwrap();
    assert_eq!(m.parts.len(), 2);
    let s = r.session(&["research/*"], None, Some("public-cloud:us-east-1"));
    let files = column(&r.face.files(&s, Bounds::default()).unwrap(), "path");
    let parts: Vec<&str> = files.iter().filter_map(|f| f.as_str()).filter(|f| f.contains("/runs/run-0002/")).collect();
    assert_eq!(parts.len(), 2, "{files:?}");
    let mut seen = Vec::new();
    for part in &parts {
        let preview = r.face.file(&s, part, ReadOptions::default()).unwrap();
        seen.push(column(&preview, "item_id"));
    }
    seen.sort_by_key(|v| v.len());
    assert_eq!(seen, [vec![json!("v4")], vec![json!("v2"), json!("v3")]]);
    let absent = "tables/research/vendor/data/runs/run-0002/ingest-a/part-00009.parquet";
    refused_with(r.face.file(&s, absent, ReadOptions::default()), "FilePreviewNotATable");
    let other_run = "tables/research/vendor/data/runs/run-0009/ingest-a/part-00000.parquet";
    refused_with(r.face.file(&s, other_run, ReadOptions::default()), "FilePreviewNotATable");
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

/// One mediated outbound call, as a test names it.
fn call(id: &str, batch_seq: Option<i32>, status: Option<u16>) -> RequestRecord {
    RequestRecord {
        request_id: id.into(),
        vendor_request_id: Some(format!("vendor-{id}")),
        connector: "http".into(),
        method: "GET".into(),
        url_host: "api.example.org".into(),
        status_code: status,
        started_at: at("2030-01-10T00:00:00Z"),
        duration_ms: 12,
        batch_seq,
    }
}

fn record_calls(r: &Reads, table: &str, run: &str, calls: &[RequestRecord]) {
    let node = NodeId::parse("ingest-a").unwrap();
    contextful_context::ledger::append(&r.store, table, run, &node, calls).unwrap();
}

/// Each table's per-run request ledger registers as the child relation `<table>__requests`, holding identifiers, connector, method, host, status and timing of mediated outbound calls.
// spec: read.register.ledger-relation@0a97be3c
#[test]
fn a_tables_request_ledger_reads_as_its_child_relation() {
    let r = Reads::new();
    record_calls(&r, "research/vendor", "run-0001", &[call("r1", Some(0), Some(200))]);
    // A second flush of the run keeps the first flush's rows; a call whose scope produced
    // no batch, or that met no response, keeps its row with nulls.
    record_calls(&r, "research/vendor", "run-0001", &[call("r2", None, None)]);
    assert!(r.store.root().join("tables/research/vendor/requests/run-0001.ingest-a.parquet").is_file());

    let s = r.session(&["research/*"], None, Some("public-cloud:us-east-1"));
    let calls = r
        .query(
            &s,
            r#"SELECT run_id, request_id, vendor_request_id, connector, method, url_host, status_code, batch_seq, duration_ms, started_at
               FROM "research/vendor__requests" ORDER BY request_id"#,
        )
        .unwrap();
    assert_eq!(column(&calls, "request_id"), [json!("r1"), json!("r2")]);
    assert_eq!(column(&calls, "run_id"), [json!("run-0001"), json!("run-0001")]);
    assert_eq!(column(&calls, "vendor_request_id"), [json!("vendor-r1"), json!("vendor-r2")]);
    assert_eq!(column(&calls, "url_host"), [json!("api.example.org"), json!("api.example.org")]);
    assert_eq!(column(&calls, "method"), [json!("GET"), json!("GET")]);
    assert_eq!(column(&calls, "connector"), [json!("http"), json!("http")]);
    assert_eq!(column(&calls, "status_code"), [json!(200), Value::Null]);
    assert_eq!(column(&calls, "batch_seq"), [json!(0), Value::Null]);
    assert_eq!(column(&calls, "duration_ms"), [json!("12"), json!("12")]);
    assert_ne!(column(&calls, "started_at")[0], Value::Null);

    // The run id and batch ordinal join a data row onto the calls that fetched it.
    let joined = r
        .query(
            &s,
            r#"SELECT v.item_id, q.request_id FROM "research/vendor" v
               JOIN "research/vendor__requests" q ON v._run_id = q.run_id AND v._batch_seq = q.batch_seq"#,
        )
        .unwrap();
    assert_eq!((column(&joined, "item_id"), column(&joined, "request_id")), (vec![json!("v1")], vec![json!("r1")]));

    // A table no call has been recorded against registers an empty ledger.
    let quiet = r.query(&s, r#"SELECT * FROM "research/visits__requests""#).unwrap();
    assert!(quiet.rows.is_empty() && quiet.columns.contains(&"request_id".to_string()), "{:?}", quiet.columns);
    // The ledger file stays out of the data listing.
    let files = r.face.files(&s, Bounds::default()).unwrap();
    assert!(column(&files, "path").iter().all(|p| !p.as_str().unwrap().contains("/requests/")));
}

/// The child relation registers on the owner read alone: a credential carrying no tenant scope, over a table
/// carrying no row policy. Naming a closed ledger raises `LedgerNotTenantScoped`, stating what closed the
/// relation.
// spec: read.register.scoped-ledger@4e735917
#[test]
fn a_tenant_scoped_read_naming_the_ledger_is_refused() {
    let r = Reads::new();
    record_calls(&r, "research/notes", "run-0001", &[call("r1", Some(0), Some(200))]);
    record_calls(&r, "research/vendor", "run-0001", &[call("r2", Some(0), Some(200))]);

    let owner = r.session(&["research/*"], None, None);
    assert!(owner.reads("research/notes__requests"));
    let seen = r.query(&owner, r#"SELECT request_id FROM "research/notes__requests""#).unwrap();
    assert_eq!(column(&seen, "request_id"), [json!("r1")]);

    let scoped = r.session(&["research/*"], Some(("research/notes", "acme")), Some("public-cloud:us-east-1"));
    assert!(!scoped.reads("research/notes__requests"));
    let message = refused_with(r.query(&scoped, r#"SELECT request_id FROM "research/notes__requests""#), "LedgerNotTenantScoped");
    assert!(message.contains("`research/notes`") && message.contains("tenant"), "{message}");
    // Inside a subquery the relation is refused all the same.
    refused_with(
        r.query(&scoped, r#"SELECT note_id FROM "research/notes" WHERE note_id IN (SELECT request_id FROM "research/notes__requests")"#),
        "LedgerNotTenantScoped",
    );
    // A tenant-scoped credential is no owner read: the ledger of a table it holds with no
    // tenant scope is closed too, while that table itself still reads.
    assert!(!scoped.reads("research/vendor__requests") && scoped.reads("research/vendor"));
    let message = refused_with(r.query(&scoped, r#"SELECT request_id FROM "research/vendor__requests""#), "LedgerNotTenantScoped");
    assert!(message.contains("`research/vendor`") && message.contains("tenant scope"), "{message}");
    // A table the credential does not read names no ledger at all.
    refused_with(r.query(&scoped, r#"SELECT * FROM "hr/salaries__requests""#), "EnforceUnknownRelation");

    // A table under a row policy is no owner read either: its ledger would carry every
    // run's calls, rows the credential cannot see among them.
    record_calls(&r, "research/contacts", "run-0001", &[call("r3", Some(0), Some(200))]);
    assert!(!owner.reads("research/contacts__requests") && owner.reads("research/contacts"));
    let message = refused_with(r.query(&owner, r#"SELECT request_id FROM "research/contacts__requests""#), "LedgerNotTenantScoped");
    assert!(message.contains("`research/contacts`") && message.contains("row policy"), "{message}");
}

/// Appends to one run's ledger file serialize under a lock on it, and an append returns once its rows and the
/// rename are synced to disk.
// spec: store.reserve.ledger-append@a90828c9
#[test]
fn concurrent_flushes_of_one_run_keep_every_row() {
    let r = Reads::new();
    let store = std::sync::Arc::new(r.store.clone());
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let store = store.clone();
            std::thread::spawn(move || {
                let node = NodeId::parse("ingest-a").unwrap();
                for j in 0..5 {
                    contextful_context::ledger::append(&store, "research/vendor", "run-0001", &node, &[call(&format!("t{i}-{j}"), None, None)]).unwrap();
                }
            })
        })
        .collect();
    threads.into_iter().for_each(|t| t.join().unwrap());
    let path = r.store.root().join("tables/research/vendor/requests/run-0001.ingest-a.parquet");
    assert_eq!(contextful_context::ledger::read(&path).unwrap().len(), 40, "a concurrent flush dropped another's rows");
}

#[test]
fn an_unreadable_ledger_file_fails_only_a_read_naming_that_ledger() {
    let r = Reads::new();
    let requests = r.store.root().join("tables/research/vendor/requests");
    std::fs::create_dir_all(&requests).unwrap();
    std::fs::write(requests.join("run-0009.ingest-a.parquet"), b"").unwrap();
    let s = r.session(&["research/*"], None, None);
    let notes = r.query(&s, r#"SELECT note_id FROM "research/notes""#).unwrap();
    assert!(!notes.rows.is_empty());
    assert!(r.query(&s, r#"SELECT * FROM "research/vendor__requests""#).is_err());
}

#[test]
fn a_ledger_answers_to_its_tables_row_ceiling() {
    let r = Reads::new();
    let calls: Vec<RequestRecord> = (0..6).map(|i| call(&format!("r{i}"), Some(0), Some(200))).collect();
    record_calls(&r, "research/notes", "run-0001", &calls);
    let s = r.session(&["research/*"], None, None);
    let seen = r.query(&s, r#"SELECT request_id FROM "research/notes__requests""#).unwrap();
    assert_eq!((seen.rows.len(), seen.truncated), (3, true), "notes publish max_rows = 3");
}
