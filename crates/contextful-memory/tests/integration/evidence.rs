//! Evidence into a keyed table: a claim cites a row version, and a later version of the
//! same key, folded or not, keeps the claim served.

use super::support::*;
use contextful_context::fold::fold;
use contextful_context::read::RecallRequest;
use contextful_core::grant::Action;
use contextful_core::memory::synthesize::{CandidateClaim, EvidenceRef};
use contextful_core::memory::MemoryError;
use contextful_core::read::respond::Response;
use contextful_core::store::lay_out::NodeId;
use contextful_memory::synthesize::Pass;
use contextful_memory::write::write_claim;
use contextful_memory::MemoryFault;
use contextful_policy::enforce::session::Request;
use contextful_policy::verify::AdmittedAuthority;
use serde_json::{json, Value};

const ACCOUNTS: &str = "research/accounts";

fn cites(run: &str) -> CandidateClaim {
    CandidateClaim {
        subject: "acme".into(),
        predicate: "cfo".into(),
        object: "Dana".into(),
        scope: None,
        confidence: 1.0,
        evidence: vec![EvidenceRef::row(ACCOUNTS, run, 0)],
    }
}

fn version(f: &Fixture, run: &str, cfo: &str) {
    land_rows(&f.face, ACCOUNTS, run, json!([{ "account_id": "acme", "cfo": cfo }]));
}

fn recall(f: &Fixture, who: &AdmittedAuthority) -> Response {
    let request = RecallRequest::new("memory/facts", "acme", at("2030-06-01T00:00:00Z"));
    let s = f.face.session(who, &Request::default(), request.bounds()).unwrap();
    f.face.recall(&s, &request).unwrap()
}

fn served(r: &Response) -> (usize, Value) {
    (r.rows.len(), r.blocks["contextful.recall"]["suppressed"]["MemoryEvidenceUnresolved"].clone())
}

fn reader(f: &Fixture) -> AdmittedAuthority {
    f.authority("agent://research-loop", &[Action::Read], &["research/*", "memory/*"])
}

/// The write landing a claim stamps each citation into a keyed table with the cited row's key columns; recall resolves a stamped citation through the key's live version, so a later version or a fold keeps the claim served.
// spec: read.recall.evidence-key@d8871a33
#[test]
fn a_fold_superseding_the_cited_version_keeps_the_claim() {
    let f = Fixture::new();
    let (writer, node) = (f.writer(), NodeId::parse("memory-a").unwrap());
    version(&f, "run-a1", "Dana");
    write_claim(&f.face, &writer, "memory/facts", cites("run-a1"), &node, at("2030-01-11T00:00:00Z"), &super::synthesize::admit).unwrap();
    let r = reader(&f);
    assert_eq!(served(&recall(&f, &r)), (1, json!(0)));
    version(&f, "run-a2", "Lee");
    assert_eq!(served(&recall(&f, &r)), (1, json!(0)), "a later version of the key leaves the claim served");
    fold(f.face.store(), &f.face.decl(ACCOUNTS), at("2030-01-12T00:00:00Z")).unwrap();
    assert_eq!(served(&recall(&f, &r)), (1, json!(0)), "the fold leaves the claim served");
    // A reader who cannot read the key's table still sees the claim withheld.
    let outsider = f.authority("agent://outsider", &[Action::Read], &["memory/*"]);
    assert_eq!(served(&recall(&f, &outsider)), (0, json!(1)));
}

/// A synthesized claim citing a keyed row survives the fold the same way.
#[test]
fn a_synthesized_claim_survives_the_fold_of_its_cited_version() {
    let f = Fixture::new();
    let (writer, node) = (f.writer(), NodeId::parse("memory-a").unwrap());
    version(&f, "run-a1", "Dana");
    let answer = json!({ "claims": [{ "subject": "acme", "predicate": "cfo", "object": "Dana", "confidence": 0.9,
        "evidence": [{ "table": ACCOUNTS, "run": "run-a1", "seq": 0 }] }] })
    .to_string();
    let inference = Scripted::new(&[answer]);
    let pass = Pass {
        face: &f.face,
        authority: &writer,
        inference: &inference,
        source: ACCOUNTS,
        into: "memory/facts",
        state: &f.state,
        node: &node,
        now: at("2030-01-11T00:00:00Z"),
        boundary: &super::synthesize::admit,
    };
    assert_eq!(pass.run().unwrap().landed, 1);
    version(&f, "run-a2", "Lee");
    fold(f.face.store(), &f.face.decl(ACCOUNTS), at("2030-01-12T00:00:00Z")).unwrap();
    assert_eq!(served(&recall(&f, &reader(&f))), (1, json!(0)));
}

/// A direct write citing a keyed table's row that does not read as its key's live version through the writer's session raises `MemoryCitationNotLive`, and nothing lands.
// spec: read.revise.citation-live@53d6ee05
#[test]
fn a_write_citing_a_superseded_version_refuses() {
    let f = Fixture::new();
    let (writer, node) = (f.writer(), NodeId::parse("memory-a").unwrap());
    version(&f, "run-a1", "Dana");
    version(&f, "run-a2", "Lee");
    match write_claim(&f.face, &writer, "memory/facts", cites("run-a1"), &node, at("2030-01-11T00:00:00Z"), &super::synthesize::admit) {
        Err(MemoryFault::Memory(MemoryError::CitationNotLive(why))) => assert!(why.contains(ACCOUNTS), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(served(&recall(&f, &reader(&f))), (0, json!(0)));
    write_claim(&f.face, &writer, "memory/facts", cites("run-a2"), &node, at("2030-01-11T00:00:00Z"), &super::synthesize::admit).unwrap();
    assert_eq!(served(&recall(&f, &reader(&f))), (1, json!(0)));
}

/// A key the model supplies on a citation is replaced by the one the cited row carries.
#[test]
fn a_forged_evidence_key_does_not_resolve() {
    let f = Fixture::new();
    let (writer, node) = (f.writer(), NodeId::parse("memory-a").unwrap());
    version(&f, "run-a1", "Dana");
    land_rows(&f.face, ACCOUNTS, "run-b1", json!([{ "account_id": "globex", "cfo": "Kim" }]));
    let mut forged = cites("run-a1");
    forged.evidence[0].key = Some([("account_id".to_string(), "globex".to_string())].into_iter().collect());
    let written = write_claim(&f.face, &writer, "memory/facts", forged, &node, at("2030-01-11T00:00:00Z"), &super::synthesize::admit).unwrap();
    let key = written.claim.unwrap().evidence[0].key.clone().unwrap();
    assert_eq!(key.get("account_id").map(String::as_str), Some("acme"));
}
