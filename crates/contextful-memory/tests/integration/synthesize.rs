//! A synthesis pass over a scratch store, and recall of what it landed.

use super::support::*;
use contextful_context::read::{ReadOptions, RetrieveRequest};
use contextful_core::grant::Action;
use contextful_core::memory::MemoryError;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::lay_out::NodeId;
use contextful_memory::synthesize::Pass;
use contextful_memory::MemoryFault;
use contextful_policy::enforce::session::Request;
use contextful_policy::verify::AdmittedAuthority;
use serde_json::{json, Value};

fn pass<'a>(f: &'a Fixture, writer: &'a AdmittedAuthority, inference: &'a Scripted, node: &'a NodeId, now: &str) -> Pass<'a> {
    Pass { face: &f.face, authority: writer, inference, source: "research/notes", into: "memory/facts", state: &f.state, node, now: at(now) }
}

fn facts(f: &Fixture, who: &AdmittedAuthority, sql: &str) -> (Vec<String>, Vec<Vec<Value>>) {
    let s = f.face.session(who, &Request::default(), Bounds::default()).unwrap();
    let r = f.face.query(&s, sql, ReadOptions::default()).unwrap();
    (r.columns, r.rows)
}

fn prompt(inference: &Scripted, call: usize) -> String {
    inference.sent.lock().unwrap()[call].iter().map(|m| m.content.clone()).collect::<Vec<_>>().join("\n")
}

/// A pass reads the source's committed runs its cursor has not recorded, through the writing credential's own session, and records them once their claims commit.
// spec: read.synthesize.pass-cursor@aa2754e7
#[test]
fn a_pass_reads_the_runs_past_its_cursor() {
    let f = Fixture::new();
    let (writer, node) = (f.writer(), NodeId::parse("memory-a").unwrap());
    land_rows(&f.face, "research/notes", "run-0001", json!([{ "note_id": "n1", "text": "Dana is Acme's CFO." }]));
    let inference = Scripted::new(&[claims("acme", "cfo", "Dana", "run-0001"), claims("acme", "cfo", "Lee", "run-0002")]);
    let first = pass(&f, &writer, &inference, &node, "2030-01-11T00:00:00Z").run().unwrap();
    assert_eq!((first.runs, first.landed), (vec!["run-0001".to_string()], 1));
    assert!(prompt(&inference, 0).contains("Dana is Acme's CFO."));
    assert!(prompt(&inference, 0).contains("research/notes#run-0001:0"));

    let idle = pass(&f, &writer, &inference, &node, "2030-01-12T00:00:00Z").run().unwrap();
    assert!(idle.runs.is_empty());
    assert_eq!(inference.calls(), 1, "no new run, no model call");

    land_rows(&f.face, "research/notes", "run-0002", json!([{ "note_id": "n2", "text": "Acme appointed Lee as CFO." }]));
    let second = pass(&f, &writer, &inference, &node, "2030-01-13T00:00:00Z").run().unwrap();
    assert_eq!(second.runs, ["run-0002"]);
    assert!(prompt(&inference, 1).contains("appointed Lee") && !prompt(&inference, 1).contains("Dana is Acme's CFO."));

    // A credential that cannot read the source reads nothing and lands nothing.
    let blind = f.authority("agent://synthesizer", &[Action::Read, Action::Write], &["memory/*"]);
    assert!(matches!(pass(&f, &blind, &inference, &node, "2030-01-14T00:00:00Z").run(), Err(MemoryFault::Denied(_))));
}

/// Every landed claim carries `grant_id`, the chain-final revocation identifier of the credential that wrote it, `agent`, that credential's agent member, and `_authored_by`, its on-behalf-of principal.
// spec: read.synthesize.attribution@634bca93
#[test]
fn a_landed_claim_names_the_grant_that_wrote_it() {
    let f = Fixture::new();
    let (writer, node) = (f.writer(), NodeId::parse("memory-a").unwrap());
    land_rows(&f.face, "research/notes", "run-0001", json!([{ "note_id": "n1", "text": "Dana is Acme's CFO." }]));
    let inference = Scripted::new(&[claims("acme", "cfo", "Dana", "run-0001")]);
    pass(&f, &writer, &inference, &node, "2030-01-11T00:00:00Z").run().unwrap();
    let (_, rows) = facts(&f, &writer, r#"SELECT grant_id, agent, _authored_by, tier FROM "memory/facts""#);
    assert_eq!(
        rows,
        [vec![json!(writer.revocation_ids().last().unwrap()), json!("agent://synthesizer"), json!("user://dana@acme.example"), json!("derived")]]
    );
}

/// A batch exhausting {{read.synthesize.extract-attempts}} writes the response, template hash and drop reason to the dead-letter table, raises `MemoryExtractExhausted`, and leaves the cursor unadvanced.
// spec: read.synthesize.dead-letter@96cedf8d
#[test]
fn an_exhausted_batch_dead_letters_and_holds_the_cursor() {
    let f = Fixture::new();
    let (writer, node) = (f.writer(), NodeId::parse("memory-a").unwrap());
    land_rows(&f.face, "research/notes", "run-0001", json!([{ "note_id": "n1", "text": "Dana is Acme's CFO." }]));
    let prose = ["Dana".to_string(), "Dana, still".to_string(), "Dana, finally".to_string(), claims("acme", "cfo", "Dana", "run-0001")];
    let inference = Scripted::new(&prose);
    match pass(&f, &writer, &inference, &node, "2030-01-11T00:00:00Z").run() {
        Err(MemoryFault::Memory(MemoryError::ExtractExhausted(why))) => assert!(why.contains("3 attempts"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(inference.calls(), 3);
    let (_, letters) = facts(&f, &writer, r#"SELECT reason, response, template_hash FROM "memory/facts_dead_letter""#);
    assert_eq!(letters.len(), 1);
    assert_eq!((letters[0][0].clone(), letters[0][1].clone()), (json!("MemoryExtractExhausted"), json!("Dana, finally")));
    assert!(letters[0][2].as_str().unwrap().starts_with("sha256:"));
    assert!(facts(&f, &writer, r#"SELECT * FROM "memory/facts""#).1.is_empty());
    // The cursor held: the next pass reads the same run again.
    let retried = pass(&f, &writer, &inference, &node, "2030-01-12T00:00:00Z").run().unwrap();
    assert_eq!((retried.runs, retried.landed), (vec!["run-0001".to_string()], 1));
}

fn superseded(f: &Fixture) -> AdmittedAuthority {
    let (writer, node) = (f.writer(), NodeId::parse("memory-a").unwrap());
    land_rows(&f.face, "research/notes", "run-0001", json!([{ "note_id": "n1", "text": "Dana is Acme's CFO." }]));
    let inference = Scripted::new(&[claims("acme", "cfo", "Dana", "run-0001"), claims("acme", "cfo", "Lee", "run-0002")]);
    pass(&f, &writer, &inference, &node, "2030-01-11T00:00:00Z").run().unwrap();
    land_rows(&f.face, "research/notes", "run-0002", json!([{ "note_id": "n2", "text": "Acme appointed Lee as CFO." }]));
    let second = pass(&f, &writer, &inference, &node, "2030-01-13T00:00:00Z").run().unwrap();
    assert_eq!((second.landed, second.retired), (1, 1));
    writer
}

fn recall(f: &Fixture, who: &AdmittedAuthority) -> contextful_core::read::respond::Response {
    let s = f.face.session(who, &Request::default(), Bounds::default()).unwrap();
    let request = RetrieveRequest::new("memory/", "acme cfo", at("2030-02-01T00:00:00Z"));
    f.face.retrieve(&s, &request, Bounds::default()).unwrap()
}

/// A `corpus.retrieve` arm over a `memory_facts` table serves only live claims — no `superseded_by`, and a `valid_to` null or past the read's anchor — whose evidence passes the gate.
// spec: read.recall.ranked-arm@2e91118b
#[test]
fn a_ranked_arm_over_claims_serves_live_claims_alone() {
    let f = Fixture::new();
    superseded(&f);
    let reader = f.authority("agent://research-loop", &[Action::Read], &["research/*", "memory/*"]);
    let recalled = recall(&f, &reader);
    let objects: Vec<Value> = recalled.rows.iter().map(|r| r[1]["object"].clone()).collect();
    assert_eq!(objects, [json!("Lee")]);
    // The table still holds the retired prior; a plain statement shows it.
    let (_, all) = facts(&f, &reader, r#"SELECT object, superseded_by IS NOT NULL AS retired FROM "memory/facts" ORDER BY object"#);
    assert_eq!(all, [vec![json!("Dana"), json!(true)], vec![json!("Lee"), json!(false)]]);
    // Asked before the successor's validity began, the prior was still live — and is not.
    let before = f.face.retrieve(
        &f.face.session(&reader, &Request::default(), Bounds::default()).unwrap(),
        &RetrieveRequest::new("memory/", "acme cfo", at("2030-01-12T00:00:00Z")),
        Bounds::default(),
    )
    .unwrap();
    assert!(before.rows.iter().all(|r| r[1]["object"] != json!("Dana")), "a superseded claim is not live at any anchor");
}

/// A suppressed claim is absent from the rows; the `contextful.recall` block counts suppressions per error identifier and names no claim.
// spec: read.recall.suppression-count@d292fd85
#[test]
fn a_suppressed_claim_is_counted_and_never_named() {
    let f = Fixture::new();
    superseded(&f);
    let outsider = f.authority("agent://outsider", &[Action::Read], &["memory/*"]);
    let withheld = recall(&f, &outsider);
    assert!(withheld.rows.is_empty());
    let block = &withheld.blocks["contextful.recall"];
    assert_eq!(block, &json!({ "suppressed": { "MemoryEvidenceUnresolved": 1, "MemoryEvidenceOverflow": 0 } }));
    assert!(!serde_json::to_string(&withheld.to_json()).unwrap().contains("Lee"));
    let reader = f.authority("agent://research-loop", &[Action::Read], &["research/*", "memory/*"]);
    assert_eq!(recall(&f, &reader).blocks["contextful.recall"]["suppressed"]["MemoryEvidenceUnresolved"], json!(0));
}

#[test]
fn an_ambiguous_subject_dead_letters_its_claim() {
    let f = Fixture::new();
    let (writer, node) = (f.writer(), NodeId::parse("memory-a").unwrap());
    land_rows(
        &f.face,
        "memory/entities",
        "run-0001",
        json!([
            { "entity_id": "e-1", "kind": "org", "name": "Initech", "aliases": "[\"it\"]" },
            { "entity_id": "e-2", "kind": "org", "name": "Initrode", "aliases": "[\"IT\"]" },
        ]),
    );
    land_rows(&f.face, "research/notes", "run-0001", json!([{ "note_id": "n1", "text": "IT hired a CFO." }]));
    let inference = Scripted::new(&[claims("it", "hired", "a CFO", "run-0001")]);
    let report = pass(&f, &writer, &inference, &node, "2030-01-11T00:00:00Z").run().unwrap();
    assert_eq!((report.landed, report.dead_lettered), (0, 1));
    let (_, letters) = facts(&f, &writer, r#"SELECT reason FROM "memory/facts_dead_letter""#);
    assert_eq!(letters, [vec![json!("MemoryEntityAmbiguous")]]);
}
