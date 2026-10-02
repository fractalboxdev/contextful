//! The keyed read: claims of one subject at an observed instant, under an ingest bound,
//! through the evidence gate.

use super::support::*;
use contextful_context::read::{ReadFault, RecallRequest};
use contextful_core::grant::Action;
use contextful_core::memory::revise::{Claim, Tier};
use contextful_core::memory::synthesize::{CandidateClaim, EvidenceRef};
use contextful_core::memory::MemoryError;
use contextful_core::read::respond::Response;
use contextful_core::read::Refusal;
use contextful_core::store::bound_time::Bound;
use contextful_core::store::lay_out::NodeId;
use contextful_memory::claims::{Landing, Writer};
use contextful_memory::write::{write_observed, Observation};
use contextful_policy::enforce::session::Request;
use contextful_policy::verify::AdmittedAuthority;
use serde_json::{json, Value};

fn candidate(subject: &str, object: &str, run: &str) -> CandidateClaim {
    CandidateClaim {
        subject: subject.into(),
        predicate: "cfo".into(),
        object: object.into(),
        scope: None,
        confidence: 1.0,
        evidence: vec![EvidenceRef::row("research/notes", run, 0)],
    }
}

fn observed(at_: &str, key: &str) -> Observation {
    Observation { observed_at: Some(at(at_)), dedup_key: Some(key.into()) }
}

/// Dana is CFO from 2030-01-01, written on the 11th; Lee from 2030-03-01, written on the
/// 12th, which retires Dana at that instant. Another subject shares the prefix.
fn history(f: &Fixture) -> AdmittedAuthority {
    let (writer, node) = (f.writer(), NodeId::parse("memory-a").unwrap());
    land_rows(&f.face, "research/notes", "run-0001", json!([{ "note_id": "n1", "text": "Dana is Acme's CFO." }]));
    land_rows(&f.face, "research/notes", "run-0002", json!([{ "note_id": "n2", "text": "Acme appointed Lee as CFO." }]));
    let write = |c: CandidateClaim, o: Observation, now: &str| {
        write_observed(&f.face, &writer, "memory/facts", c, &o, &node, at(now), &super::synthesize::admit).unwrap()
    };
    write(candidate("acme", "Dana", "run-0001"), observed("2030-01-01T00:00:00Z", "evt-1"), "2030-01-11T00:00:00Z");
    let lee = write(candidate("acme", "Lee", "run-0002"), observed("2030-03-01T00:00:00Z", "evt-2"), "2030-01-12T00:00:00Z");
    assert_eq!(lee.retired.len(), 1);
    write(candidate("acme labs", "Kim", "run-0001"), observed("2030-01-01T00:00:00Z", "evt-3"), "2030-01-12T01:00:00Z");
    writer
}

fn reader(f: &Fixture) -> AdmittedAuthority {
    f.authority("agent://research-loop", &[Action::Read], &["research/*", "memory/*"])
}

fn recall(f: &Fixture, who: &AdmittedAuthority, request: &RecallRequest) -> Result<Response, ReadFault> {
    let s = f.face.session(who, &Request::default(), request.bounds()).unwrap();
    f.face.recall(&s, request)
}

fn keyed(table: &str, subject: &str, observed_at: Option<&str>) -> RecallRequest {
    RecallRequest {
        observed_at: observed_at.map(|o| Bound::parse(o).unwrap()),
        ..RecallRequest::new(table, subject, at("2030-06-01T00:00:00Z"))
    }
}

fn column(r: &Response, name: &str) -> Vec<Value> {
    let i = r.columns.iter().position(|c| c == name).unwrap_or_else(|| panic!("no column {name}: {:?}", r.columns));
    r.rows.iter().map(|row| row[i].clone()).collect()
}

/// `memory.recall` takes `table`, `subject`, `observed_at`, `as_of_ingest` and `limit`, and returns the claims whose `subject` equals `subject` exactly and whose `valid_from` and `valid_to` cover `observed_at` as {{store.bound-time.valid-as-of}} does.
// spec: read.recall.keyed@d58fdab3
#[test]
fn a_keyed_recall_returns_the_subjects_claims_valid_at_the_observed_instant() {
    let f = Fixture::new();
    history(&f);
    let r = reader(&f);
    let objects = |subject: &str, when: &str| column(&recall(&f, &r, &keyed("memory/facts", subject, Some(when))).unwrap(), "object");
    assert_eq!(objects("acme", "2030-02-01T00:00:00Z"), [json!("Dana")]);
    assert_eq!(objects("acme", "2030-04-01T00:00:00Z"), [json!("Lee")]);
    // The successor's start is the prior's exclusive end.
    assert_eq!(objects("acme", "2030-03-01T00:00:00Z"), [json!("Lee")]);
    assert!(objects("acme", "2029-12-01T00:00:00Z").is_empty());
    // An exact match: neither case nor a shared prefix reaches another subject.
    assert!(objects("Acme", "2030-02-01T00:00:00Z").is_empty());
    assert_eq!(objects("acme labs", "2030-02-01T00:00:00Z"), [json!("Kim")]);
}

/// `as_of_ingest` bounds the read as {{store.bound-time.as-of}} does; absent, the read takes the latest committed state, and an absent `observed_at` the call's instant. `contextful.bounds` echoes each supplied bound under its argument name, with a per-name `inclusive` map.
// spec: read.recall.keyed-clocks@edef8aaa
#[test]
fn as_of_ingest_reads_what_was_known_then() {
    let f = Fixture::new();
    history(&f);
    let r = reader(&f);
    // Before Lee's write landed, Dana stood open-ended.
    let then = RecallRequest {
        as_of_ingest: Some(Bound::parse("2030-01-11T12:00:00Z").unwrap()),
        ..keyed("memory/facts", "acme", Some("2030-04-01T00:00:00Z"))
    };
    let known = recall(&f, &r, &then).unwrap();
    assert_eq!(column(&known, "object"), [json!("Dana")]);
    assert_eq!(
        known.blocks["contextful.bounds"],
        json!({
            "as_of_ingest": "2030-01-11T12:00:00.000000000Z",
            "observed_at": "2030-04-01T00:00:00.000000000Z",
            "inclusive": { "as_of_ingest": true, "observed_at": true }
        })
    );
    // A date literal reads exclusive; each bound echoes its own inclusivity.
    let dated = RecallRequest {
        as_of_ingest: Some(Bound::parse("2030-01-11T12:00:00Z").unwrap()),
        ..keyed("memory/facts", "acme", Some("2030-03-31"))
    };
    assert_eq!(
        recall(&f, &r, &dated).unwrap().blocks["contextful.bounds"],
        json!({
            "as_of_ingest": "2030-01-11T12:00:00.000000000Z",
            "observed_at": "2030-04-01T00:00:00.000000000Z",
            "inclusive": { "as_of_ingest": true, "observed_at": false }
        })
    );
    // No observed instant: the call's own, where Lee holds; no bound supplied, no echo.
    let now = recall(&f, &r, &keyed("memory/facts", "acme", None)).unwrap();
    assert_eq!(column(&now, "object"), [json!("Lee")]);
    assert!(!now.blocks.contains_key("contextful.bounds"), "{:?}", now.blocks);
}

/// A claim a successor retired still answers an `observed_at` inside its validity; the keyed read filters on validity alone, never on `superseded_by`.
// spec: read.recall.keyed-history@6a407cb7
#[test]
fn a_retired_claim_answers_inside_its_interval() {
    let f = Fixture::new();
    history(&f);
    let got = recall(&f, &reader(&f), &keyed("memory/facts", "acme", Some("2030-02-15T00:00:00Z"))).unwrap();
    assert_eq!(column(&got, "object"), [json!("Dana")]);
    assert!(column(&got, "superseded_by")[0].is_string(), "{:?}", got.rows);
    assert_eq!(column(&got, "valid_to"), [json!("2030-03-01T00:00:00Z")]);
}

/// Every keyed claim passes {{read.recall.evidence-unresolved}} and {{read.recall.evidence-references}}, and the response carries {{read.recall.suppression-count}}.
// spec: read.recall.keyed-gate@184d7022
#[test]
fn a_keyed_claim_passes_the_evidence_gate() {
    let f = Fixture::new();
    history(&f);
    let outsider = f.authority("agent://outsider", &[Action::Read], &["memory/*"]);
    let withheld = recall(&f, &outsider, &keyed("memory/facts", "acme", Some("2030-02-01T00:00:00Z"))).unwrap();
    assert!(withheld.rows.is_empty());
    assert_eq!(
        withheld.blocks["contextful.recall"],
        json!({ "suppressed": { "MemoryEvidenceUnresolved": 1, "MemoryEvidenceOverflow": 0 } })
    );
    assert!(!serde_json::to_string(&withheld.to_json()).unwrap().contains("Dana"));
    let seen = recall(&f, &reader(&f), &keyed("memory/facts", "acme", Some("2030-02-01T00:00:00Z"))).unwrap();
    assert_eq!(seen.blocks["contextful.recall"]["suppressed"]["MemoryEvidenceUnresolved"], json!(0));
}

/// The keyed read gates claims in order and stops once it keeps one claim past the row ceiling, so the suppression count covers the claims gated before that stop.
// spec: read.recall.keyed-window@fccd87ec
#[test]
fn the_keyed_read_stops_gating_one_claim_past_the_ceiling() {
    let f = Fixture::new();
    let writer = history(&f);
    let w = Writer::of(&writer);
    let node = NodeId::parse("memory-a").unwrap();
    let claim = |id: &str, object: &str, tier: Tier, from: &str, table: &str| Claim {
        claim_id: id.into(),
        subject: "acme".into(),
        predicate: "ceo".into(),
        object: object.into(),
        scope: None,
        tier,
        confidence: 0.6,
        valid_from: at(from),
        valid_to: None,
        evidence: vec![EvidenceRef::row(table, "run-0001", 0)],
        superseded_by: None,
        grant_id: w.grant_id.clone(),
        agent: w.agent.clone(),
    };
    // Two unresolvable claims rank ahead of Dana, one behind every resolvable claim.
    let claims = [
        claim("c-x1", "Xan", Tier::Curated, "2030-01-30T00:00:00Z", "nowhere/notes"),
        claim("c-x2", "Yul", Tier::Curated, "2030-01-29T00:00:00Z", "nowhere/notes"),
        claim("c-b", "Ola", Tier::Derived, "2030-01-20T00:00:00Z", "research/notes"),
        claim("c-z", "Zed", Tier::Researched, "2030-01-20T00:00:00Z", "nowhere/notes"),
    ];
    Landing { node: &node, at: at("2030-01-13T00:00:00Z"), writer: &w, run_id: "memory-mixed".into(), boundary: &super::synthesize::admit, taint: None }
        .commit(&f.face, "memory/facts", &claims, &[])
        .unwrap();
    let r = reader(&f);
    let unresolved = |resp: &Response| resp.blocks["contextful.recall"]["suppressed"]["MemoryEvidenceUnresolved"].clone();
    // The first page holds two suppressed claims; the read pages on until it keeps two.
    let one = recall(&f, &r, &RecallRequest { limit: Some(1), ..keyed("memory/facts", "acme", Some("2030-02-01T00:00:00Z")) }).unwrap();
    assert_eq!(column(&one, "object"), [json!("Dana")]);
    assert!(one.truncated);
    assert_eq!(unresolved(&one), json!(2));
    let all = recall(&f, &r, &keyed("memory/facts", "acme", Some("2030-02-01T00:00:00Z"))).unwrap();
    assert_eq!(column(&all, "object"), [json!("Dana"), json!("Ola")]);
    assert!(!all.truncated);
    assert_eq!(unresolved(&all), json!(3));
}

/// Keyed claims order by tier, `curated` first, then `valid_from`, newest first, then `claim_id`; `limit` and the table's ceilings bound them under {{read.respond.row-ceiling}}.
// spec: read.recall.keyed-order@05cef173
#[test]
fn keyed_claims_order_tier_first_and_meet_the_limit() {
    let f = Fixture::new();
    let writer = history(&f);
    let w = Writer::of(&writer);
    let node = NodeId::parse("memory-a").unwrap();
    let derived = |id: &str, object: &str, from: &str| Claim {
        claim_id: id.into(),
        subject: "acme".into(),
        predicate: "ceo".into(),
        object: object.into(),
        scope: None,
        tier: Tier::Derived,
        confidence: 0.6,
        valid_from: at(from),
        valid_to: None,
        evidence: vec![EvidenceRef::row("research/notes", "run-0001", 0)],
        superseded_by: None,
        grant_id: w.grant_id.clone(),
        agent: w.agent.clone(),
    };
    Landing { node: &node, at: at("2030-01-13T00:00:00Z"), writer: &w, run_id: "memory-derived".into(), boundary: &super::synthesize::admit, taint: None }
        .commit(&f.face, "memory/facts", &[derived("c-b", "Ola", "2030-01-20T00:00:00Z"), derived("c-a", "Pat", "2030-01-20T00:00:00Z"), derived("c-z", "Ray", "2030-01-25T00:00:00Z")], &[])
        .unwrap();
    let r = reader(&f);
    let all = recall(&f, &r, &keyed("memory/facts", "acme", Some("2030-02-01T00:00:00Z"))).unwrap();
    assert_eq!(column(&all, "object"), [json!("Dana"), json!("Ray"), json!("Pat"), json!("Ola")]);
    assert_eq!(column(&all, "tier"), [json!("curated"), json!("derived"), json!("derived"), json!("derived")]);
    assert!(!all.truncated);
    let two = recall(&f, &r, &RecallRequest { limit: Some(2), ..keyed("memory/facts", "acme", Some("2030-02-01T00:00:00Z")) }).unwrap();
    assert_eq!(column(&two, "object"), [json!("Dana"), json!("Ray")]);
    assert!(two.truncated);
}

/// A granted `table` declaring a shape other than `memory_facts`, or no memory shape, raises `MemoryRecallNotClaims`.
// spec: read.recall.keyed-not-claims@8a12b788
#[test]
fn a_keyed_recall_over_a_table_holding_no_claims_refuses() {
    let f = Fixture::new();
    history(&f);
    let r = reader(&f);
    for table in ["memory/episodes", "research/notes"] {
        match recall(&f, &r, &keyed(table, "acme", None)) {
            Err(ReadFault::Refused(Refusal::Memory(MemoryError::RecallNotClaims(why)))) => assert!(why.contains(table), "{why}"),
            other => panic!("{table}: {other:?}"),
        }
    }
    // An ungranted table refuses as unknown before its shape is consulted.
    let narrow = f.authority("agent://narrow", &[Action::Read], &["research/*"]);
    let refused = recall(&f, &narrow, &keyed("memory/facts", "acme", None)).unwrap_err();
    assert_eq!(refused.refusal().map(|r| r.identifier()), Some("EnforceUnknownRelation"), "{refused}");
}
