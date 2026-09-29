//! `read.respond`'s restriction block, `context.describe`'s zone report and the ranked
//! read's excluded arms: a relation the session's zone withholds is named, never read as
//! an empty table.

use super::*;
use contextful_context::read::RetrieveRequest;
use contextful_core::store::bound_time::Bounds;
use serde_json::json;

const VENDOR_FILE: &str = "tables/research/vendor/data/runs/run-0001/ingest-a/part-00000.parquet";

fn restriction(r: &Response) -> &Value {
    r.blocks.get("contextful.restriction").unwrap_or_else(|| panic!("no restriction block in {:?}", r.blocks))
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

/// An arm whose table the session's zone excludes contributes no candidate, and the ranked response names that table in {{read.respond.restriction-block}}.
// spec: read.retrieve.excluded-arm@12f1c8ad
#[test]
fn a_ranked_read_names_its_excluded_arms() {
    let r = Reads::new();
    let s = r.session(&["research/*"], Some(("research/notes", "acme")), None);
    let request = RetrieveRequest::new("research/", "solar battery storage", at("2030-02-01T00:00:00Z"));
    let ranked = r.face.retrieve(&s, &request, Bounds::default()).unwrap();
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
