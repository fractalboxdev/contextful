//! `read.respond`'s restriction block, `context.describe`'s zone report and the ranked
//! read's excluded arms: a relation the session's zone withholds is named, never read as
//! an empty table.

use super::*;
use contextful_context::read::RetrieveRequest;
use contextful_core::store::bound_time::Bounds;
use serde_json::json;

const VENDOR_FILE: &str = "tables/research/vendor/data/runs/run-0001/ingest-a/part-00000.parquet";

#[test]
fn a_live_session_and_cached_response_refuse_an_incomplete_erasure_frontier() {
    let manifest = MANIFEST.replace("name = \"research/notes\"", "name = \"research/notes\"\nresult_cache = \"1h\"");
    let mut r = Reads::with_manifest(&manifest);
    r.face = r.face.with_result_cache(1_000_000);
    let session = r.session(&["research/notes"], None, None);
    let sql = "SELECT note_id FROM \"research/notes\" ORDER BY note_id";
    assert_eq!(r.query(&session, sql).unwrap().rows.len(), 3);
    assert_eq!(r.query(&session, sql).unwrap().rows.len(), 3);
    assert_eq!(r.face.results().unwrap().counts().hits, 1, "the retained response is a real cache hit");
    std::fs::write(r.store.root().join("_erasure_frontier.json"), b"{truncated").unwrap();
    let answer = r.query(&session, sql);
    assert!(answer.is_err(), "a pre-erasure session releases rows after a disagreeing publication: {answer:?}");
    assert!(answer.unwrap_err().to_string().starts_with("ErasureTransactionIncomplete"));
}

#[test]
fn file_listing_and_preview_keep_logical_names_at_a_replaced_frontier() {
    let mut r = Reads::new();
    r.store = crate::erase::select_signed_store_replacement(&r.store, "research/notes").0;
    r.face = Face::open(r.store.clone(), MANIFEST, pepper()).unwrap();
    let session = r.session(&["research/notes"], None, None);
    let listing = r.face.files(&session, Bounds::default()).unwrap();
    assert!(!listing.rows.is_empty());
    for row in listing.rows {
        let path = row[1].as_str().unwrap();
        assert!(path.starts_with("tables/research/notes/"), "internal replacement path escapes into the file API: {path}");
        let preview = r.face.file(&session, path, ReadOptions::default()).unwrap();
        assert!(!preview.rows.is_empty());
    }
}

fn restriction(r: &Response) -> &Value {
    r.blocks.get("contextful.restriction").unwrap_or_else(|| panic!("no restriction block in {:?}", r.blocks))
}

fn expensive_statement() -> String {
    // Four materialized arms execute 96^4 random evaluations under the statement deadline.
    let rows = (0..32).map(|_| "SELECT note_id FROM \"research/notes\"").collect::<Vec<_>>().join(" UNION ALL ");
    format!("WITH work AS MATERIALIZED ({rows}) SELECT sum(random()) FROM work t0, work t1, work t2, work t3")
}

// spec: read.respond.duration-ceiling@cfa152d7
// spec: authority.grant.duration-ceiling@e1d352e3
#[test]
fn a_duration_ceiling_interrupts_one_statement_and_the_next_read_answers() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], None, None);
    let started = std::time::Instant::now();
    let err = r.face.query(&s, &expensive_statement(), ReadOptions { max_duration_ms: Some(50), ..ReadOptions::default() }).unwrap_err();
    // The 30-second guard catches stalled cancellation while shared test workers may delay a 50 ms deadline.
    assert!(started.elapsed() < std::time::Duration::from_secs(30), "{err}");
    let message = err.to_string();
    assert!(message.contains("ReadDurationExceeded") && message.contains("50") && message.contains("request"), "{message}");
    let next = r.query(&s, "SELECT note_id FROM \"research/notes\" ORDER BY note_id").unwrap();
    assert_eq!(next.rows.len(), 3);
}

// spec: read.register.budget-advertisement@1f7e2056
#[test]
fn a_table_deadline_wins_and_describe_advertises_only_declared_budgets() {
    let manifest = MANIFEST.replace("max_rows = 3", "max_rows = 3\nmax_duration_ms = 25\nmax_response_bytes = 8192");
    let r = Reads::with_manifest(&manifest);
    let s = r.session(&["research/*"], None, None);
    let notes = r.face.describe(&s, Some("research/notes"), Bounds::default()).unwrap();
    assert_eq!(notes["limits"]["max_duration_ms"], json!(25));
    assert_eq!(notes["limits"]["max_response_bytes"], json!(8192));
    let vendor = r.face.describe(&s, Some("research/vendor"), Bounds::default()).unwrap();
    assert!(vendor.get("limits").is_none_or(|limits| limits.get("max_duration_ms").is_none()));
    assert!(vendor.get("limits").is_none_or(|limits| limits.get("max_response_bytes").is_none()));
    let err = r.face.query(&s, &expensive_statement(), ReadOptions { max_duration_ms: Some(200), ..ReadOptions::default() }).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("ReadDurationExceeded") && message.contains("25") && message.contains("table"), "{message}");
}

// spec: authority.grant.byte-ceiling@af2c0696
#[test]
fn a_grant_budget_wins_over_request_and_table_budgets() {
    let manifest = MANIFEST.replace("max_rows = 3", "max_rows = 3\nmax_duration_ms = 100\nmax_response_bytes = 8192");
    let r = Reads::with_manifest(&manifest);
    let mut grant = read(&["research/notes"], None);
    grant.max_duration_ms = Some(25);
    grant.max_response_bytes = Some(500);
    let s = r.session_for(loop_subject("agent://budget"), vec![grant], None);
    let err = r.face.query(&s, &expensive_statement(), ReadOptions { max_duration_ms: Some(200), ..ReadOptions::default() }).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("ReadDurationExceeded") && message.contains("25") && message.contains("grant"), "{message}");
    let byte_manifest = MANIFEST.replace("max_rows = 3", "max_rows = 3\nmax_response_bytes = 8192");
    let r = Reads::with_manifest(&byte_manifest);
    let sql = "SELECT repeat(title, 20) FROM \"research/notes\" ORDER BY note_id";
    let unbounded = r.session(&["research/notes"], None, None);
    let mut one = r.query(&unbounded, sql).unwrap();
    one.rows.truncate(1);
    one.truncated = true;
    one.blocks.insert("contextful.truncation".into(), json!({ "by": "bytes", "ceiling": 999999, "source": "grant" }));
    let bytes = serde_json::to_vec(&one).unwrap().len() as u64;
    let mut byte_grant = read(&["research/notes"], None);
    byte_grant.max_response_bytes = Some(bytes);
    let byte_session = r.session_for(loop_subject("agent://budget"), vec![byte_grant], None);
    let cut = r.face.query(&byte_session, sql, ReadOptions { max_response_bytes: Some(bytes), ..ReadOptions::default() }).unwrap();
    assert_eq!(cut.to_json()["contextful.truncation"], json!({ "by": "bytes", "ceiling": bytes, "source": "grant" }));
    assert!(serde_json::to_vec(&cut).unwrap().len() as u64 <= bytes);
}

/// A grant over another table does not constrain a read its own action and pattern cannot authorize.
#[test]
fn unrelated_read_grants_do_not_set_row_duration_or_byte_ceilings() {
    let r = Reads::new();
    let vendor = read(&["research/vendor"], None);
    let sql = r#"SELECT item_id FROM "research/vendor""#;
    let answer = |unrelated| {
        let session = r.session_for(loop_subject("agent://budget"), vec![unrelated, vendor.clone()], Some("public-cloud:us-east-1"));
        column(&r.query(&session, sql).unwrap(), "item_id")
    };

    let mut row_grant = read(&["research/visits"], None);
    row_grant.max_rows = Some(0);
    assert_eq!(answer(row_grant), [json!("v1")]);

    let mut duration_grant = read(&["research/visits"], None);
    duration_grant.max_duration_ms = Some(0);
    assert_eq!(answer(duration_grant), [json!("v1")]);

    let mut byte_grant = read(&["research/visits"], None);
    byte_grant.max_response_bytes = Some(1);
    assert_eq!(answer(byte_grant), [json!("v1")]);
}

// spec: read.respond.byte-ceiling@72418c75
// spec: read.respond.truncation-cause@38517ea2
#[test]
fn a_byte_ceiling_preserves_whole_rows_and_names_the_cut() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], None, None);
    let sql = "SELECT repeat(title, 20) AS body FROM \"research/notes\" ORDER BY note_id";
    let full = r.query(&s, sql).unwrap();
    let mut one = full.clone();
    one.rows.truncate(1);
    one.truncated = true;
    one.blocks.insert("contextful.truncation".into(), json!({ "by": "bytes", "ceiling": 999999, "source": "request" }));
    let ceiling = serde_json::to_vec(&one).unwrap().len() as u64;
    let cut = r.face.query(&s, sql, ReadOptions { max_response_bytes: Some(ceiling), ..ReadOptions::default() }).unwrap();
    assert_eq!(cut.rows.len(), 1);
    assert!(cut.truncated);
    assert_eq!(cut.to_json()["contextful.truncation"], json!({ "by": "bytes", "ceiling": ceiling, "source": "request" }));
    assert!(serde_json::to_vec(&cut).unwrap().len() as u64 <= ceiling);

    let mut zero = one;
    zero.rows.clear();
    let too_small = serde_json::to_vec(&zero).unwrap().len() as u64 + 1;
    let err = r.face.query(&s, sql, ReadOptions { max_response_bytes: Some(too_small), ..ReadOptions::default() }).unwrap_err();
    assert!(err.to_string().contains("ReadResponseTooLarge"), "{err}");

    let row_cut = r.face.query(&s, sql, ReadOptions { limit: Some(1), ..ReadOptions::default() }).unwrap();
    assert_eq!(row_cut.to_json()["contextful.truncation"], json!({ "by": "rows", "ceiling": 1, "source": "request" }));
}

// spec: read.respond.truncation-tie@9027666a
#[test]
fn a_byte_cut_wins_when_a_row_ceiling_cuts_the_same_next_row() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], None, None);
    let sql = "SELECT repeat(title, 20) FROM \"research/notes\" ORDER BY note_id";
    let mut one = r.query(&s, sql).unwrap();
    one.rows.truncate(1);
    one.truncated = true;
    one.probe_row = None;
    one.blocks.insert("contextful.truncation".into(), json!({ "by": "bytes", "ceiling": 999999, "source": "request" }));
    let ceiling = serde_json::to_vec(&one).unwrap().len() as u64;
    let cut = r.face.query(&s, sql, ReadOptions { limit: Some(1), max_response_bytes: Some(ceiling), ..ReadOptions::default() }).unwrap();
    assert_eq!(cut.rows.len(), 1);
    assert_eq!(cut.to_json()["contextful.truncation"], json!({ "by": "bytes", "ceiling": ceiling, "source": "request" }));
    let mut grant = read(&["research/notes"], None);
    grant.max_rows = Some(1);
    let grant_session = r.session_for(loop_subject("agent://row-tie"), vec![grant], None);
    let row_cut = r.face.query(&grant_session, sql, ReadOptions { limit: Some(1), ..ReadOptions::default() }).unwrap();
    assert_eq!(row_cut.to_json()["contextful.truncation"], json!({ "by": "rows", "ceiling": 1, "source": "grant" }));
}

#[test]
fn a_byte_cut_reports_the_delivered_row_count_in_internals() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], None, None);
    let sql = "SELECT repeat(title, 20) FROM \"research/notes\" ORDER BY note_id";
    let mut one = r.face.query(&s, sql, ReadOptions { internals: true, ..ReadOptions::default() }).unwrap();
    one.rows.truncate(1);
    one.truncated = true;
    one.blocks["contextful.internals"]["row_count"] = json!(1);
    one.blocks["contextful.internals"]["elapsed_ms"] = json!(999999999);
    one.blocks.insert("contextful.truncation".into(), json!({ "by": "bytes", "ceiling": 999999, "source": "request" }));
    let ceiling = serde_json::to_vec(&one).unwrap().len() as u64;
    let cut = r.face.query(&s, sql, ReadOptions { max_response_bytes: Some(ceiling), internals: true, ..ReadOptions::default() }).unwrap();
    assert_eq!(cut.rows.len(), 1);
    assert_eq!(cut.to_json()["contextful.internals"]["row_count"], json!(1));
}

/// The restriction block carries the session zone, the incognito flag and one entry per touched relation the zone excludes or column-masks: `table`, `excluded`, `rows_dropped` and `columns_masked`. A read withholding no touched relation omits the block.
// spec: read.respond.restriction-block@ccb9c983
#[test]
fn a_zone_excluded_relation_is_named_beside_its_empty_rows() {
    let r = Reads::new();
    let s = r.session(&["research/*"], Some(("research/notes", "acme")), None);
    let vendor = r.query(&s, r#"SELECT * FROM "research/vendor""#).unwrap();
    assert!(vendor.rows.is_empty());
    assert_eq!(
        restriction(&vendor),
        &json!({
            "zone": "on-prem:hq",
            "incognito": false,
            "tables": [{ "table": "research/vendor", "excluded": true, "rows_dropped": 1, "columns_masked": [] }],
        })
    );
    // A statement touching only admitted relations carries no block.
    let notes = r.query(&s, r#"SELECT note_id FROM "research/notes""#).unwrap();
    assert!(!notes.blocks.contains_key("contextful.restriction"), "{:?}", notes.blocks);
    // A zone-nulled column is named; its table's rows all arrive.
    let public = r.session(&["research/*"], Some(("research/notes", "acme")), Some("public-cloud:us-east-1"));
    let visits = r.query(&public, r#"SELECT visit_id, case_notes FROM "research/visits""#).unwrap();
    assert_eq!(visits.rows, [vec![json!("w1"), Value::Null]]);
    assert_eq!(
        restriction(&visits)["tables"],
        json!([{ "table": "research/visits", "excluded": false, "rows_dropped": 0, "columns_masked": ["case_notes"] }])
    );
    // A template, a file preview and an engine-composed read carry the same block.
    let preview = r.face.file(&s, VENDOR_FILE, ReadOptions::default()).unwrap();
    assert_eq!(restriction(&preview)["tables"][0]["table"], json!("research/vendor"));
    let rows = r.face.rows(&s, "research/vendor", None).unwrap();
    assert_eq!(restriction(&rows)["tables"][0]["rows_dropped"], json!(1));
    let grant = Grant { templates: Some(vec!["notes_for".into()]), ..read(&["research/*"], Some(("research/notes", "acme"))) };
    let templated = r.session_for(loop_subject("agent://research-loop"), vec![grant], Some("public-cloud:us-east-1"));
    let template = r
        .face
        .execute_template(&templated, "notes_for", json!({ "tenant": "acme" }).as_object().unwrap(), ReadOptions::default())
        .unwrap();
    assert_eq!(restriction(&template)["tables"][0], json!({ "table": "research/notes", "excluded": true, "rows_dropped": 3, "columns_masked": [] }));
}

/// `rows_dropped` counts the rows the zone step removes from the whole relation, every earlier step applied; the caller's statement, filter and requested size never enter it, and no removed value crosses.
// spec: authority.place.excluded-disclosed@19c3d4fe
#[test]
fn rows_dropped_counts_the_relation_never_the_statement() {
    let r = Reads::new();
    let public = r.session(&["research/*"], Some(("research/notes", "acme")), Some("public-cloud:us-east-1"));
    // The tenant step precedes the zone step: globex's note is outside the count.
    for sql in [
        r#"SELECT * FROM "research/notes""#,
        r#"SELECT * FROM "research/notes" WHERE note_id = 'n1'"#,
        r#"SELECT * FROM "research/notes" WHERE title LIKE '%zeppelin%' LIMIT 1"#,
    ] {
        let out = r.query(&public, sql).unwrap();
        assert!(out.rows.is_empty(), "{sql}");
        let block = restriction(&out);
        assert_eq!(block["tables"][0]["rows_dropped"], json!(3), "{sql}");
        let text = block.to_string();
        for value in ["n1", "Solar", "dana@acme.example", "globex"] {
            assert!(!text.contains(value), "{value} crossed in {text}");
        }
    }
    // The row policy precedes the zone step too: the contacts the agent does not own stay uncounted.
    let contacts = r.query(&public, r#"SELECT * FROM "research/contacts""#).unwrap();
    assert_eq!(restriction(&contacts)["tables"][0]["rows_dropped"], json!(3));
}

/// `context.describe` reports the session zone as `session_zone`, and per table, listed or described, `zone_admitted`: whether the table's effective allow-set admits that zone.
// spec: read.register.describe-zone@6cd98d09
#[test]
fn describe_reports_the_session_zone_and_each_tables_admission() {
    let r = Reads::new();
    let s = r.session(&["research/*"], Some(("research/notes", "acme")), None);
    let listing = r.face.describe(&s, None, Bounds::default()).unwrap();
    assert_eq!(listing["session_zone"], json!("on-prem:hq"));
    let admitted = |name: &str| {
        listing["tables"].as_array().unwrap().iter().find(|t| t["table"] == json!(name)).unwrap_or_else(|| panic!("{name}"))["zone_admitted"]
            .clone()
    };
    assert_eq!(admitted("research/vendor"), json!(false));
    assert_eq!(admitted("research/notes"), json!(true));
    assert_eq!(admitted("research/visits"), json!(true));
    let vendor = r.face.describe(&s, Some("research/vendor"), Bounds::default()).unwrap();
    assert_eq!((vendor["session_zone"].clone(), vendor["zone_admitted"].clone()), (json!("on-prem:hq"), json!(false)));
    let public = r.session(&["research/*"], None, Some("public-cloud:us-east-1"));
    let vendor = r.face.describe(&public, Some("research/vendor"), Bounds::default()).unwrap();
    assert_eq!((vendor["session_zone"].clone(), vendor["zone_admitted"].clone()), (json!("public-cloud:us-east-1"), json!(true)));
}

#[test]
fn describe_marks_declared_memory_tables_regardless_of_name() {
    let manifest = format!("{MANIFEST}\n[[table]]\nname = \"research/insights\"\nshape = \"memory_facts\"\ncolumns = [\"claim_id\", \"subject\", \"predicate\", \"object\", \"scope\", \"tier\", \"confidence\", \"valid_from\", \"valid_to\", \"evidence\", \"superseded_by\", \"grant_id\", \"agent\"]\n");
    let r = Reads::with_manifest(&manifest);
    let s = r.session(&["research/*"], None, None);
    let listing = r.face.describe(&s, None, Bounds::default()).unwrap();
    let tables = listing["tables"].as_array().unwrap();
    let insights = tables.iter().find(|item| item["table"] == "research/insights").unwrap();
    assert_eq!(insights["kind"], json!("memory"));
    let notes = tables.iter().find(|item| item["table"] == "research/notes").unwrap();
    assert_eq!(notes["kind"], json!("data"));
}

/// An arm whose table the session's zone excludes contributes no candidate, and the ranked response names that table in {{read.respond.restriction-block}}.
// spec: read.retrieve.excluded-arm@12f1c8ad
#[test]
fn a_ranked_read_names_its_excluded_arms() {
    let r = Reads::new();
    let s = r.session(&["research/*"], Some(("research/notes", "acme")), None);
    let request = RetrieveRequest::new("research/", "solar battery storage", at("2030-02-01T00:00:00Z"));
    let ranked = r.face.retrieve(&s, &request, Bounds::default()).unwrap();
    assert!(!column(&ranked, "_table").is_empty(), "the exclusion below ranges over no element");
    assert!(column(&ranked, "_table").iter().all(|t| t != "research/vendor"));
    let tables = &restriction(&ranked)["tables"];
    assert_eq!(tables, &json!([{ "table": "research/vendor", "excluded": true, "rows_dropped": 1, "columns_masked": [] }]));
    // The count holds whatever the requested size.
    let one = r.face.retrieve(&s, &RetrieveRequest { limit: Some(1), ..request }, Bounds::default()).unwrap();
    assert_eq!(&restriction(&one)["tables"], tables);
}

/// An owner read naming the request ledger of a zone-excluded table receives no call row,
/// and the block names the ledger with its whole call count.
#[test]
fn a_zone_excluded_tables_ledger_is_named_with_its_call_count() {
    use super::register::{call, record_calls};
    let r = Reads::new();
    record_calls(&r, "research/vendor", "run-0001", &[call("r1", Some(0), Some(200)), call("r2", None, None)]);
    let owner = r.session(&["research/*"], None, None);
    let calls = r.query(&owner, r#"SELECT request_id FROM "research/vendor__requests""#).unwrap();
    assert!(calls.rows.is_empty());
    assert_eq!(
        restriction(&calls),
        &json!({
            "zone": "on-prem:hq",
            "incognito": false,
            "tables": [{ "table": "research/vendor__requests", "excluded": true, "rows_dropped": 2, "columns_masked": [] }],
        })
    );
    // The admitting zone reads the calls and carries no block.
    let public = r.session(&["research/*"], None, Some("public-cloud:us-east-1"));
    let seen = r.query(&public, r#"SELECT request_id FROM "research/vendor__requests""#).unwrap();
    assert_eq!(column(&seen, "request_id"), [json!("r1"), json!("r2")]);
    assert!(!seen.blocks.contains_key("contextful.restriction"), "{:?}", seen.blocks);
}
