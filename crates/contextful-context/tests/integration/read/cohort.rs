//! `disclosure.bound-cohort` on the read face: a `cohort` model's source table answers only a
//! grant holding `read-individual`, and its published table refuses a coarser grouping.

use super::*;
use contextful_context::build::{build_released, BuildRequest};
use contextful_context::read::RetrieveRequest;
use contextful_core::disclosure::release::{Breakdown, DerivedTable};
use contextful_core::pipeline::declare::{collect, ManifestFile};
use contextful_core::pipeline::model::{collect_models, ModelSpec};
use contextful_core::read::pin::Pins;
use serde_json::Map;
use std::collections::BTreeMap;

/// A cohort model over `survey/responses`, one row per respondent, released per region and
/// period; a template reading the source rows, and templates grouping the published table
/// by its whole key and by one column of it.
const COHORT: &str = r#"
[[model]]
id = "survey/regional"
sql = "SELECT region, period, respondent_id, CAST(score AS BIGINT) AS score FROM \"survey/responses\""
unique_key = ["region", "period", "respondent_id"]

[model.contract]
version = "1.0.0"
columns = [{ name = "region", type = "utf8", nullable = false }, { name = "period", type = "utf8", nullable = false }, { name = "respondent_id", type = "utf8", nullable = false }, { name = "score", type = "int64", nullable = false }]

[model.disclosure]
grouping_allowlist = ["region", "period"]
contributor_key = "respondent_id"
min_group_size = 2
governance = "cohort"
"#;

const TEMPLATES: &str = r#"
[[query_templates]]
id = "responses_all"
sql = "SELECT respondent_id, score FROM \"survey/responses\" ORDER BY respondent_id"

[[query_templates]]
id = "regional_by_key"
sql = "SELECT region, period, sum(score) AS score FROM \"survey/regional\" GROUP BY region, period ORDER BY region, period"

[[query_templates]]
id = "regional_by_region"
sql = "SELECT region, sum(score) AS score FROM \"survey/regional\" GROUP BY region ORDER BY region"
"#;

fn spec() -> ModelSpec {
    let files = [ManifestFile { path: "contextful.toml".into(), text: COHORT.to_string() }];
    collect_models(&files, &collect(&files).unwrap()).unwrap().remove(0).spec
}

/// Sum each region and period's scores, dropping the respondent: the published rows.
fn per_group(breakdown: Breakdown) -> DerivedTable {
    let mut sums: BTreeMap<(String, String), i64> = BTreeMap::new();
    for row in &breakdown.rows {
        let key = (row["region"].as_str().unwrap().to_string(), row["period"].as_str().unwrap().to_string());
        *sums.entry(key).or_default() += row["score"].as_i64().unwrap();
    }
    let rows = sums.into_iter().map(|((region, period), score)| json!({ "region": region, "period": period, "score": score }).as_object().unwrap().clone()).collect();
    DerivedTable { name: "survey/regional".into(), columns: vec!["region".into(), "period".into(), "score".into()], rows }
}

/// The fixture store holding six respondents and the released cohort table, and the face
/// over it knowing the cohort model.
fn released() -> (Reads, Face) {
    let r = Reads::new();
    land_rows(
        &r.store,
        "survey/responses",
        "run-0001",
        json!([
            { "respondent_id": "p1", "region": "emea", "period": "q1", "score": 10, "comment": "solar battery storage" },
            { "respondent_id": "p2", "region": "emea", "period": "q1", "score": 20, "comment": "solar battery storage" },
            { "respondent_id": "p3", "region": "emea", "period": "q2", "score": 30, "comment": "battery storage" },
            { "respondent_id": "p4", "region": "emea", "period": "q2", "score": 40, "comment": "battery storage" },
            { "respondent_id": "p5", "region": "apac", "period": "q1", "score": 50, "comment": "storage" },
            { "respondent_id": "p6", "region": "apac", "period": "q1", "score": 60, "comment": "storage" },
        ]),
    );
    let spec = spec();
    let builder = Face::open(r.store.clone(), MANIFEST, pepper()).unwrap();
    let step = |_: &str, b: Breakdown| Ok(per_group(b));
    let req = BuildRequest { model: &spec, site_id: "site-a", started_at: at("2030-01-11T00:00:00Z"), completed_at: at("2030-01-11T00:00:00Z"), available_as_of: None };
    build_released(&builder, &req, &step).unwrap();
    let face = Face::open(r.store.clone(), &format!("{MANIFEST}{TEMPLATES}"), pepper()).unwrap().with_cohorts(&[spec]).unwrap();
    (r, face)
}

fn grant(actions: &[Action], tables: &[&str]) -> Grant {
    Grant { actions: actions.to_vec(), templates: Some(vec!["*".into()]), ..read(tables, None) }
}

fn authority(r: &Reads, grants: Vec<Grant>) -> AdmittedAuthority {
    r.authority(loop_subject("agent://research-loop"), grants)
}

fn template(face: &Face, authority: &AdmittedAuthority, id: &str) -> Result<Response, ReadFault> {
    let session = face.template_session(authority, &Request::default(), Bounds::default(), &Pins::default(), id).unwrap();
    face.execute_template(&session, id, &Map::new(), ReadOptions::default())
}

/// A query, template, retrieve, row, subscribe or recall read touching a per-individual table
/// raises `DisclosurePerIndividualUngranted` unless a grant holds `read-individual` over that
/// table; a derive task's input read is a row read.
// spec: disclosure.bound-cohort.per-individual-row@a1b24e75
#[test]
fn a_cohort_source_table_answers_only_a_grant_holding_read_individual() {
    let (r, face) = released();
    let query = |session: &Session, sql: &str| face.query(session, sql, ReadOptions::default());
    let retrieve = |session: &Session| face.retrieve(session, &RetrieveRequest { min_score: Some(0), ..RetrieveRequest::new("survey/responses", "battery storage", at("2030-02-01T00:00:00Z")) }, Bounds::default());

    // `read` over every table reaches the source table, and every read of it refuses.
    let reader = authority(&r, vec![grant(&[Action::Read], &["*"])]);
    let session = face.session(&reader, &Request::default(), Bounds::default()).unwrap();
    let message = refused_with(query(&session, "SELECT respondent_id FROM \"survey/responses\""), "DisclosurePerIndividualUngranted");
    assert!(message.contains("survey/responses") && message.contains("read-individual"), "{message}");
    refused_with(query(&session, "SELECT count(*) FROM \"survey/responses\""), "DisclosurePerIndividualUngranted");
    refused_with(template(&face, &reader, "responses_all"), "DisclosurePerIndividualUngranted");
    refused_with(retrieve(&session), "DisclosurePerIndividualUngranted");
    // The published cohort table and every other table read as before.
    assert_eq!(query(&session, "SELECT region FROM \"survey/regional\" ORDER BY region, period").unwrap().rows.len(), 3);
    assert_eq!(query(&session, "SELECT employee FROM \"hr/salaries\"").unwrap().rows.len(), 1);

    // `read-individual` over another table grants nothing here.
    let elsewhere = authority(&r, vec![grant(&[Action::Read], &["*"]), grant(&[Action::Read, Action::ReadIndividual], &["research/*"])]);
    let session = face.session(&elsewhere, &Request::default(), Bounds::default()).unwrap();
    refused_with(query(&session, "SELECT respondent_id FROM \"survey/responses\""), "DisclosurePerIndividualUngranted");

    // A grant holding `read-individual` over the source table reads its rows on each surface.
    let individual = authority(&r, vec![grant(&[Action::Read, Action::ReadIndividual], &["survey/responses"])]);
    let session = face.session(&individual, &Request::default(), Bounds::default()).unwrap();
    let rows = query(&session, "SELECT respondent_id FROM \"survey/responses\" ORDER BY respondent_id").unwrap();
    assert_eq!(column(&rows, "respondent_id"), ["p1", "p2", "p3", "p4", "p5", "p6"]);
    let templated = template(&face, &individual, "responses_all").unwrap();
    assert_eq!(templated.rows.len(), 6);
    let ranked = retrieve(&session).unwrap();
    assert!(!ranked.rows.is_empty(), "the ranked read returns the source rows");
}

/// A template or statement read grouping a `cohort` model's published table by a strict
/// subset of the group key its release records raises `DisclosureCohortWidening`; grouping by
/// the whole key or a superset of it reads.
// spec: disclosure.bound-cohort.cohort-widening@5979fff9
// spec: disclosure.bound-cohort.widening-groupings@dcde26da
#[test]
fn grouping_a_cohort_table_by_a_strict_subset_of_its_key_refuses() {
    let (r, face) = released();
    let reader = authority(&r, vec![grant(&[Action::Read], &["*"])]);
    let session = face.session(&reader, &Request::default(), Bounds::default()).unwrap();
    let query = |sql: &str| face.query(&session, sql, ReadOptions::default());

    let widening = [
        "SELECT region, sum(score) FROM \"survey/regional\" GROUP BY region",
        "SELECT sum(score) FROM \"survey/regional\"",
        "SELECT count(*) FROM \"survey/regional\" HAVING count(*) > 0",
        "SELECT region, period, sum(score) FROM \"survey/regional\" GROUP BY ROLLUP (region, period)",
        "SELECT region, sum(score) FROM \"survey/regional\" GROUP BY ALL",
        "SELECT region, period, sum(score) OVER (PARTITION BY region) FROM \"survey/regional\"",
        "SELECT region, sum(score) FROM \"survey/regional\" AS g GROUP BY g.region",
        "SELECT upper(region), period, sum(score) FROM \"survey/regional\" GROUP BY upper(region), period",
        "WITH c AS (SELECT * FROM \"survey/regional\") SELECT period, sum(score) FROM c GROUP BY period",
        "SELECT period, sum(score) FROM (SELECT region, period, score FROM \"survey/regional\") GROUP BY period",
        "SELECT n.note_id FROM \"research/notes\" AS n WHERE n.tenant IN (SELECT region FROM \"survey/regional\" GROUP BY region)",
    ];
    assert!(!widening.is_empty());
    for sql in widening {
        let message = refused_with(query(sql), "DisclosureCohortWidening");
        assert!(message.contains("survey/regional"), "{sql}: {message}");
    }
    refused_with(template(&face, &reader, "regional_by_region"), "DisclosureCohortWidening");

    let whole = query("SELECT region, period, sum(score) AS score FROM \"survey/regional\" GROUP BY region, period ORDER BY region, period").unwrap();
    assert_eq!(column(&whole, "score"), ["110", "30", "70"]);
    for sql in [
        "SELECT region, period, score, count(*) FROM \"survey/regional\" GROUP BY region, period, score",
        "SELECT region, period, sum(score) FROM \"survey/regional\" GROUP BY 1, 2",
        "SELECT region AS r, period, sum(score) FROM \"survey/regional\" GROUP BY r, period",
        "SELECT region, period, sum(score) FROM \"survey/regional\" GROUP BY ALL",
        "SELECT region, period, sum(score) OVER (PARTITION BY region, period) FROM \"survey/regional\"",
        "SELECT * FROM \"survey/regional\"",
        "SELECT tenant, count(*) FROM \"research/notes\" WHERE tenant NOT IN (SELECT region FROM \"survey/regional\") GROUP BY tenant",
    ] {
        assert!(query(sql).is_ok(), "{sql}: {:?}", query(sql));
    }
    assert_eq!(template(&face, &reader, "regional_by_key").unwrap().rows.len(), 3);
}

/// A row, native row or subscribe read of a cohort model's source table answers only a grant
/// holding `read-individual` over it, as a statement read does.
// spec: disclosure.bound-cohort.per-individual-row@a1b24e75
#[test]
fn row_and_subscribe_reads_of_a_cohort_source_answer_only_read_individual() {
    let (r, face) = released();
    let source = ["survey/responses".to_string()];
    let none = BTreeMap::new();

    let reader = authority(&r, vec![grant(&[Action::Read], &["*"])]);
    let session = face.session(&reader, &Request::default(), Bounds::default()).unwrap();
    let message = refused_with(face.rows(&session, "survey/responses", None), "DisclosurePerIndividualUngranted");
    assert!(message.contains("survey/responses"), "{message}");
    refused_with(face.source_rows(&session, "survey/responses", None, &["respondent_id"]), "DisclosurePerIndividualUngranted");
    refused_with(face.subscribe(&session, &source, &none), "DisclosurePerIndividualUngranted");
    assert_eq!(face.rows(&session, "survey/regional", None).unwrap().rows.len(), 3);

    let individual = authority(&r, vec![grant(&[Action::Read, Action::ReadIndividual], &["survey/responses"])]);
    let session = face.session(&individual, &Request::default(), Bounds::default()).unwrap();
    assert_eq!(face.rows(&session, "survey/responses", None).unwrap().rows.len(), 6);
    assert_eq!(face.source_rows(&session, "survey/responses", None, &["respondent_id"]).unwrap().rows.len(), 6);
    let page = face.subscribe(&session, &source, &none).unwrap();
    assert!(!page["messages"].as_array().unwrap().is_empty(), "{page}");
}

/// A derive output whose parent is a cohort source table holds per-individual rows, and so
/// does an output derived from that output: each answers only `read-individual`.
// spec: disclosure.bound-cohort.derived-individual@b63ba1cf
#[test]
fn a_derived_output_of_a_cohort_source_answers_only_read_individual() {
    let (r, _) = released();
    land_rows(&r.store, "survey/passages", "run-0002", json!([{ "respondent_id": "p1", "text": "solar" }]));
    land_rows(&r.store, "survey/topics", "run-0003", json!([{ "respondent_id": "p1", "topic": "energy" }]));
    let derives = [
        (vec!["research/notes".to_string(), "survey/passages".to_string()], vec!["survey/topics".to_string()]),
        (vec!["survey/responses".to_string()], vec!["survey/passages".to_string()]),
    ];
    let face = Face::open(r.store.clone(), MANIFEST, pepper()).unwrap().with_cohorts(&[spec()]).unwrap().with_derived(&derives);
    let derived = ["survey/passages", "survey/topics"];

    let reader = authority(&r, vec![grant(&[Action::Read], &["*"])]);
    let session = face.session(&reader, &Request::default(), Bounds::default()).unwrap();
    assert!(!derived.is_empty());
    for table in derived {
        let message = refused_with(face.rows(&session, table, None), "DisclosurePerIndividualUngranted");
        assert!(message.contains(table), "{message}");
        refused_with(face.query(&session, &format!("SELECT * FROM \"{table}\""), ReadOptions::default()), "DisclosurePerIndividualUngranted");
    }
    assert!(!face.rows(&session, "research/notes", None).unwrap().rows.is_empty());

    let individual = authority(&r, vec![grant(&[Action::Read, Action::ReadIndividual], &["survey/passages", "survey/topics"])]);
    let session = face.session(&individual, &Request::default(), Bounds::default()).unwrap();
    for table in derived {
        assert_eq!(face.rows(&session, table, None).unwrap().rows.len(), 1, "{table}");
    }
}

/// A claims table synthesized from a cohort source holds per-individual claims: recall of it,
/// and a knowledge card's claims through recall, answer only a grant holding `read-individual`.
// spec: disclosure.bound-cohort.per-individual-row@a1b24e75
#[test]
fn recall_of_a_claims_table_synthesized_from_a_cohort_source_answers_only_read_individual() {
    use contextful_context::read::RecallRequest;
    use contextful_core::store::reconcile::ColumnType;
    let (r, _) = released();
    let facts = "[[table]]\nname = \"survey/facts\"\nshape = \"memory_facts\"\ncolumns = [\"claim_id\", \"subject\", \"predicate\", \"object\", \"scope\", \"tier\", \"confidence\", \"valid_from\", \"valid_to\", \"evidence\", \"superseded_by\", \"grant_id\", \"agent\"]\n";
    let evidence = serde_json::to_string(&json!([{ "table": "survey/responses", "run": "run-0001", "seq": 0 }])).unwrap();
    let claim = json!({ "claim_id": "c1", "subject": "p1", "predicate": "prefers", "object": "solar", "scope": null, "tier": "derived",
        "confidence": 0.6, "valid_from": "2030-01-05T00:00:00Z", "valid_to": null, "evidence": evidence, "superseded_by": null, "grant_id": "g", "agent": null });
    let types = [("valid_from", ColumnType::Timestamp), ("valid_to", ColumnType::Timestamp), ("confidence", ColumnType::Float64), ("scope", ColumnType::Utf8), ("superseded_by", ColumnType::Utf8), ("agent", ColumnType::Utf8)]
        .iter()
        .map(|(c, t)| (c.to_string(), t.clone()))
        .collect();
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "memory-1".into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, authored_agent: None, taint: None },
        committed_at: at("2030-01-10T00:00:00Z"),
    };
    let decl = TableDecl { primary_key: Some(vec!["claim_id".into()]), ..TableDecl::named("survey/facts") };
    land(&r.store, &decl, &Batch { rows: vec![claim.as_object().unwrap().clone()], types }, &ctx).unwrap();
    let lineage = [(vec!["survey/responses".to_string()], vec!["survey/facts".to_string(), "survey/facts_dead_letter".to_string()])];
    let face = Face::open(r.store.clone(), &format!("{MANIFEST}{facts}"), pepper()).unwrap().with_cohorts(&[spec()]).unwrap().with_derived(&lineage);
    let request = RecallRequest::new("survey/facts", "p1", at("2030-06-01T00:00:00Z"));

    let reader = authority(&r, vec![grant(&[Action::Read], &["*"])]);
    let session = face.session(&reader, &Request::default(), Bounds::default()).unwrap();
    let message = refused_with(face.recall(&session, &request), "DisclosurePerIndividualUngranted");
    assert!(message.contains("survey/facts"), "{message}");

    let individual = authority(&r, vec![grant(&[Action::Read, Action::ReadIndividual], &["survey/facts", "survey/responses"])]);
    let session = face.session(&individual, &Request::default(), Bounds::default()).unwrap();
    assert_eq!(face.recall(&session, &request).unwrap().rows.len(), 1);
}
