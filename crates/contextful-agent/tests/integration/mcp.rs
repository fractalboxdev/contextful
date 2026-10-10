//! The tool protocol over the read face: the handshake, the closed tool set, in-band
//! refusals, the effect-boundary re-read and the bound arguments every read tool admits.

use contextful_agent::mcp::Server;
use contextful_context::fold::fold;
use contextful_context::land::{land, Batch, RunContext};
use contextful_context::read::Face;
use contextful_context::Store;
use contextful_core::grant::{Action, Grant, TablePattern};
use contextful_core::identify::Subject;
use contextful_core::issue::{IssuancePolicy, Lifetime, MintContext, MintRequest, NodeRole, SignatureAlgorithm};
use contextful_core::ports::FixedClock;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::reconcile::ColumnType;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reserve::Injection;
use contextful_core::time::Instant;
use contextful_core::AuthorityError;
use contextful_policy::audit::{AuditLog, NoIssuerKey};
use contextful_policy::enforce::mask::Pepper;
use contextful_policy::issue::{mint, MintClaims, SeedSigner};
use contextful_policy::keyset::{KeySource, StaticPins};
use contextful_policy::revoke::RevocationState;
use contextful_policy::verify::{verify_inherited_pipe, Admission, AdmittedAuthority};
use serde_json::{json, Value};

const AUD: &str = "contextful://acme-research";
const MANIFEST: &str = "[[pipeline.tables]]\nname = \"research/notes\"\n\n[[pipeline.tables]]\nname = \"hr/salaries\"\n";

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

/// The audit log is declared first so it closes before the directory goes.
pub(crate) struct Fixture {
    pub(crate) audit: AuditLog<NoIssuerKey>,
    pub(crate) face: Face,
    pub(crate) authority: AdmittedAuthority,
    pub(crate) dir: tempfile::TempDir,
}

/// Land one batch of `rows` into `decl`'s table as run `run`, committed at `now`.
fn put(store: &Store, decl: &TableDecl, run: &str, now: &str, rows: Value, types: &[(&str, ColumnType)]) {
    let rows = rows.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect();
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: run.into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint: None },
        committed_at: at(now),
    };
    let types = types.iter().map(|(c, t)| (c.to_string(), t.clone())).collect();
    land(store, decl, &Batch { rows, types }, &ctx).unwrap();
}

pub(crate) fn fixture() -> Fixture {
    fixture_over(MANIFEST, |store| {
        let notes = json!([{ "note_id": "n1", "title": "Solar battery storage" }, { "note_id": "n2", "title": "Hiring plan" }]);
        put(store, &TableDecl::named("research/notes"), "run-0001", "2030-01-01T00:00:00Z", notes, &[]);
        let salaries = json!([{ "employee": "e1", "title": "Battery storage engineer" }]);
        put(store, &TableDecl::named("hr/salaries"), "run-0001", "2030-01-01T00:00:00Z", salaries, &[]);
    })
}

pub(crate) fn fixture_over(manifest: &str, seed: impl FnOnce(&Store)) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), "research").unwrap();
    seed(&store);
    let face = Face::open(store, manifest, Pepper::resolve(|_| None)).unwrap();
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
        templates: Some(vec!["*".into()]),
        max_rows: None,
        max_duration_ms: None,
        max_response_bytes: None,
    };
    let mut req = MintRequest::custody(subject, vec![grant]);
    req.lifetime = Lifetime::Requested(900);
    let clock = FixedClock(at("2030-01-01T00:00:00Z"));
    let plan = policy.check(&req, &MintContext { node: NodeRole::Primary, signer: &signer, clock: &clock }).unwrap();
    let token = mint(&plan, &MintClaims::default(), &signer).unwrap();
    let keys = StaticPins::parse(&signer.public_key_text()).unwrap().keys().unwrap();
    let revocation = RevocationState::default();
    let authority = verify_inherited_pipe(&token, &keys, &Admission::new(at("2030-01-01T00:05:00Z"), &revocation).expecting(AUD)).unwrap();
    let audit = AuditLog::unanchored(dir.path().join("audit")).unwrap();
    Fixture { audit, face, authority, dir }
}

/// An effect boundary that admits the carried authority.
pub(crate) fn current(_: &AdmittedAuthority) -> Result<(), AuthorityError> {
    Ok(())
}

pub(crate) fn ask(server: &Server<'_>, id: u64, method: &str, params: Value) -> Value {
    let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
    let answer = server.handle(&line).expect("a request is answered");
    assert_eq!(answer["id"], json!(id));
    answer
}

pub(crate) fn call(server: &Server<'_>, tool: &str, arguments: Value) -> Value {
    ask(server, 7, "tools/call", json!({ "name": tool, "arguments": arguments }))
}

#[test]
fn reference_dispatch_is_read_admitted_bounded_and_audited() {
    let fixture = fixture();
    let clock = FixedClock(at("2030-01-01T00:06:00Z"));
    let server = Server::new(&fixture.face, fixture.authority.clone(), &current, &clock, &fixture.audit).unwrap();
    let result = call(&server, "context.reference", json!({ "table":"research/notes", "run":"never-landed", "seq":0 }));
    assert_eq!(result["result"]["structuredContent"]["rows"], json!([[false, "missing"]]), "{result}");
    let refused = call(&server, "context.reference", json!({ "table":"hr/salaries", "run":"never-landed", "seq":0 }));
    assert_eq!(refused["result"]["isError"], json!(true), "{refused}");
    let bytes = call(&server, "context.reference", json!({ "table":"research/notes", "run":"never-landed", "seq":0, "max_response_bytes":1 }));
    assert_eq!(bytes["result"]["isError"], json!(true), "{bytes}");
    let entries = contextful_policy::audit::entries(&fixture.dir.path().join("audit")).unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].attributes["contextful.tool"], json!("context.reference"));
}

/// The face exposes a closed tool set: `context.describe`, `context.query`, `context.execute_query` for templates, `context.files` and `context.file` over committed data files, `corpus.retrieve` for ranked reads across a prefix, and `memory.recall` for keyed claim reads.
// spec: read.register.tool-set@3b79e223
#[test]
fn the_tool_list_is_the_closed_read_set() {
    let f = fixture();
    let clock = FixedClock(at("2030-01-01T00:06:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let tools = ask(&server, 1, "tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["context.describe", "context.query", "context.reference", "context.execute_query", "context.files", "context.file", "corpus.retrieve", "memory.recall"]);
    let unknown = call(&server, "memory.write", json!({}));
    assert_eq!(unknown["error"]["code"], json!(-32602), "{unknown}");
}

/// On the tool protocol a refusal arrives in-band, as a protocol error object or a result flagged as an error under transport success.
// spec: read.respond.in-band-error@57469a30
#[test]
fn a_refusal_arrives_in_band() {
    let f = fixture();
    let clock = FixedClock(at("2030-01-01T00:06:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
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

/// `context.query` carries typed parameters over the tool protocol: a bound value answers
/// the read, a mismatched one arrives in-band as `QueryParameterRejected`, and a
/// `parameters` field that is no object is a protocol error.
#[test]
fn context_query_binds_typed_parameters() {
    let f = fixture();
    let clock = FixedClock(at("2030-01-01T00:06:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let tools = ask(&server, 1, "tools/list", json!({}));
    let query = tools["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == json!("context.query")).unwrap().clone();
    assert_eq!(query["inputSchema"]["properties"]["parameters"]["type"], json!("object"), "{query}");
    let sql = "SELECT note_id FROM \"research/notes\" WHERE note_id = $id";
    let answered = call(&server, "context.query", json!({ "sql": sql, "parameters": { "id": { "type": "string", "value": "n2" } } }));
    assert_eq!(answered["result"]["structuredContent"]["rows"], json!([["n2"]]), "{answered}");
    let mistyped = call(&server, "context.query", json!({ "sql": sql, "parameters": { "id": { "type": "integer", "value": "n2" } } }));
    assert_eq!(mistyped["result"]["isError"], json!(true), "{mistyped}");
    assert_eq!(mistyped["result"]["structuredContent"]["error"]["identifier"], json!("QueryParameterRejected"));
    let malformed = call(&server, "context.query", json!({ "sql": sql, "parameters": ["n2"] }));
    assert_eq!(malformed["error"]["code"], json!(-32602), "{malformed}");
}

// spec: read.register.budget-arguments@7d71066c
// spec: read.register.duration-no-statement@68942eca
#[test]
fn read_tools_advertise_and_enforce_request_budgets() {
    let f = fixture();
    let clock = FixedClock(at("2030-01-01T00:06:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let tools = ask(&server, 1, "tools/list", json!({}));
    for tool in tools["result"]["tools"].as_array().unwrap() {
        let properties = &tool["inputSchema"]["properties"];
        assert_eq!(properties["max_duration_ms"]["type"], json!("integer"), "{tool}");
        assert_eq!(properties["max_response_bytes"]["type"], json!("integer"), "{tool}");
    }
    let sql = "SELECT note_id FROM \"research/notes\"";
    let duration = call(&server, "context.query", json!({ "sql": sql, "max_duration_ms": 0 }));
    assert_eq!(duration["result"]["structuredContent"]["error"]["identifier"], json!("ReadDurationExceeded"), "{duration}");
    let bytes = call(&server, "context.query", json!({ "sql": sql, "max_response_bytes": 10 }));
    assert_eq!(bytes["result"]["structuredContent"]["error"]["identifier"], json!("ReadResponseTooLarge"), "{bytes}");
    for name in ["context.describe", "context.files"] {
        let bounded = call(&server, name, json!({ "max_response_bytes": 10 }));
        assert_eq!(bounded["result"]["structuredContent"]["error"]["identifier"], json!("ReadResponseTooLarge"), "{bounded}");
        let no_statement = call(&server, name, json!({ "max_duration_ms": 0 }));
        assert!(no_statement["result"].get("isError").is_none(), "{no_statement}");
    }
    let counted = call(&server, "context.describe", json!({ "table": "research/notes", "max_duration_ms": 0 }));
    assert_eq!(counted["result"]["structuredContent"]["error"]["identifier"], json!("ReadDurationExceeded"), "{counted}");
    let invalid = call(&server, "context.query", json!({ "sql": sql, "max_duration_ms": -1 }));
    assert_eq!(invalid["error"]["code"], json!(-32602));
}

#[test]
fn the_handshake_reports_the_build_and_refuses_an_absent_face() {
    let f = fixture();
    let clock = FixedClock(at("2030-01-01T00:06:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let init = ask(&server, 1, "initialize", json!({ "protocolVersion": "2025-06-18", "require": ["duckdb"] }));
    assert_eq!(init["result"]["contextful.build"]["backends"], json!(["duckdb", "fts", "hnsw"]));
    let absent = ask(&server, 2, "initialize", json!({ "require": ["m365"] }));
    assert!(absent["error"]["message"].as_str().unwrap().starts_with("RequiredFaceAbsent"), "{absent}");
    assert!(server.handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string()).is_none());
}

/// A server reporting no embedded SQL engine answers every read tool in-band with
/// `ReadBackendAbsent`, records the refusal, and reads no row; the handshake still answers.
#[test]
fn a_server_reporting_no_sql_engine_refuses_every_read_tool() {
    use contextful_core::read::face::BuildIdentity;
    let f = fixture();
    let clock = FixedClock(at("2030-01-01T00:06:00Z"));
    let unlinked = BuildIdentity { backends: vec!["fts".into()], connectors: Vec::new(), faces: Vec::new() };
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap().with_build(unlinked);
    let init = ask(&server, 1, "initialize", json!({}));
    assert_eq!(init["result"]["contextful.build"]["backends"], json!(["fts"]), "{init}");
    let reads = [
        ("context.describe", json!({ "table": "research/notes" })),
        ("context.query", json!({ "sql": "SELECT note_id FROM \"research/notes\"" })),
        ("context.files", json!({})),
        ("corpus.retrieve", json!({ "prefix": "research/", "query": "battery" })),
    ];
    for (tool, arguments) in reads {
        let refused = call(&server, tool, arguments);
        assert_eq!(refused["result"]["isError"], json!(true), "{refused}");
        assert_eq!(refused["result"]["structuredContent"]["error"]["identifier"], json!("ReadBackendAbsent"), "{refused}");
        assert!(refused["result"]["structuredContent"].get("rows").is_none(), "{refused}");
    }
    let entries = contextful_policy::audit::entries(&f.dir.path().join("audit")).unwrap();
    assert_eq!(entries.len(), 4);
    assert!(entries.iter().all(|e| e.attributes["contextful.read.refusal"] == json!("ReadBackendAbsent")), "{:?}", entries[0].attributes);
}

#[test]
fn every_call_re_reads_the_authority() {
    let f = fixture();
    let boundary = |_: &AdmittedAuthority| Err(AuthorityError::AuthorityExpired("expired at 2030-01-01T00:15:00Z".into()));
    let clock = FixedClock(at("2030-01-01T00:06:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &boundary, &clock, &f.audit).unwrap();
    let stopped = call(&server, "context.query", json!({ "sql": "SELECT note_id FROM \"research/notes\"" }));
    assert_eq!(stopped["result"]["isError"], json!(true));
    assert!(stopped["result"]["content"][0]["text"].as_str().unwrap().contains("AuthorityExpired"));
}

const BOUNDED: &str = r#"[[pipeline.tables]]
name = "research/filings"
primary_key = ["doc"]

[[pipeline.tables]]
name = "research/rates"
primary_key = ["ccy"]

[pipeline.tables.valid_time]
from = "from_ts"
to = "to_ts"

[[query_templates]]
id = "filing"
sql = "SELECT CAST(v AS VARCHAR) AS v FROM \"research/filings\" WHERE doc = ?"
parameters = ["doc:string"]

[[query_templates]]
id = "rates"
sql = "SELECT ccy FROM \"research/rates\" WHERE ccy <> ? ORDER BY ccy"
parameters = ["skip:string"]
"#;

/// A keyed table folded after its second run, beside a table declaring a valid-time pair.
pub(crate) fn bounded() -> Fixture {
    let decls = TableDecl::parse_pipeline(BOUNDED).unwrap();
    let decl = |name: &str| decls.iter().find(|d| d.name == name).unwrap().clone();
    fixture_over(BOUNDED, |store| {
        let filings = decl("research/filings");
        put(store, &filings, "run-1", "2030-01-01T00:00:00Z", json!([{ "doc": "a", "v": 1 }]), &[]);
        put(store, &filings, "run-2", "2030-01-01T02:00:00Z", json!([{ "doc": "a", "v": 2 }]), &[]);
        fold(store, &filings, at("2030-01-01T03:00:00Z")).unwrap();
        let ts = [("from_ts", ColumnType::Timestamp), ("to_ts", ColumnType::Timestamp)];
        let rates = json!([
            { "ccy": "eur", "from_ts": "2030-01-01T00:00:00Z", "to_ts": "2030-02-01T00:00:00Z" },
            { "ccy": "gbp", "from_ts": "2030-01-15T00:00:00Z", "to_ts": null },
        ]);
        put(store, &decl("research/rates"), "run-1", "2030-01-01T00:00:00Z", rates, &ts);
    })
}

fn rows(answer: &Value) -> &Value {
    assert!(answer["result"].get("isError").is_none(), "{answer}");
    &answer["result"]["structuredContent"]["rows"]
}

fn echoed(answer: &Value) -> &Value {
    &answer["result"]["structuredContent"]["contextful.bounds"]
}

/// Over a keyed table folded after its second run, `as_of` on every read tool returns the pre-fold row and echoes the bound.
#[test]
fn as_of_on_every_read_tool_returns_the_pre_fold_row() {
    let f = bounded();
    let clock = FixedClock(at("2030-01-01T04:00:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let sql = r#"SELECT CAST(v AS VARCHAR) AS v FROM "research/filings""#;
    let before = "2030-01-01T01:00:00Z";
    let echo = json!({ "as_of": "2030-01-01T01:00:00.000000000Z", "inclusive": true });

    let latest = call(&server, "context.query", json!({ "sql": sql }));
    assert_eq!(rows(&latest), &json!([["2"]]));
    assert!(latest["result"]["structuredContent"].get("contextful.bounds").is_none(), "{latest}");

    let old = call(&server, "context.query", json!({ "sql": sql, "as_of": before }));
    assert_eq!(rows(&old), &json!([["1"]]));
    assert_eq!(echoed(&old), &echo);

    let executed = call(&server, "context.execute_query", json!({ "id": "filing", "arguments": { "doc": "a" }, "as_of": before }));
    assert_eq!(rows(&executed), &json!([["1"]]));
    assert_eq!(echoed(&executed), &echo);

    let template = call(&server, "filing", json!({ "doc": "a", "as_of": before }));
    assert_eq!(rows(&template), &json!([["1"]]));
    assert_eq!(echoed(&template), &echo);

    let described = call(&server, "context.describe", json!({ "table": "research/filings", "as_of": before }));
    assert_eq!(echoed(&described), &echo, "{described}");

    // The listing under the bound holds run-1 alone; its preview reads, and run-2's refuses.
    let listed = call(&server, "context.files", json!({ "as_of": before }));
    let paths: Vec<String> = rows(&listed)
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r[0] == json!("research/filings"))
        .map(|r| r[1].as_str().unwrap().to_string())
        .collect();
    assert_eq!(paths.len(), 1, "{listed}");
    assert!(paths[0].contains("/runs/run-1/"), "{listed}");
    assert_eq!(echoed(&listed), &echo);
    let preview = call(&server, "context.file", json!({ "path": paths[0], "as_of": before }));
    assert_eq!(rows(&preview).as_array().unwrap().len(), 1);
    assert_eq!(echoed(&preview), &echo);
    let later = paths[0].replace("/runs/run-1/", "/runs/run-2/");
    let refused = call(&server, "context.file", json!({ "path": later, "as_of": before }));
    assert_eq!(refused["result"]["isError"], json!(true), "{refused}");
}

/// Every read tool but `memory.recall`, each template tool included, admits `as_of` and `valid_as_of` and echoes {{store.bound-time.echo}}; {{store.bound-time.valid-as-of}} wraps only the tables the read touches.
// spec: read.register.bound-arguments@8d3edd37
#[test]
fn valid_as_of_wraps_only_the_tables_a_read_touches() {
    let f = bounded();
    let clock = FixedClock(at("2030-01-01T04:00:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let when = "2030-01-10T00:00:00Z";
    let echo = json!({ "valid_as_of": "2030-01-10T00:00:00.000000000Z", "inclusive": true });
    let sql = r#"SELECT ccy FROM "research/rates" ORDER BY ccy"#;

    // `research/filings` declares no pair and sits in the session; the statement touches `research/rates` alone.
    let rates = call(&server, "context.query", json!({ "sql": sql, "valid_as_of": when }));
    assert_eq!(rows(&rates), &json!([["eur"]]));
    assert_eq!(echoed(&rates), &echo);
    let both = call(&server, "context.query", json!({ "sql": sql, "valid_as_of": "2030-01-20T00:00:00Z" }));
    assert_eq!(rows(&both), &json!([["eur"], ["gbp"]]));

    let executed = call(&server, "context.execute_query", json!({ "id": "rates", "arguments": { "skip": "usd" }, "valid_as_of": when }));
    assert_eq!(rows(&executed), &json!([["eur"]]));
    assert_eq!(echoed(&executed), &echo);
    let template = call(&server, "rates", json!({ "skip": "usd", "valid_as_of": when }));
    assert_eq!(rows(&template), &json!([["eur"]]));
    assert_eq!(echoed(&template), &echo);

    let undeclared = call(&server, "context.query", json!({ "sql": r#"SELECT v FROM "research/filings""#, "valid_as_of": when }));
    assert_eq!(undeclared["result"]["isError"], json!(true), "{undeclared}");
    assert!(undeclared["result"]["content"][0]["text"].as_str().unwrap().contains("research/filings"), "{undeclared}");

    let described = call(&server, "context.describe", json!({ "table": "research/rates", "valid_as_of": when }));
    assert_eq!(described["result"]["structuredContent"]["row_count"], json!("1"), "{described}");
    assert_eq!(echoed(&described), &echo);

    let listed = call(&server, "context.files", json!({}));
    let part = rows(&listed).as_array().unwrap().iter().find(|r| r[0] == json!("research/rates")).unwrap()[1].clone();
    let preview = call(&server, "context.file", json!({ "path": part, "valid_as_of": when }));
    assert_eq!(rows(&preview).as_array().unwrap().len(), 1, "{preview}");
    assert_eq!(echoed(&preview), &echo);

    let retrieved = call(&server, "corpus.retrieve", json!({ "prefix": "research/rates", "query": "eur gbp", "valid_as_of": when }));
    assert_eq!(rows(&retrieved).as_array().unwrap().len(), 1, "{retrieved}");
    assert_eq!(echoed(&retrieved), &echo);
}

/// `context.files` and a `context.describe` naming no table select under `as_of` alone; each ignores `valid_as_of` and echoes only its `as_of` part.
// spec: read.register.bound-listing@9360c1d2
#[test]
fn a_listing_ignores_valid_as_of_and_echoes_only_as_of() {
    let f = bounded();
    let clock = FixedClock(at("2030-01-01T04:00:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let when = "2030-01-10T00:00:00Z";
    let before = "2030-01-01T01:00:00Z";
    let echo = json!({ "as_of": "2030-01-01T01:00:00.000000000Z", "inclusive": true });

    let unbounded = call(&server, "context.files", json!({}));
    let valid_only = call(&server, "context.files", json!({ "valid_as_of": when }));
    assert_eq!(rows(&valid_only), rows(&unbounded));
    assert!(valid_only["result"]["structuredContent"].get("contextful.bounds").is_none(), "{valid_only}");
    let both = call(&server, "context.files", json!({ "as_of": before, "valid_as_of": when }));
    assert_eq!(rows(&both), rows(&call(&server, "context.files", json!({ "as_of": before }))));
    assert_eq!(echoed(&both), &echo);

    let tables = call(&server, "context.describe", json!({ "valid_as_of": when }));
    assert!(tables["result"]["structuredContent"].get("contextful.bounds").is_none(), "{tables}");
    let tables = call(&server, "context.describe", json!({ "as_of": before, "valid_as_of": when }));
    assert_eq!(echoed(&tables), &echo, "{tables}");
}

#[test]
fn every_read_tool_declares_both_bounds() {
    let f = bounded();
    let clock = FixedClock(at("2030-01-01T04:00:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let tools = ask(&server, 1, "tools/list", json!({}));
    let tools = tools["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == json!("filing")));
    for tool in tools {
        let properties = &tool["inputSchema"]["properties"];
        let (present, absent) = if tool["name"] == json!("memory.recall") {
            (["observed_at", "as_of_ingest"], ["as_of", "valid_as_of"])
        } else {
            (["as_of", "valid_as_of"], ["observed_at", "as_of_ingest"])
        };
        for bound in present {
            assert_eq!(properties[bound]["type"], json!("string"), "{} lacks `{bound}`", tool["name"]);
        }
        for bound in absent {
            assert!(properties.get(bound).is_none(), "{} declares `{bound}`", tool["name"]);
        }
    }
}

const CLAIMS: &str = r#"[[pipeline.tables]]
name = "research/notes"

[[table]]
name = "research/facts"
shape = "memory_facts"
columns = ["claim_id", "subject", "predicate", "object", "scope", "tier", "confidence", "valid_from", "valid_to", "evidence", "superseded_by", "grant_id", "agent"]
"#;

/// Dana holds from 2030-01-01 until Lee's 2030-03-01 start retires her.
fn claims() -> Fixture {
    fixture_over(CLAIMS, |store| {
        put(store, &TableDecl::named("research/notes"), "run-0001", "2030-01-01T00:00:00Z", json!([{ "note_id": "n1" }]), &[]);
        let evidence = r#"[{"table":"research/notes","run":"run-0001","seq":0}]"#;
        let claim = |id: &str, object: &str, from: &str, to: Option<&str>, by: Option<&str>| {
            json!({ "claim_id": id, "subject": "acme", "predicate": "cfo", "object": object, "scope": null, "tier": "curated",
                "confidence": 1.0, "valid_from": from, "valid_to": to, "evidence": evidence, "superseded_by": by,
                "grant_id": "g", "agent": null })
        };
        let rows = json!([
            claim("c-dana", "Dana", "2030-01-01T00:00:00Z", Some("2030-03-01T00:00:00Z"), Some("c-lee")),
            claim("c-lee", "Lee", "2030-03-01T00:00:00Z", None, None),
        ]);
        let ts = [
            ("valid_from", ColumnType::Timestamp),
            ("valid_to", ColumnType::Timestamp),
            ("confidence", ColumnType::Float64),
            ("scope", ColumnType::Utf8),
            ("superseded_by", ColumnType::Utf8),
            ("agent", ColumnType::Utf8),
        ];
        let decl = TableDecl { primary_key: Some(vec!["claim_id".into()]), ..TableDecl::named("research/facts") };
        put(store, &decl, "memory-1", "2030-01-02T00:00:00Z", rows, &ts);
    })
}

/// Executed SQL, engine name, applied limit, row count and elapsed milliseconds ride a separate object returned only under `internals: true`, on every read tool and the HTTP face.
// spec: read.respond.internals-opt-in@82ae2a18
#[test]
fn every_read_tool_returns_internals_only_on_request() {
    let f = claims();
    let clock = FixedClock(at("2030-06-01T00:00:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let listed = call(&server, "context.files", json!({}));
    let path = rows(&listed).as_array().unwrap().iter().find(|r| r[0] == json!("research/notes")).unwrap()[1].clone();
    let reads = [
        ("context.describe", json!({ "table": "research/notes" }), true),
        ("context.describe", json!({}), false),
        ("context.query", json!({ "sql": "SELECT note_id FROM \"research/notes\"" }), true),
        ("context.reference", json!({ "table": "research/notes", "run": "run-0001", "seq": 0 }), true),
        ("context.files", json!({}), false),
        ("context.file", json!({ "path": path }), true),
        ("corpus.retrieve", json!({ "prefix": "research/", "query": "acme cfo" }), true),
        ("memory.recall", json!({ "table": "research/facts", "subject": "acme" }), true),
    ];
    for (tool, arguments, runs_sql) in reads {
        let plain = call(&server, tool, arguments.clone());
        let content = &plain["result"]["structuredContent"];
        assert!(plain["result"].get("isError").is_none(), "{tool}: {plain}");
        assert!(content.get("contextful.internals").is_none(), "{tool}: {plain}");
        let mut asked = arguments.clone();
        asked["internals"] = json!(true);
        let answer = call(&server, tool, asked);
        let internals = &answer["result"]["structuredContent"]["contextful.internals"];
        assert_eq!(internals["engine"], json!("duckdb"), "{tool}: {answer}");
        assert!(internals["row_count"].is_u64() && internals["elapsed_ms"].is_u64(), "{tool}: {internals}");
        assert!(internals.get("limit").is_some(), "{tool}: {internals}");
        assert_eq!(internals["sql"].is_string(), runs_sql, "{tool}: {internals}");
    }
}

/// `memory.recall` answers over the tool protocol with the subject's claims at `observed_at`,
/// takes `as_of_ingest` for transaction time, and takes no `as_of` or `valid_as_of`.
#[test]
fn memory_recall_answers_keyed_over_the_tool_protocol() {
    let f = claims();
    let clock = FixedClock(at("2030-06-01T00:00:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let objects = |answer: &Value| -> Vec<Value> {
        let content = &answer["result"]["structuredContent"];
        let i = content["columns"].as_array().unwrap().iter().position(|c| c == "object").unwrap();
        rows(answer).as_array().unwrap().iter().map(|r| r[i].clone()).collect()
    };
    let then = call(&server, "memory.recall", json!({ "table": "research/facts", "subject": "acme", "observed_at": "2030-02-01T00:00:00Z" }));
    assert_eq!(objects(&then), [json!("Dana")], "{then}");
    assert_eq!(then["result"]["structuredContent"]["contextful.recall"]["suppressed"]["MemoryEvidenceUnresolved"], json!(0));
    let now = call(&server, "memory.recall", json!({ "table": "research/facts", "subject": "acme", "limit": 5 }));
    assert_eq!(objects(&now), [json!("Lee")], "{now}");
    let unknown = call(&server, "memory.recall", json!({ "table": "research/facts", "subject": "acme", "as_of_ingest": "2030-01-01T12:00:00Z" }));
    assert_eq!(rows(&unknown), &json!([]), "{unknown}");
    // The echo names the tool's own arguments, so a client passes it back unchanged.
    let echo = json!({ "as_of_ingest": "2030-01-01T12:00:00.000000000Z", "inclusive": { "as_of_ingest": true } });
    assert_eq!(echoed(&unknown), &echo);
    let replayed = call(&server, "memory.recall", json!({ "table": "research/facts", "subject": "acme", "as_of_ingest": echo["as_of_ingest"] }));
    assert_eq!(echoed(&replayed), &echo, "{replayed}");
    for bound in ["as_of", "valid_as_of"] {
        let refused = call(&server, "memory.recall", json!({ "table": "research/facts", "subject": "acme", bound: "2030-02-01" }));
        assert_eq!(refused["error"]["code"], json!(-32602), "{refused}");
    }
    let missing = call(&server, "memory.recall", json!({ "table": "research/facts" }));
    assert_eq!(missing["error"]["code"], json!(-32602), "{missing}");
    let notes = call(&server, "memory.recall", json!({ "table": "research/notes", "subject": "acme" }));
    assert_eq!(notes["result"]["structuredContent"]["error"]["identifier"], json!("MemoryRecallNotClaims"), "{notes}");
}

/// A `memory.recall` read, and a `corpus.retrieve` read returning `memory_facts` rows, records the returned claim ids as `contextful.memory.claims` in its read entry, beside the caller's credential, subject and read instant.
// spec: read.recall.usage-ledger@cbc45e6f
#[test]
fn memory_reads_record_the_claim_ids_they_returned() {
    let f = claims();
    let clock = FixedClock(at("2030-06-01T00:00:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    call(&server, "memory.recall", json!({ "table": "research/facts", "subject": "acme", "observed_at": "2030-02-01T00:00:00Z" }));
    call(&server, "memory.recall", json!({ "table": "research/facts", "subject": "nobody" }));
    let ranked = call(&server, "corpus.retrieve", json!({ "prefix": "research/facts", "query": "acme cfo" }));
    assert_eq!(rows(&ranked).as_array().map(Vec::len), Some(1), "{ranked}");
    call(&server, "corpus.retrieve", json!({ "prefix": "research/notes", "query": "n1" }));
    call(&server, "context.query", json!({ "sql": "SELECT claim_id FROM \"research/facts\"" }));

    let entries = contextful_policy::audit::entries(&f.dir.path().join("audit")).unwrap();
    let claims: Vec<&Value> = entries.iter().map(|e| &e.attributes["contextful.memory.claims"]).collect();
    assert_eq!(claims, [&json!(["c-dana"]), &json!([]), &json!(["c-lee"]), &Value::Null, &Value::Null]);
    let first = &entries[0].attributes;
    assert_eq!(first["contextful.credential"], json!(f.authority.credential_id()));
    assert_eq!(first["contextful.subject.on_behalf_of"], json!("user://dana@acme.example"));
    assert_eq!(first["contextful.read.at"], json!(at("2030-06-01T00:00:00Z").to_rfc3339()));
}

/// `corpus.retrieve({prefix, query, query_embedding?, filter?, kinds?, limit?, since?, min_score?, as_of?})` returns the top rows across the item and artifact genres under one prefix, each with a snippet and full provenance. It composes the query surface and memory recall, storing nothing.
// spec: read.retrieve.ranked-call@ea39e5ac
#[test]
fn corpus_retrieve_takes_a_filter_and_kinds() {
    let f = fixture();
    let clock = FixedClock(at("2030-01-01T00:06:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let tools = ask(&server, 1, "tools/list", json!({}));
    let retrieve = tools["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == json!("corpus.retrieve")).unwrap().clone();
    let properties = &retrieve["inputSchema"]["properties"];
    assert_eq!(properties["filter"]["type"], json!("object"), "{retrieve}");
    assert_eq!(properties["kinds"]["items"]["type"], json!("string"), "{retrieve}");
    for argument in ["prefix", "query", "query_embedding", "limit", "since", "min_score", "as_of"] {
        assert!(properties.get(argument).is_some(), "`corpus.retrieve` lacks `{argument}`");
    }

    let base = json!({ "prefix": "research/", "query": "solar battery storage", "min_score": 0 });
    let with = |extra: Value| {
        let mut a = base.clone();
        a.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        a
    };
    let unfiltered = call(&server, "corpus.retrieve", base.clone());
    assert_eq!(rows(&unfiltered).as_array().unwrap().len(), 2, "{unfiltered}");
    let one = call(&server, "corpus.retrieve", with(json!({ "filter": { "note_id": "n2" } })));
    let one = rows(&one).as_array().unwrap().clone();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0][1]["note_id"], json!("n2"));
    // No note carries a `kind` column, so the notes arm drops.
    let memos = call(&server, "corpus.retrieve", with(json!({ "kinds": ["memo"] })));
    assert_eq!(rows(&memos), &json!([]));

    let values: Vec<String> = (0..257).map(|i| format!("n{i}")).collect();
    let over = call(&server, "corpus.retrieve", with(json!({ "filter": { "note_id": values } })));
    assert_eq!(over["result"]["isError"], json!(true), "{over}");
    assert_eq!(over["result"]["structuredContent"]["error"]["identifier"], json!("FilterBudgetExceeded"));
    let malformed = call(&server, "corpus.retrieve", with(json!({ "kinds": "memo" })));
    assert_eq!(malformed["error"]["code"], json!(-32602), "{malformed}");
}

const PINNED: &str = r#"[[pipeline.tables]]
name = "research/notes"

[[model]]
id = "research/titles"
sql = "SELECT note_id, title FROM \"research/notes\""
unique_key = ["note_id"]

[model.contract]
version = "1.0.0"
columns = [{ name = "note_id", type = "utf8", nullable = false }, { name = "title", type = "utf8" }]
"#;

/// A template over the model, checked when the face opens over the built store.
const TITLES_TEMPLATE: &str = r#"
[[query_templates]]
id = "titles"
sql = "SELECT note_id FROM \"research/titles\" ORDER BY note_id"
"#;

/// Build `research/titles` over the store at `now`; its build id.
fn build_titles(store: &Store, now: &str) -> String {
    use contextful_context::build::{build, BuildRequest};
    use contextful_core::pipeline::declare::{collect, ManifestFile};
    use contextful_core::pipeline::model::collect_models;
    let files = [ManifestFile { path: "contextful.toml".into(), text: PINNED.to_string() }];
    let spec = collect_models(&files, &collect(&files).unwrap()).unwrap().remove(0).spec;
    let face = Face::open(store.clone(), PINNED, Pepper::resolve(|_| None)).unwrap();
    build(&face, &BuildRequest { model: &spec, site_id: "site-a", started_at: at(now), completed_at: at(now) }).unwrap().build_id
}

/// Every read tool declares and admits `pin`; each answer touching the pinned model reads
/// and echoes the pinned build, and a malformed map is a protocol error.
#[test]
fn every_read_tool_admits_a_pin_map() {
    let notes = TableDecl::named("research/notes");
    let mut first = String::new();
    let f = fixture_over(&format!("{PINNED}{TITLES_TEMPLATE}"), |store| {
        put(store, &notes, "run-0001", "2030-01-01T00:00:00Z", json!([{ "note_id": "n1", "title": "Solar battery storage" }]), &[]);
        first = build_titles(store, "2030-01-01T01:00:00Z");
        put(store, &notes, "run-0002", "2030-01-01T02:00:00Z", json!([{ "note_id": "n2", "title": "Battery storage grids" }]), &[]);
        build_titles(store, "2030-01-01T03:00:00Z");
    });
    let clock = FixedClock(at("2030-01-01T04:00:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let tools = ask(&server, 1, "tools/list", json!({}));
    for tool in tools["result"]["tools"].as_array().unwrap() {
        assert_eq!(tool["inputSchema"]["properties"]["pin"]["type"], json!("object"), "{} lacks `pin`", tool["name"]);
    }

    let pin = json!({ "research/titles": first });
    let echoed_build = |answer: &Value| answer["result"]["structuredContent"]["contextful.resolved"]["research/titles"]["build_id"].clone();
    let sql = r#"SELECT note_id FROM "research/titles" ORDER BY note_id"#;
    for answer in [
        call(&server, "context.query", json!({ "sql": sql, "pin": pin })),
        call(&server, "context.execute_query", json!({ "id": "titles", "pin": pin })),
        call(&server, "titles", json!({ "pin": pin })),
    ] {
        assert_eq!(rows(&answer), &json!([["n1"]]));
        assert_eq!(echoed_build(&answer), json!(first), "{answer}");
    }
    let described = call(&server, "context.describe", json!({ "table": "research/titles", "pin": pin }));
    assert_eq!(described["result"]["structuredContent"]["row_count"], json!("1"), "{described}");
    assert_eq!(echoed_build(&described), json!(first));
    let listed = call(&server, "context.files", json!({ "pin": pin }));
    assert!(rows(&listed).as_array().unwrap().iter().any(|r| r[1].as_str().unwrap().contains(&format!("/{first}/"))), "{listed}");
    assert_eq!(echoed_build(&listed), json!(first));
    let ranked = call(&server, "corpus.retrieve", json!({ "prefix": "research/titles", "query": "battery storage", "pin": pin }));
    assert_eq!(rows(&ranked).as_array().unwrap().len(), 1, "{ranked}");
    assert_eq!(echoed_build(&ranked), json!(first));

    // A null pin reads the latest build; a malformed map is a protocol error.
    let latest = call(&server, "context.query", json!({ "sql": sql, "pin": { "research/titles": null } }));
    assert_eq!(rows(&latest), &json!([["n1"], ["n2"]]));
    assert_ne!(echoed_build(&latest), json!(first));
    let malformed = call(&server, "context.query", json!({ "sql": sql, "pin": ["research/titles"] }));
    assert_eq!(malformed["error"]["code"], json!(-32602), "{malformed}");
    let unknown = call(&server, "context.query", json!({ "sql": sql, "pin": { "research/titles": "snapshot-garbage" } }));
    assert_eq!(unknown["result"]["structuredContent"]["error"]["identifier"], json!("PinnedBuildUnavailable"), "{unknown}");
}
