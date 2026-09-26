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
use contextful_core::AuthorityError;
use contextful_policy::verify::AdmittedAuthority;
use serde_json::{json, Value};

/// An effect boundary that admits the carried authority.
pub fn admit() -> Result<(), AuthorityError> {
    Ok(())
}

fn pass<'a>(f: &'a Fixture, writer: &'a AdmittedAuthority, inference: &'a Scripted, node: &'a NodeId, now: &str) -> Pass<'a> {
    Pass {
        face: &f.face,
        authority: writer,
        inference,
        source: "research/notes",
        into: "memory/facts",
        state: &f.state,
        node,
        now: at(now),
        boundary: &admit,
    }
}

fn facts(f: &Fixture, who: &AdmittedAuthority, sql: &str) -> (Vec<String>, Vec<Vec<Value>>) {
    let s = f.face.session(who, &Request::default(), Bounds::default()).unwrap();
    let r = f.face.query(&s, sql, ReadOptions::default()).unwrap();
    (r.columns, r.rows)
}

fn prompt(inference: &Scripted, call: usize) -> String {
    inference.sent.lock().unwrap()[call].iter().map(|m| m.content.clone()).collect::<Vec<_>>().join("\n")
}

/// A pass reads the source's committed rows its cursor has not recorded, through the writing credential's own session, in batches under a prompt bound, recording each batch once its claims commit.
// spec: read.synthesize.pass-cursor@081ca4c3
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

fn many_notes(f: &Fixture, run: &str, n: usize, text: impl Fn(usize) -> String) {
    let rows: Vec<Value> = (0..n).map(|i| json!({ "note_id": format!("n{i}"), "text": text(i) })).collect();
    land_rows(&f.face, "research/notes", run, Value::Array(rows));
}

/// Regression: retired and expired claims newer than a live claim never push it out of
/// the ranked window; a page of claims whose evidence is suppressed reads on.
#[test]
fn dead_claims_never_starve_a_live_one() {
    use contextful_memory::claims::{Landing, Writer};
    use contextful_core::memory::revise::{Claim, Tier};
    use contextful_core::memory::synthesize::EvidenceRef;
    let f = Fixture::new();
    let writer = f.writer();
    land_rows(&f.face, "research/notes", "run-0001", json!([{ "note_id": "n1", "text": "Dana is Acme's CFO." }]));
    let node = NodeId::parse("memory-a").unwrap();
    let w = Writer::of(&writer);
    let claim = |id: &str, object: &str, retired: bool, expired: bool, table: &str| Claim {
        claim_id: id.into(),
        subject: "acme".into(),
        predicate: "cfo".into(),
        object: object.into(),
        scope: None,
        tier: Tier::Derived,
        confidence: 0.5,
        valid_from: at("2030-01-01T00:00:00Z"),
        valid_to: expired.then(|| at("2030-01-02T00:00:00Z")),
        evidence: vec![EvidenceRef { table: table.into(), run: "run-0001".into(), seq: 0 }],
        superseded_by: retired.then(|| "c-other".to_string()),
        grant_id: w.grant_id.clone(),
        agent: w.agent.clone(),
    };
    let land = |run: &str, at_: &str, claims: Vec<Claim>| {
        Landing { node: &node, at: at(at_), writer: &w, run_id: run.into(), boundary: &admit }.commit(&f.face, "memory/facts", &claims, &[]).unwrap();
    };
    land("memory-live", "2030-01-10T00:00:00Z", vec![claim("c-live", "Dana", false, false, "research/notes")]);
    let dead: Vec<Claim> = (0..250).map(|i| claim(&format!("c-r{i}"), "Old CFO", i % 2 == 0, i % 2 == 1, "research/notes")).collect();
    land("memory-dead", "2030-01-11T00:00:00Z", dead);
    let hidden: Vec<Claim> = (0..250).map(|i| claim(&format!("c-h{i}"), "Hidden CFO", false, false, "hr/secret")).collect();
    land("memory-hidden", "2030-01-12T00:00:00Z", hidden);
    let reader = f.authority("agent://research-loop", &[Action::Read], &["research/*", "memory/*"]);
    let recalled = recall(&f, &reader);
    let objects: Vec<Value> = recalled.rows.iter().map(|r| r[1]["object"].clone()).collect();
    assert_eq!(objects, [json!("Dana")], "{:?}", recalled.blocks);
    assert_eq!(recalled.blocks["contextful.recall"]["suppressed"]["MemoryEvidenceUnresolved"], json!(250));
}

/// Regression: every landing re-reads the writer's authority first, so a credential
/// revoked or expired mid-pass lands nothing and advances no cursor.
#[test]
fn a_landing_rereads_the_writers_authority() {
    let f = Fixture::new();
    let (writer, node) = (f.writer(), NodeId::parse("memory-a").unwrap());
    land_rows(&f.face, "research/notes", "run-0001", json!([{ "note_id": "n1", "text": "Dana is Acme's CFO." }]));
    let inference = Scripted::new(&[claims("acme", "cfo", "Dana", "run-0001")]);
    let revoked = || Err(AuthorityError::AuthorityRevoked("denylisted after admission".into()));
    let blocked = Pass { boundary: &revoked, ..pass(&f, &writer, &inference, &node, "2030-01-11T00:00:00Z") };
    assert!(matches!(blocked.run(), Err(MemoryFault::Authority(AuthorityError::AuthorityRevoked(_)))));
    assert!(facts(&f, &writer, r#"SELECT * FROM "memory/facts""#).1.is_empty());
    let write = contextful_memory::write::write_claim(
        &f.face,
        &writer,
        "memory/facts",
        serde_json::from_value(json!({ "subject": "acme", "predicate": "cfo", "object": "Dana", "confidence": 1.0, "evidence": [] })).unwrap(),
        &node,
        at("2030-01-11T00:00:00Z"),
        &revoked,
    );
    assert!(matches!(write, Err(MemoryFault::Authority(_))));
    assert!(facts(&f, &writer, r#"SELECT * FROM "memory/facts""#).1.is_empty());
}

/// Regression: a backlog past the prompt bound goes out in batches, each committing and
/// advancing the cursor on its own; a failed batch keeps the batches before it.
#[test]
fn a_backlog_goes_out_in_bounded_batches() {
    use contextful_memory::synthesize::PROMPT_BYTES;
    let f = Fixture::new();
    let (writer, node) = (f.writer(), NodeId::parse("memory-a").unwrap());
    let long = |i: usize| format!("Note {i}: {}", "Acme's quarterly figures. ".repeat(500));
    many_notes(&f, "run-0001", 12, long);
    let answers: Vec<String> = (0..3).map(|i| claims("acme", "fact", &format!("f{i}"), "run-0001")).chain(["bad".into(), "bad".into(), "bad".into()]).collect();
    let inference = Scripted::new(&answers);
    let failed = pass(&f, &writer, &inference, &node, "2030-01-11T00:00:00Z").run();
    assert!(matches!(failed, Err(MemoryFault::Memory(MemoryError::ExtractExhausted(_)))), "{failed:?}");
    let sent = inference.sent.lock().unwrap().clone();
    assert!(sent.iter().all(|m| m[1].content.len() < PROMPT_BYTES + 1024), "each prompt stays under the bound");
    let (_, landed) = facts(&f, &writer, r#"SELECT object FROM "memory/facts" ORDER BY object"#);
    assert_eq!(landed, [vec![json!("f0")], vec![json!("f1")], vec![json!("f2")]], "three batches committed before the fourth failed");
    // The next pass starts at the failed batch, not at the first row.
    let resumed = Scripted::new(&(0..6).map(|i| claims("acme", "fact", &format!("g{i}"), "run-0001")).collect::<Vec<_>>());
    let report = pass(&f, &writer, &resumed, &node, "2030-01-12T00:00:00Z").run().unwrap();
    assert!(report.batches >= 1 && resumed.calls() == report.batches);
    assert!(!prompt(&resumed, 0).contains("Note 0:"), "committed rows are not read again");
    let again = Scripted::new(&[]);
    assert!(pass(&f, &writer, &again, &node, "2030-01-13T00:00:00Z").run().unwrap().runs.is_empty());
}

/// Regression: a batch whose claims committed and whose cursor did not lands nothing
/// again when retried, whatever the model answers the second time.
#[test]
fn a_retried_batch_commits_once() {
    let f = Fixture::new();
    let (writer, node) = (f.writer(), NodeId::parse("memory-a").unwrap());
    land_rows(&f.face, "research/notes", "run-0001", json!([{ "note_id": "n1", "text": "Dana is Acme's CFO." }]));
    let first = Scripted::new(&[claims("acme", "cfo", "Dana", "run-0001")]);
    pass(&f, &writer, &first, &node, "2030-01-11T00:00:00Z").run().unwrap();
    // The cursor is lost after the commit, as a crash between the two would leave it.
    std::fs::remove_dir_all(&f.state).unwrap();
    let second = Scripted::new(&[claims("acme", "cfo", "Lee", "run-0001")]);
    pass(&f, &writer, &second, &node, "2030-01-12T00:00:00Z").run().unwrap();
    let (_, objects) = facts(&f, &writer, r#"SELECT object FROM "memory/facts""#);
    assert_eq!(objects, [vec![json!("Dana")]]);
}

/// Regression: the resolve stage and the extract stage record one template hash.
#[test]
fn every_dead_letter_carries_one_template_hash() {
    use contextful_core::memory::synthesize::template_hash;
    use contextful_memory::synthesize::system_prompt;
    let f = Fixture::new();
    let (writer, node) = (f.writer(), NodeId::parse("memory-a").unwrap());
    land_rows(
        &f.face,
        "memory/entities",
        "run-0001",
        json!([{ "entity_id": "e-1", "kind": "org", "name": "Initech", "aliases": "[\"it\"]" }, { "entity_id": "e-2", "kind": "org", "name": "Initrode", "aliases": "[\"IT\"]" }]),
    );
    land_rows(&f.face, "research/notes", "run-0001", json!([{ "note_id": "n1", "text": "IT hired a CFO." }]));
    let inference = Scripted::new(&[claims("it", "hired", "a CFO", "run-0001")]);
    pass(&f, &writer, &inference, &node, "2030-01-11T00:00:00Z").run().unwrap();
    land_rows(&f.face, "research/notes", "run-0002", json!([{ "note_id": "n2", "text": "More." }]));
    let prose = Scripted::new(&["a".into(), "b".into(), "c".into()]);
    assert!(pass(&f, &writer, &prose, &node, "2030-01-12T00:00:00Z").run().is_err());
    let (_, hashes) = facts(&f, &writer, r#"SELECT DISTINCT template_hash FROM "memory/facts_dead_letter""#);
    assert_eq!(hashes, [vec![json!(template_hash(&system_prompt()))]]);
}

/// Regression: no two source and target pairs share a cursor file.
#[test]
fn cursor_files_never_collide() {
    use contextful_memory::synthesize::cursor_path;
    let state = std::path::Path::new("state");
    assert_ne!(cursor_path(state, "a--b", "c"), cursor_path(state, "a", "b--c"));
    assert_ne!(cursor_path(state, "a/b", "c"), cursor_path(state, "a%2Fb", "c"));
    assert_eq!(cursor_path(state, "a/b", "c"), cursor_path(state, "a/b", "c"));
}
