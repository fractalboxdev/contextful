//! `memory.recall` through the face: how the evidence gate reads a claim's citations.

use super::*;
use contextful_context::read::RecallRequest;
use contextful_core::store::bound_time::Bound;
use contextful_core::store::reconcile::ColumnType;

const FACTS: &str = "[[table]]\nname = \"research/insights\"\nshape = \"memory_facts\"\ncolumns = [\"claim_id\", \"subject\", \"predicate\", \"object\", \"scope\", \"tier\", \"confidence\", \"valid_from\", \"valid_to\", \"evidence\", \"superseded_by\", \"grant_id\", \"agent\"]\n";

/// Land `claims` of subject `acme`, each `(claim_id, evidence)`, into `research/insights`.
fn land_claims(r: &Reads, claims: &[(&str, Vec<Value>)]) {
    let rows = claims
        .iter()
        .map(|(id, evidence)| {
            json!({ "claim_id": id, "subject": "acme", "predicate": "cfo", "object": id, "scope": null, "tier": "derived",
                "confidence": 0.6, "valid_from": "2030-01-05T00:00:00Z", "valid_to": null,
                "evidence": serde_json::to_string(evidence).unwrap(), "superseded_by": null, "grant_id": "g", "agent": null })
            .as_object()
            .unwrap()
            .clone()
        })
        .collect();
    let types = [("valid_from", ColumnType::Timestamp), ("valid_to", ColumnType::Timestamp), ("confidence", ColumnType::Float64),
        ("scope", ColumnType::Utf8), ("superseded_by", ColumnType::Utf8), ("agent", ColumnType::Utf8)]
        .iter()
        .map(|(c, t)| (c.to_string(), t.clone()))
        .collect();
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "memory-1".into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint: None },
        committed_at: at("2030-01-10T00:00:00Z"),
    };
    let decl = TableDecl { primary_key: Some(vec!["claim_id".into()]), ..TableDecl::named("research/insights") };
    land(&r.store, &decl, &Batch { rows, types }, &ctx).unwrap();
}

fn cite(run: &str, seq: i64) -> Value {
    json!({ "table": "research/sources", "run": run, "seq": seq })
}

/// The keyed read resolves a claim's citations into one table and run with one statement, listed among the executed SQL under `internals: true`.
// spec: read.recall.keyed-evidence-batch@f352668d
#[test]
fn a_claims_citations_into_one_run_resolve_in_one_statement() {
    let r = Reads::with_manifest(&format!("{MANIFEST}\n{FACTS}"));
    land_rows(&r.store, "research/sources", "src-1", Value::Array((0..300).map(|i| json!({ "source_id": format!("s{i}") })).collect()));
    land_rows(&r.store, "research/sources", "src-2", json!([{ "source_id": "t0" }, { "source_id": "t1" }]));
    // `a` cites every row of two runs; `b` cites a sequence its run never landed.
    let mut a: Vec<Value> = (0..300).map(|i| cite("src-1", i)).collect();
    a.extend([cite("src-2", 0), cite("src-2", 1)]);
    land_claims(&r, &[("a", a), ("b", vec![cite("src-2", 7)])]);
    let s = r.session(&["research/*"], None, None);
    let request = RecallRequest {
        observed_at: Some(Bound::parse("2030-02-01T00:00:00Z").unwrap()),
        internals: true,
        ..RecallRequest::new("research/insights", "acme", at("2030-06-01T00:00:00Z"))
    };
    let got = r.face.recall(&s, &request).unwrap();
    assert_eq!(column(&got, "claim_id"), [json!("a")]);
    assert_eq!(column(&got, "evidence_omitted"), [json!(46)]);
    assert_eq!(got.blocks["contextful.recall"]["suppressed"]["MemoryEvidenceUnresolved"], json!(1));
    let sql = got.blocks["contextful.internals"]["sql"].as_str().unwrap_or_default().to_string();
    let evidence: Vec<&str> = sql.lines().filter(|l| l.contains("\"_row_seq\"")).collect();
    // Two runs behind `a`, one behind `b`.
    assert_eq!(evidence.len(), 3, "{sql}");
}
