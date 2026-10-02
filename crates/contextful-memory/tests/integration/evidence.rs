//! Evidence into a keyed table: a claim cites a row version, and a later version of the
//! same key, folded or not, keeps the claim served.

use super::support::*;
use contextful_context::fold::fold;
use contextful_context::read::{ReadOptions, RecallRequest};
use contextful_core::grant::Action;
use contextful_core::memory::synthesize::{CandidateClaim, EvidenceRef};
use contextful_core::memory::MemoryError;
use contextful_core::read::respond::Response;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::lay_out::NodeId;
use contextful_memory::synthesize::Pass;
use contextful_memory::write::write_claim;
use contextful_memory::MemoryFault;
use contextful_policy::enforce::session::Request;
use contextful_policy::verify::AdmittedAuthority;
use serde_json::{json, Value};

const ACCOUNTS: &str = "research/accounts";
const PATIENTS: &str = "research/patients";

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
    recall_of(f, who, "acme")
}

fn recall_of(f: &Fixture, who: &AdmittedAuthority, subject: &str) -> Response {
    let request = RecallRequest::new("memory/facts", subject, at("2030-06-01T00:00:00Z"));
    let s = f.face.session(who, &Request::default(), request.bounds()).unwrap();
    f.face.recall(&s, &request).unwrap()
}

fn served(r: &Response) -> (usize, Value) {
    (r.rows.len(), r.blocks["contextful.recall"]["suppressed"]["MemoryEvidenceUnresolved"].clone())
}

fn stale(r: &Response) -> Value {
    r.blocks["contextful.recall"]["stale"].clone()
}

fn reader(f: &Fixture) -> AdmittedAuthority {
    f.authority("agent://research-loop", &[Action::Read], &["research/*", "memory/*"])
}

fn write(f: &Fixture, who: &AdmittedAuthority, claim: CandidateClaim) -> Result<contextful_memory::write::Written, MemoryFault> {
    write_claim(&f.face, who, "memory/facts", claim, &NodeId::parse("memory-a").unwrap(), at("2030-01-11T00:00:00Z"), &super::synthesize::admit)
}

/// The write landing a claim stamps each citation into a keyed table with a pepper-keyed digest of the cited row's key; recall resolves a citation whose row no longer reads through the digest's live version.
// spec: read.recall.evidence-key@020c59aa
#[test]
fn a_fold_superseding_the_cited_version_keeps_the_claim() {
    let f = Fixture::new();
    version(&f, "run-a1", "Dana");
    write(&f, &f.writer(), cites("run-a1")).unwrap();
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

/// A direct write citing a keyed table's row that reads through the writer's session but is not its key's live version raises `MemoryCitationNotLive`, and nothing lands.
// spec: read.revise.citation-live@c5f90564
#[test]
fn a_write_citing_a_superseded_version_refuses() {
    let f = Fixture::new();
    let writer = f.writer();
    version(&f, "run-a1", "Dana");
    version(&f, "run-a2", "Lee");
    match write(&f, &writer, cites("run-a1")) {
        Err(MemoryFault::Memory(MemoryError::CitationNotLive(why))) => assert!(why.contains(ACCOUNTS), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(served(&recall(&f, &reader(&f))), (0, json!(0)));
    write(&f, &writer, cites("run-a2")).unwrap();
    assert_eq!(served(&recall(&f, &reader(&f))), (1, json!(0)));
}

/// A direct write citing a keyed table's run and sequence no landed row carries lands unstamped, and recall withholds the claim as unresolved.
#[test]
fn a_write_citing_an_absent_keyed_row_lands_unresolved() {
    let f = Fixture::new();
    version(&f, "run-a1", "Dana");
    let written = write(&f, &f.writer(), cites("run-zzz")).unwrap();
    assert_eq!(written.claim.unwrap().evidence[0].key_digest, None);
    assert_eq!(served(&recall(&f, &reader(&f))), (0, json!(1)));
}

/// A digest the payload supplies on a citation is replaced by the cited row's own.
#[test]
fn a_forged_evidence_digest_is_replaced() {
    let f = Fixture::new();
    let writer = f.writer();
    version(&f, "run-a1", "Dana");
    land_rows(&f.face, ACCOUNTS, "run-b1", json!([{ "account_id": "globex", "cfo": "Kim" }]));
    let mut globex = cites("run-b1");
    globex.subject = "globex".into();
    let globex = write(&f, &writer, globex).unwrap().claim.unwrap().evidence[0].key_digest.clone().unwrap();
    let mut forged = cites("run-a1");
    forged.evidence[0].key_digest = Some(globex.clone());
    let acme = write(&f, &writer, forged).unwrap().claim.unwrap().evidence[0].key_digest.clone().unwrap();
    assert_ne!(acme, globex);
    assert_eq!(acme.len(), 64, "{acme}");
}

/// A citation stored in a memory row carries the cited table, run, sequence and key digest, and no cell value of the cited row.
// spec: read.recall.evidence-no-value@2ac3fb45
#[test]
fn a_memory_reader_reads_no_source_key_value() {
    let f = Fixture::new();
    land_rows(&f.face, ACCOUNTS, "run-a1", json!([{ "account_id": "acct-secret-123", "cfo": "Dana" }]));
    let mut claim = cites("run-a1");
    claim.subject = "acct-secret-123".into();
    write(&f, &f.writer(), claim).unwrap();
    let outsider = f.authority("agent://outsider", &[Action::Read], &["memory/*"]);
    let s = f.face.session(&outsider, &Request::default(), Bounds::default()).unwrap();
    let r = f.face.query(&s, r#"SELECT evidence FROM "memory/facts""#, ReadOptions::default()).unwrap();
    assert_eq!(r.rows.len(), 1);
    let text = serde_json::to_string(&r.rows).unwrap();
    assert!(!text.contains("acct-secret") && !text.contains("account_id"), "{text}");
    land_rows(&f.face, ACCOUNTS, "run-a2", json!([{ "account_id": "acct-secret-123", "cfo": "Lee" }]));
    fold(f.face.store(), &f.face.decl(ACCOUNTS), at("2030-01-12T00:00:00Z")).unwrap();
    assert_eq!(served(&recall_of(&f, &reader(&f), "acct-secret-123")), (1, json!(0)), "the digest resolves the citation");
}

/// A citation whose key column reads masked or nulled by zone in the writer's session carries no digest; a masked or nulled column outside the key leaves the digest stamped.
// spec: read.recall.evidence-key-masked@04c7f215
#[test]
fn a_writer_zone_nulling_a_non_key_column_still_stamps_the_key() {
    let f = Fixture::new();
    land_rows(&f.face, PATIENTS, "run-p1", json!([{ "patient_id": "p-1", "case_notes": "stable" }]));
    let cloud = f.authority_in("agent://synthesizer", &[Action::Read, Action::Write], &["research/*", "memory/*"], "public-cloud:eu");
    let claim = CandidateClaim {
        subject: "p-1".into(),
        predicate: "status".into(),
        object: "stable".into(),
        scope: None,
        confidence: 1.0,
        evidence: vec![EvidenceRef::row(PATIENTS, "run-p1", 0)],
    };
    let written = write(&f, &cloud, claim).unwrap();
    assert!(written.claim.unwrap().evidence[0].key_digest.is_some(), "the key column reads, so the citation carries its digest");
    land_rows(&f.face, PATIENTS, "run-p2", json!([{ "patient_id": "p-1", "case_notes": "discharged" }]));
    fold(f.face.store(), &f.face.decl(PATIENTS), at("2030-01-12T00:00:00Z")).unwrap();
    assert_eq!(served(&recall_of(&f, &reader(&f), "p-1")), (1, json!(0)));
}

/// A claim served through its key digest while its cited run and sequence no longer read counts under `stale` in the `contextful.recall` block, which names no claim.
// spec: read.recall.evidence-stale@96c0b504
#[test]
fn a_claim_served_past_its_cited_version_counts_stale() {
    let f = Fixture::new();
    version(&f, "run-a1", "Dana");
    write(&f, &f.writer(), cites("run-a1")).unwrap();
    let r = reader(&f);
    assert_eq!(stale(&recall(&f, &r)), json!(0));
    version(&f, "run-a2", "Lee");
    let after = recall(&f, &r);
    assert_eq!(served(&after), (1, json!(0)));
    assert_eq!(stale(&after), json!(1), "the claim cites a version the key no longer serves");
}
