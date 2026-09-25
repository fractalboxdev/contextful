//! `read.retrieve` and `read.rank` over the engine: arms per registered relation,
//! snippets, the resolved publication column, projected fields and the retrieval block.

use super::*;
use contextful_context::read::RetrieveRequest;
use contextful_core::store::bound_time::Bounds;
use serde_json::json;

fn ask(prefix: &str, query: &str) -> RetrieveRequest {
    RetrieveRequest { prefix: prefix.into(), query: query.into(), ..RetrieveRequest::default() }
}

fn ids(r: &Response, key: &str) -> Vec<String> {
    column(r, "_row").iter().map(|row| row[key].as_str().unwrap().to_string()).collect()
}

/// A snippet concatenates up to three text columns: label-priority columns — title, summary, description, thesis and kin — first, then prose-worthy columns in schema order.
// spec: read.retrieve.snippet@f249e729
#[test]
fn a_snippet_leads_with_label_columns() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], Some(("research/notes", "acme")), None);
    let ranked = r.face.retrieve(&s, &ask("research/", "solar battery storage"), Bounds::default()).unwrap();
    assert_eq!(column(&ranked, "_snippet")[0], json!("Solar battery storage costs fall — Cell prices fell again."));
    let v = r.session(&["research/*"], None, Some("public-cloud:us-east-1"));
    let vendor = r.face.retrieve(&v, &ask("research/vendor", "feed"), Bounds::default()).unwrap();
    // Three text columns at most, in schema order after the label column.
    assert_eq!(column(&vendor, "_snippet"), [json!("Solar battery storage feed — quoted")]);
}

/// Content hashes, URLs, identifiers and instant-valued columns never qualify for a snippet.
// spec: read.retrieve.identifiers-never-snippet@ff991c2b
#[test]
fn identifier_columns_never_enter_a_snippet() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], Some(("research/notes", "acme")), None);
    let ranked = r.face.retrieve(&s, &RetrieveRequest { min_score: Some(0), ..ask("research/", "solar battery storage") }, Bounds::default()).unwrap();
    let rows = column(&ranked, "_row");
    let n3 = rows.iter().position(|row| row["note_id"] == json!("n3")).unwrap();
    // n3's URL spells the whole question; its snippet carries none of it.
    assert!(rows[n3]["source_url"].as_str().unwrap().contains("solar-battery-storage"));
    assert_eq!(column(&ranked, "_score")[n3], json!(0));
    for snippet in column(&ranked, "_snippet") {
        let snippet = snippet.as_str().unwrap();
        assert!(!snippet.contains("https://") && !snippet.contains("2030-") && !snippet.contains("n1"), "{snippet}");
    }
}

/// The engine resolves each table's publication column itself, outside the caller's filter.
// spec: read.retrieve.engine-resolved-date@31afd195
#[test]
fn the_engine_resolves_the_publication_column() {
    let r = Reads::new();
    let s = r.session(&["research/*"], Some(("research/notes", "acme")), Some("on-prem:hq"));
    let request = RetrieveRequest { min_score: Some(0), ..ask("research/", "solar battery storage hiring grids") };
    let ranked = r.face.retrieve(&s, &request, Bounds::default()).unwrap();
    let basis: Vec<(String, String)> = column(&ranked, "_row")
        .iter()
        .zip(column(&ranked, "_date_basis"))
        .filter_map(|(row, b)| Some((row.get("note_id")?.as_str()?.to_string(), b.as_str()?.to_string())))
        .collect();
    assert!(basis.contains(&("n1".into(), "published_at".into())), "{basis:?}");
    assert!(basis.contains(&("n3".into(), "uncastable".into())), "{basis:?}");
    let quiet = r.face.retrieve(&s, &RetrieveRequest { min_score: Some(0), ..ask("research/visits", "") }, Bounds::default()).unwrap();
    assert!(column(&quiet, "_date_basis").iter().all(|b| b == "_ingested_at"), "{:?}", quiet.rows);
}

/// Reserved projected columns — modality, language, prompt hash, kind — render null for a table lacking them, and its rows stay in the union.
// spec: read.retrieve.reserved-columns-project-null@ec5bb1d4
#[test]
fn reserved_columns_project_null_for_a_table_lacking_them() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], Some(("research/notes", "acme")), None);
    let ranked = r.face.retrieve(&s, &ask("research/", "solar battery storage"), Bounds::default()).unwrap();
    assert_eq!(ranked.rows.len(), 2);
    for c in ["_modality", "_lang", "_prompt_hash", "_kind"] {
        assert!(column(&ranked, c).iter().all(|v| v.is_null()), "{c}");
    }
}

/// Match counts ride outside the internals opt-in, reporting how many rows the ranker scored as matching in the same call.
// spec: read.respond.match-count@d3df9689
#[test]
fn the_block_reports_how_many_rows_matched() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], Some(("research/notes", "acme")), None);
    let ranked = r.face.retrieve(&s, &ask("research/", "solar battery storage"), Bounds::default()).unwrap();
    let block = &ranked.blocks["contextful.retrieval"];
    assert_eq!(block["matched"], json!(2));
    assert_eq!(block["candidates_prefloor"], json!(3));
    assert_eq!(block["floor"], json!(2));
    assert!(!ranked.blocks.contains_key("contextful.internals"));
}

/// The lexical engine's own float score never crosses to a caller.
// spec: read.rank.internal-score-stays-internal@8a3741ac
#[test]
fn only_the_integer_score_crosses() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], Some(("research/notes", "acme")), None);
    let ranked = r.face.retrieve(&s, &ask("research/", "solar battery storage"), Bounds::default()).unwrap();
    assert_eq!(column(&ranked, "_score"), [json!(3), json!(2)]);
    assert!(column(&ranked, "_vscore").iter().all(|v| v.is_null()));
    for row in &ranked.rows {
        for v in row {
            assert!(!v.is_f64(), "a float crossed: {row:?}");
        }
    }
}

/// The retrieval block is omitted from every non-ranked statement and from a build with no ranker, and an absent block differs from one reporting zero matches.
// spec: read.rank.absent-block@886f6327
#[test]
fn a_statement_carries_no_retrieval_block() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], None, None);
    let statement = r.query(&s, r#"SELECT note_id FROM "research/notes""#).unwrap();
    assert!(!statement.blocks.contains_key("contextful.retrieval"));
    let nothing = r.face.retrieve(&s, &ask("research/", "zeppelin"), Bounds::default()).unwrap();
    assert_eq!(nothing.blocks["contextful.retrieval"]["matched"], json!(0));
    assert!(nothing.rows.is_empty());
}

/// Every step, zone exclusion included, completes before a ranked result is cut to its requested size; visible positions carry no trace of a withheld row.
// spec: authority.compose.before-the-cut@7710de0e
#[test]
fn restriction_completes_before_the_cut() {
    let r = Reads::new();
    // Globex's note and the vendor feed score as high as the best acme note.
    let s = r.session(&["research/*"], Some(("research/notes", "acme")), None);
    let one = r.face.retrieve(&s, &RetrieveRequest { limit: Some(1), ..ask("research/", "solar battery storage") }, Bounds::default()).unwrap();
    assert_eq!(ids(&one, "note_id"), ["n1"]);
    let two = r.face.retrieve(&s, &RetrieveRequest { limit: Some(2), ..ask("research/", "solar battery storage") }, Bounds::default()).unwrap();
    assert_eq!(ids(&two, "note_id"), ["n1", "n2"]);
    let block = &two.blocks["contextful.retrieval"];
    assert_eq!((block["returned"].clone(), block["candidates"].clone()), (json!(2), json!(2)));
}

/// The compiler emits one relation per granted table, bound to the table's bare name for the session's lifetime. Every retrieval arm reads through it.
// spec: authority.compose.registered-relation@4af661d3
#[test]
fn one_relation_per_granted_table_and_every_arm_reads_it() {
    let r = Reads::new();
    let s = r.session(&["research/notes", "research/vendor"], None, Some("public-cloud:us-east-1"));
    let names: Vec<&str> = s.relations().map(|rel| rel.name()).collect();
    assert_eq!(names, ["research/notes", "research/vendor"]);
    let ranked = r.face.retrieve(&s, &ask("research/", "solar battery storage"), Bounds::default()).unwrap();
    // The notes relation drops every row under this zone; the vendor arm answers.
    assert_eq!(column(&ranked, "_table"), [json!("research/vendor")]);
}

/// A table reached with no matching grant yields no rows.
// spec: authority.filter-rows.default-deny@5b48112f
#[test]
fn an_ungranted_table_yields_no_rows() {
    let r = Reads::new();
    let s = r.session(&["research/notes"], None, None);
    let ranked = r.face.retrieve(&s, &RetrieveRequest { limit: Some(50), ..ask("", "battery storage") }, Bounds::default()).unwrap();
    let tables = column(&ranked, "_table");
    assert!(!tables.is_empty());
    assert!(tables.iter().all(|t| t == "research/notes"), "{tables:?}");
}
