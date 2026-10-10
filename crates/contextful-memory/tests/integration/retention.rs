//! Retention: recorded validity holds until expiry or erasure, and ranking decay is an
//! opt-in per table.

use super::support::*;
use contextful_context::read::{Face, RecallRequest, RetrieveRequest};
use contextful_context::Store;
use contextful_core::grant::Action;
use contextful_core::memory::synthesize::{CandidateClaim, EvidenceRef};
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::lay_out::NodeId;
use contextful_memory::write::{write_observed, Observation};
use contextful_policy::enforce::mask::Pepper;
use contextful_policy::enforce::session::Request;
use serde_json::{json, Value};

const ANCHOR: &str = "2030-06-01T00:00:00Z";

/// The manifest, its `memory/facts` declaring `decay_half_life` when `half_life` is set.
fn manifest(half_life: Option<&str>) -> String {
    match half_life {
        Some(h) => MANIFEST.replace("shape = \"memory_facts\"\n", &format!("shape = \"memory_facts\"\ndecay_half_life = \"{h}\"\n")),
        None => MANIFEST.to_string(),
    }
}

fn open(dir: &tempfile::TempDir, half_life: Option<&str>) -> Result<Face, contextful_context::read::ReadFault> {
    Face::open(Store::open(dir.path(), "research").unwrap(), &manifest(half_life), Pepper::resolve(|_| None))
}

fn store(half_life: Option<&str>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let face = open(&dir, half_life).unwrap();
    let f = Fixture::over(dir, face);
    land_rows(&f.face, "research/notes", "run-0001", json!([{ "note_id": "n1", "text": "filings" }]));
    let (writer, node) = (f.writer(), NodeId::parse("memory-a").unwrap());
    // An old claim matching the whole question, a fresh one matching one word of it, and a
    // fresh one matching that word in a longer subject, which ranks last either way.
    let claims = [
        ("acme holdings", "Dana Rivera", "2020-01-01T00:00:00Z", "2030-05-31T12:00:00Z"),
        ("acme", "Lee", "2030-05-31T00:00:00Z", "2030-05-31T13:00:00Z"),
        ("acme widgets corp", "Kim", "2030-05-31T00:00:00Z", "2030-05-31T14:00:00Z"),
    ];
    for (subject, object, observed, now) in claims {
        let claim = CandidateClaim {
            subject: subject.into(),
            predicate: "cfo".into(),
            object: object.into(),
            scope: None,
            confidence: 1.0,
            evidence: vec![EvidenceRef::row("research/notes", "run-0001", 0)],
        };
        let observation = Observation { observed_at: Some(at(observed)), dedup_key: None };
        write_observed(&f.face, &writer, "memory/facts", claim, &observation, &node, at(now), &super::synthesize::admit).unwrap();
    }
    f
}

fn ranked(f: &Fixture) -> Vec<Value> {
    let reader = f.authority("agent://research-loop", &[Action::Read], &["research/*", "memory/*"]);
    let session = f.face.session(&reader, &Request::default(), Bounds::default()).unwrap();
    let r = f.face.retrieve(&session, &RetrieveRequest::new("memory/", "acme holdings", at(ANCHOR)), Bounds::default()).unwrap();
    r.rows.iter().map(|row| row[1]["object"].clone()).collect()
}

fn recalled(f: &Fixture, subject: &str) -> (Value, Value, Value) {
    let reader = f.authority("agent://research-loop", &[Action::Read], &["research/*", "memory/*"]);
    let request = RecallRequest::new("memory/facts", subject, at(ANCHOR));
    let session = f.face.session(&reader, &Request::default(), request.bounds()).unwrap();
    let r = f.face.recall(&session, &request).unwrap();
    let col = |name: &str| r.columns.iter().position(|c| c == name).unwrap();
    let row = &r.rows[0];
    (row[col("object")].clone(), row[col("valid_from")].clone(), row[col("valid_to")].clone())
}

/// Claims retain their recorded validity until explicit expiry or erasure; a claims table declaring `decay_half_life` halves a claim's ranked score per half-life since its `valid_from`, never deleting a claim or changing its validity.
// spec: read.revise.retention-default@3b870a73
#[test]
fn decay_reorders_ranking_only_where_a_half_life_is_declared() {
    let hard = store(None);
    assert_eq!(ranked(&hard), [json!("Dana Rivera"), json!("Lee"), json!("Kim")], "no declared half-life, no decay");

    let fading = store(Some("365d"));
    assert_eq!(ranked(&fading), [json!("Lee"), json!("Dana Rivera"), json!("Kim")], "a decayed claim ranks lower and still answers");
    for f in [&hard, &fading] {
        let (object, from, to) = recalled(f, "acme holdings");
        assert_eq!(object, json!("Dana Rivera"));
        assert!(from.as_str().is_some_and(|s| s.starts_with("2020-01-01")), "{from}");
        assert_eq!(to, Value::Null, "validity stays open-ended");
    }

    let dir = tempfile::tempdir().unwrap();
    assert!(open(&dir, Some("soon")).is_err(), "an unreadable half-life refuses the declaration");
}
