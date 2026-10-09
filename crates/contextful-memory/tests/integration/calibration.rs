//! Keyed recall labels each claim's confidence from the claims table's settled outcomes.

use super::support::*;
use contextful_context::read::{Face, RecallRequest};
use contextful_context::Store;
use contextful_core::grant::Action;
use contextful_core::memory::synthesize::{CandidateClaim, EvidenceRef};
use contextful_core::store::lay_out::NodeId;
use contextful_memory::write::{write_observed, Observation};
use contextful_policy::enforce::mask::Pepper;
use contextful_policy::enforce::session::Request;
use contextful_policy::verify::AdmittedAuthority;
use serde_json::{json, Value};

const OUTCOMES: &str = "research/outcomes";

fn labelled() -> Fixture {
    let manifest = format!(
        "{}\n[[pipeline.tables]]\nname = \"{OUTCOMES}\"\n",
        MANIFEST.replace("shape = \"memory_facts\"\n", &format!("shape = \"memory_facts\"\nlabels = \"{OUTCOMES}\"\n"))
    );
    let dir = tempfile::tempdir().unwrap();
    let face = Face::open(Store::open(dir.path(), "research").unwrap(), &manifest, Pepper::resolve(|_| None)).unwrap();
    let f = Fixture::over(dir, face);
    land_rows(&f.face, "research/notes", "run-0001", json!([{ "note_id": "n1", "text": "filings" }]));
    let (writer, node) = (f.writer(), NodeId::parse("memory-a").unwrap());
    let write = |subject: &str, predicate: &str, confidence: f64, i: u64| {
        let claim = CandidateClaim {
            subject: subject.into(),
            predicate: predicate.into(),
            object: "yes".into(),
            scope: None,
            confidence,
            evidence: vec![EvidenceRef::row("research/notes", "run-0001", 0)],
        };
        let observation = Observation { observed_at: Some(at("2030-01-11T00:00:00Z")), dedup_key: None };
        let now = at("2030-01-11T00:00:00Z").plus_secs(i);
        write_observed(&f.face, &writer, "memory/facts", claim, &observation, &node, now, &super::synthesize::admit).unwrap().claim.unwrap().claim_id
    };
    // Ten settled `ships` claims emitted at 0.9 hold half the time.
    let settled: Vec<Value> = (0..10)
        .map(|i| json!({ "prediction_id": write(&format!("vendor-{i}"), "ships", 0.9, i), "verdict": i % 2 == 0 }))
        .collect();
    write("acme", "ships", 0.9, 20);
    write("acme", "hires", 0.7, 21);
    land_rows(&f.face, OUTCOMES, "settle-1", Value::Array(settled));
    f
}

fn recall(f: &Fixture, who: &AdmittedAuthority) -> Vec<(Value, Value, Value)> {
    let request = RecallRequest::new("memory/facts", "acme", at("2030-06-01T00:00:00Z"));
    let session = f.face.session(who, &Request::default(), request.bounds()).unwrap();
    let r = f.face.recall(&session, &request).unwrap();
    let col = |name: &str| r.columns.iter().position(|c| c == name).unwrap_or_else(|| panic!("no {name} in {:?}", r.columns));
    let mut rows: Vec<(Value, Value, Value)> =
        r.rows.iter().map(|row| (row[col("predicate")].clone(), row[col("confidence")].clone(), row[col("confidence_label")].clone())).collect();
    rows.sort_by_key(|(p, _, _)| p.to_string());
    rows
}

/// Each keyed claim carries `confidence_label`, `calibrated` or `uncalibrated` under {{read.synthesize.confidence-calibration}}, fitted on its table's declared `labels` table joined `prediction_id` to `claim_id`, read through the caller's session.
// spec: read.recall.confidence-label@1d497777
#[test]
fn recall_labels_confidence_from_the_declared_outcome_table() {
    let f = labelled();
    let reader = f.authority("agent://research-loop", &[Action::Read], &["research/*", "memory/*"]);
    let rows = recall(&f, &reader);
    assert_eq!(rows[0], (json!("hires"), json!(0.7), json!("uncalibrated")), "`hires` has no settled outcome");
    // Settlement order follows `prediction_id`; the eight fitted outcomes hold four or five times.
    assert_eq!((&rows[1].0, &rows[1].2), (&json!("ships"), &json!("calibrated")));
    assert!([0.5, 0.625].contains(&rows[1].1.as_f64().unwrap()), "a validated map rescales `ships`: {rows:?}");
    // A caller that cannot read the outcomes fits nothing.
    let blind = f.authority("agent://research-loop", &[Action::Read], &["research/notes", "memory/*"]);
    assert_eq!(recall(&f, &blind), [(json!("hires"), json!(0.7), json!("uncalibrated")), (json!("ships"), json!(0.9), json!("uncalibrated"))]);

    // A table declaring no `labels` reports every claim uncalibrated.
    let plain = Fixture::new();
    land_rows(&plain.face, "research/notes", "run-0001", json!([{ "note_id": "n1" }]));
    let claim = CandidateClaim {
        subject: "acme".into(),
        predicate: "ships".into(),
        object: "yes".into(),
        scope: None,
        confidence: 0.9,
        evidence: vec![EvidenceRef::row("research/notes", "run-0001", 0)],
    };
    let node = NodeId::parse("memory-a").unwrap();
    write_observed(&plain.face, &plain.writer(), "memory/facts", claim, &Observation::default(), &node, at("2030-01-11T00:00:00Z"), &super::synthesize::admit).unwrap();
    let reader = plain.authority("agent://research-loop", &[Action::Read], &["research/*", "memory/*"]);
    assert_eq!(recall(&plain, &reader), [(json!("ships"), json!(0.9), json!("uncalibrated"))]);
}
