//! The tool protocol over the read face: the handshake, the closed tool set, in-band
//! refusals and the effect-boundary re-read.

use contextful_agent::mcp::Server;
use contextful_context::land::{land, Batch, RunContext};
use contextful_context::read::Face;
use contextful_context::Store;
use contextful_core::grant::{Action, Grant, TablePattern};
use contextful_core::identify::Subject;
use contextful_core::issue::{IssuancePolicy, Lifetime, MintContext, MintRequest, NodeRole, SignatureAlgorithm};
use contextful_core::ports::FixedClock;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reserve::Injection;
use contextful_core::time::Instant;
use contextful_core::AuthorityError;
use contextful_policy::enforce::mask::Pepper;
use contextful_policy::issue::{mint, MintClaims, SeedSigner};
use contextful_policy::keyset::{KeySource, StaticPins};
use contextful_policy::revoke::RevocationState;
use contextful_policy::verify::{verify, Admission, AdmittedAuthority};
use serde_json::{json, Value};
use std::collections::HashMap;

const AUD: &str = "contextful://acme-research";
const MANIFEST: &str = "[[pipeline.tables]]\nname = \"research/notes\"\n\n[[pipeline.tables]]\nname = \"hr/salaries\"\n";

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

struct Fixture {
    _dir: tempfile::TempDir,
    face: Face,
    authority: AdmittedAuthority,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), "research").unwrap();
    for (table, rows) in [
        ("research/notes", json!([{ "note_id": "n1", "title": "Solar battery storage" }, { "note_id": "n2", "title": "Hiring plan" }])),
        ("hr/salaries", json!([{ "employee": "e1", "title": "Battery storage engineer" }])),
    ] {
        let rows = rows.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect();
        let ctx = RunContext {
            node: NodeId::parse("ingest-a").unwrap(),
            injection: Injection { run_id: "run-0001".into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None },
            committed_at: at("2030-01-01T00:00:00Z"),
        };
        land(&store, &TableDecl::named(table), &Batch { rows, types: HashMap::new() }, &ctx).unwrap();
    }
    let face = Face::open(store, MANIFEST, Pepper::resolve(|_| None)).unwrap();
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let policy = IssuancePolicy::parse(&format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n")).unwrap();
    let subject = Subject {
        on_behalf_of: Some("user://dana@acme.example".into()),
        agent: Some("agent://research-loop".into()),
        zone: Some("on-prem:hq".into()),
        ..Subject::default()
    };
    let grant = Grant {
        actions: vec![Action::Read],
        tables: vec![TablePattern::parse("research/*").unwrap()],
        tenant: None,
        aggregate: None,
        templates: None,
        max_rows: None,
    };
    let mut req = MintRequest::custody(subject, vec![grant]);
    req.lifetime = Lifetime::Requested(900);
    let clock = FixedClock(at("2030-01-01T00:00:00Z"));
    let plan = policy.check(&req, &MintContext { node: NodeRole::Primary, signer: &signer, clock: &clock }).unwrap();
    let token = mint(&plan, &MintClaims::default(), &signer).unwrap();
    let keys = StaticPins::parse(&signer.public_key_text()).unwrap().keys().unwrap();
    let revocation = RevocationState::default();
    let authority = verify(&token, &keys, &Admission::new(at("2030-01-01T00:05:00Z"), &revocation).expecting(AUD)).unwrap();
    Fixture { _dir: dir, face, authority }
}

/// An effect boundary that admits the carried authority.
fn current(_: &AdmittedAuthority) -> Result<(), AuthorityError> {
    Ok(())
}

fn ask(server: &Server<'_>, id: u64, method: &str, params: Value) -> Value {
    let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
    let answer = server.handle(&line).expect("a request is answered");
    assert_eq!(answer["id"], json!(id));
    answer
}

fn call(server: &Server<'_>, tool: &str, arguments: Value) -> Value {
    ask(server, 7, "tools/call", json!({ "name": tool, "arguments": arguments }))
}

/// The face exposes a closed tool set: `context.describe`, `context.query`, `context.execute_query` for templates, `context.files` and `context.file` over committed data files, and `corpus.retrieve` for ranked reads across a prefix.
// spec: read.register.tool-set@6d72284d
#[test]
fn the_tool_list_is_the_closed_read_set() {
    let f = fixture();
    let server = Server::new(&f.face, f.authority.clone(), &current).unwrap();
    let tools = ask(&server, 1, "tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["context.describe", "context.query", "context.execute_query", "context.files", "context.file", "corpus.retrieve"]);
    let unknown = call(&server, "memory.write", json!({}));
    assert_eq!(unknown["error"]["code"], json!(-32602), "{unknown}");
}

/// On the tool protocol a refusal arrives in-band, as a protocol error object or a result flagged as an error under transport success.
// spec: read.respond.in-band-error@57469a30
#[test]
fn a_refusal_arrives_in_band() {
    let f = fixture();
    let server = Server::new(&f.face, f.authority.clone(), &current).unwrap();
    let refused = call(&server, "context.query", json!({ "sql": "SELECT * FROM \"hr/salaries\"" }));
    assert_eq!(refused["result"]["isError"], json!(true), "{refused}");
    assert_eq!(refused["result"]["structuredContent"]["error"]["identifier"], json!("EnforceUnknownRelation"));
    assert!(refused["result"]["content"][0]["text"].as_str().unwrap().contains("unknown_relation"));
    let answered = call(&server, "context.query", json!({ "sql": "SELECT note_id FROM \"research/notes\" ORDER BY note_id" }));
    assert_eq!(answered["result"]["structuredContent"]["rows"], json!([["n1"], ["n2"]]));
    assert!(answered["result"].get("isError").is_none());
    let malformed = call(&server, "context.query", json!({ "sql": 3 }));
    assert_eq!(malformed["error"]["code"], json!(-32602));
}

#[test]
fn the_handshake_reports_the_build_and_refuses_an_absent_face() {
    let f = fixture();
    let server = Server::new(&f.face, f.authority.clone(), &current).unwrap();
    let init = ask(&server, 1, "initialize", json!({ "protocolVersion": "2025-06-18", "require": ["duckdb"] }));
    assert_eq!(init["result"]["contextful.build"]["backends"], json!(["duckdb", "fts"]));
    let absent = ask(&server, 2, "initialize", json!({ "require": ["hnsw"] }));
    assert!(absent["error"]["message"].as_str().unwrap().starts_with("RequiredFaceAbsent"), "{absent}");
    assert!(server.handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string()).is_none());
}

#[test]
fn every_call_re_reads_the_authority() {
    let f = fixture();
    let boundary = |_: &AdmittedAuthority| Err(AuthorityError::AuthorityExpired("expired at 2030-01-01T00:15:00Z".into()));
    let server = Server::new(&f.face, f.authority.clone(), &boundary).unwrap();
    let stopped = call(&server, "context.query", json!({ "sql": "SELECT note_id FROM \"research/notes\"" }));
    assert_eq!(stopped["result"]["isError"], json!(true));
    assert!(stopped["result"]["content"][0]["text"].as_str().unwrap().contains("AuthorityExpired"));
}
