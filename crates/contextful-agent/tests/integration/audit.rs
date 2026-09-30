//! What a read leaves behind on the tool protocol: each answered read tool call appends one
//! audit entry, and its result leaves only after that entry's append group syncs.

use crate::mcp::{ask, bounded, call, current, fixture, Fixture};
use contextful_agent::mcp::Server;
use contextful_core::ports::FixedClock;
use contextful_core::time::Instant;
use contextful_policy::audit::{verify, AuditLog, AuditOptions, Fsync, FsyncPort, NoIssuerKey};
use serde_json::{json, Value};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

/// Segment syncs, counted; every one fails while `fail` is set.
#[derive(Default)]
struct Syncs {
    segment: AtomicUsize,
    fail: AtomicBool,
}

struct Port(Arc<Syncs>);

impl FsyncPort for Port {
    fn sync(&self, what: Fsync, file: &File) -> std::io::Result<()> {
        if let Fsync::Segment(_) = what {
            if self.0.fail.load(Ordering::SeqCst) {
                return Err(std::io::Error::other("the storage port refuses the sync"));
            }
            self.0.segment.fetch_add(1, Ordering::SeqCst);
        }
        file.sync_all()
    }
}

/// A chain under the fixture's directory whose segment syncs run through `syncs`.
fn probed(f: &Fixture, syncs: &Arc<Syncs>) -> (AuditLog<NoIssuerKey>, PathBuf) {
    let dir = f.dir.path().join("probed");
    let options = AuditOptions { fsync: Arc::new(Port(syncs.clone())), ..AuditOptions::default() };
    (AuditLog::unanchored_with(&dir, options).unwrap(), dir)
}

/// Every entry the chain holds, in `seq` order.
fn entries(chain: &Path) -> Vec<Value> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(chain.join("segments"))
        .map(|d| d.map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|x| x == "jsonl")).collect())
        .unwrap_or_default();
    files.sort();
    files.iter().flat_map(|f| std::fs::read_to_string(f).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect::<Vec<Value>>()).collect()
}

fn clock() -> FixedClock {
    FixedClock(Instant::parse("2030-01-01T04:00:00Z").unwrap())
}

/// One call per read tool, each answered with a result, and the rows each returns.
fn reads(server: &Server<'_>) -> Vec<(&'static str, Value)> {
    let listed = call(server, "context.files", json!({}));
    let part = listed["result"]["structuredContent"]["rows"].as_array().unwrap().iter().find(|r| r[0] == json!("research/rates")).unwrap()[1].clone();
    vec![
        ("context.describe", json!({})),
        ("context.query", json!({ "sql": r#"SELECT ccy FROM "research/rates" ORDER BY ccy"# })),
        ("context.execute_query", json!({ "id": "filing", "arguments": { "doc": "a" } })),
        ("rates", json!({ "skip": "usd" })),
        ("context.files", json!({})),
        ("context.file", json!({ "path": part })),
        ("corpus.retrieve", json!({ "prefix": "research/rates", "query": "eur gbp" })),
    ]
}

/// Each read tool call the face answers with a result appends one entry before the result leaves; `tools/list` appends none.
// spec: disclosure.record.read-entry@49ba52b7
#[test]
fn each_answered_read_tool_call_appends_one_entry_synced_before_its_result() {
    let f = bounded();
    let syncs = Arc::new(Syncs::default());
    let (audit, chain) = probed(&f, &syncs);
    let clock = clock();
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &audit).unwrap();

    ask(&server, 1, "initialize", json!({}));
    ask(&server, 2, "tools/list", json!({}));
    ask(&server, 3, "ping", json!({}));
    assert_eq!((audit.tip().seq, syncs.segment.load(Ordering::SeqCst)), (0, 0), "no read tool ran");

    let calls = reads(&server);
    let base = audit.tip().seq;
    for (i, (tool, arguments)) in calls.iter().enumerate() {
        let answer = call(&server, tool, arguments.clone());
        assert!(answer["result"].get("isError").is_none(), "{tool}: {answer}");
        // By the time the answer exists, its entry is synced.
        let n = base + i as u64 + 1;
        assert_eq!((audit.tip().seq, syncs.segment.load(Ordering::SeqCst) as u64), (n, n), "{tool}");
        let rows = answer["result"]["structuredContent"]["rows"].as_array().map_or(0, Vec::len);
        let entry = &entries(&chain)[n as usize - 1]["attributes"];
        assert_eq!((entry["contextful.tool"].as_str(), entry["contextful.result.rows"].as_u64()), (Some(*tool), Some(rows as u64)), "{tool}");
    }
    assert_eq!(verify(&chain).unwrap().seq, base + calls.len() as u64, "the read path's entries link into a chain that verifies");
}

/// A read's entry carries `contextful.tool`, `contextful.credential`, each present subject member and its attestation, and `contextful.result.rows`.
// spec: disclosure.record.read-attributes@2032a08c
#[test]
fn a_read_entry_names_tool_credential_subject_and_row_count() {
    let f = bounded();
    let clock = clock();
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    call(&server, "context.query", json!({ "sql": r#"SELECT ccy FROM "research/rates" ORDER BY ccy"# }));

    let lines = entries(&f.dir.path().join("audit"));
    let attributes = lines[0]["attributes"].as_object().unwrap();
    assert_eq!(
        attributes.clone().into_iter().collect::<Value>(),
        json!({
            "contextful.tool": "context.query",
            "contextful.credential": f.authority.credential_id(),
            "contextful.subject.on_behalf_of": "user://dana@acme.example",
            "contextful.subject.attestation.on_behalf_of": "verified",
            "contextful.subject.agent": "agent://research-loop",
            "contextful.subject.attestation.agent": "asserted",
            "contextful.subject.zone": "on-prem:hq",
            "contextful.subject.attestation.zone": "asserted",
            "contextful.result.rows": 2,
            "contextful.read.at": "2030-01-01T04:00:00Z",
            "contextful.read.outcome": "served",
            "contextful.tables": ["research/rates"],
        })
    );
    let text = std::fs::read_to_string(f.dir.path().join("audit/segments/000001.jsonl")).unwrap();
    for held in ["SELECT", "eur", "gbp"] {
        assert!(!text.contains(held), "the chain holds `{held}`: {text}");
    }
}

/// A read whose entry fails to reach local durable storage raises `AuditEntryUnpersisted` and returns no rows; the face answers it in-band with code `audit_entry_unpersisted` and HTTP 503.
// spec: disclosure.record.unpersisted-entry@0038925d
// spec: disclosure.record.unpersisted-wire@3b8f8217
#[test]
fn a_read_whose_entry_does_not_sync_releases_no_rows() {
    let f = bounded();
    let syncs = Arc::new(Syncs::default());
    let (audit, chain) = probed(&f, &syncs);
    let clock = clock();
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &audit).unwrap();
    let calls = reads(&server);
    let base = audit.tip().seq;
    let before = entries(&chain).len();
    syncs.fail.store(true, Ordering::SeqCst);

    let mut released = 0;
    for (tool, arguments) in &calls {
        let answer = call(&server, tool, arguments.clone());
        let result = &answer["result"];
        assert_eq!(result["isError"], json!(true), "{tool}: {answer}");
        assert_eq!(
            result["structuredContent"]["error"].as_object().map(|e| (e["code"].clone(), e["http"].clone(), e["identifier"].clone())),
            Some((json!("audit_entry_unpersisted"), json!(503), json!("AuditEntryUnpersisted"))),
            "{tool}: {answer}"
        );
        released += result["structuredContent"]["rows"].as_array().map_or(0, Vec::len);
        let text = answer.to_string();
        assert!(!text.contains("eur") && !text.contains("gbp") && !text.contains("/runs/"), "{tool}: a row value leaves: {text}");
    }
    contextful_eval::record::emit("audit-read-waits-on-entry", released as f64, calls.len() as u64, 0);
    assert_eq!(released, 0, "rows released behind an unsynced entry");
    assert_eq!(audit.tip().seq, base, "the tip stays at the prior entry");
    assert_eq!(entries(&chain).len(), before, "the segment returns to its prior length");

    // The storage recovers, and the next read appends and releases.
    syncs.fail.store(false, Ordering::SeqCst);
    let answer = call(&server, "context.query", json!({ "sql": r#"SELECT ccy FROM "research/rates" ORDER BY ccy"# }));
    assert_eq!(answer["result"]["structuredContent"]["rows"], json!([["eur"], ["gbp"]]), "{answer}");
    assert_eq!(audit.tip().seq, base + 1);
}

/// A read enforcement refuses appends one entry with `outcome` `refused`, naming the relations the guard parsed, and releases no rows.
// spec: disclosure.record.refused-read@49fe051c
#[test]
fn a_refused_read_appends_one_refused_entry_naming_its_relations() {
    let f = fixture();
    let clock = clock();
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let answer = call(&server, "context.query", json!({ "sql": r#"SELECT employee, title FROM "hr/salaries""# }));
    let result = &answer["result"];
    assert_eq!(result["isError"], json!(true), "{answer}");
    assert!(result["structuredContent"].get("rows").is_none(), "{answer}");
    assert!(!answer.to_string().contains("Battery storage engineer"), "{answer}");

    let lines = entries(&f.dir.path().join("audit"));
    assert_eq!(lines.len(), 1, "one entry for the refused read");
    let entry = &lines[0]["attributes"];
    assert_eq!(entry["contextful.read.outcome"], json!("refused"));
    assert_eq!(entry["contextful.tables"], json!(["hr/salaries"]));
    assert_eq!(entry["contextful.result.rows"], json!(0));
    assert_eq!(entry["contextful.read.refusal"], result["structuredContent"]["error"]["identifier"], "{answer}");
    assert_eq!(entry["contextful.subject.on_behalf_of"], json!("user://dana@acme.example"));
}
