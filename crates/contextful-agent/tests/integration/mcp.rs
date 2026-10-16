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
pub(crate) fn put(store: &Store, decl: &TableDecl, run: &str, now: &str, rows: Value, types: &[(&str, ColumnType)]) {
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
    fixture_bounded(manifest, None, seed)
}

/// [`fixture_over`] with the credential's one grant carrying `horizon`.
pub(crate) fn fixture_bounded(manifest: &str, horizon: Option<&str>, seed: impl FnOnce(&Store)) -> Fixture {
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
        horizon: horizon.map(at),
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
    let bounded = call(&server, "context.reference", json!({ "table":"research/notes", "run":"run-0001", "seq":0, "as_of":"2030-01-01T00:05:00Z" }));
    assert_eq!(bounded["result"]["isError"], Value::Null, "{bounded}");
    let entries = contextful_policy::audit::entries(&fixture.dir.path().join("audit")).unwrap();
    assert!(entries[3].attributes["contextful.read.snapshots"]["research/notes"].is_string(), "{:?}", entries[3].attributes);
}

/// The face exposes a closed tool set: `context.describe`, `context.query`, `context.execute_query` for templates, `context.files` and `context.file` over committed data files, `corpus.retrieve` for ranked reads across a prefix, and `memory.recall` for keyed claim reads.
// spec: read.register.tool-set@ec1eab57
#[test]
fn the_tool_list_is_the_closed_read_set() {
    let f = fixture();
    let clock = FixedClock(at("2030-01-01T00:06:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let tools = ask(&server, 1, "tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["context.describe", "context.query", "context.reference", "context.execute_query", "context.files", "context.file", "context.subscribe", "corpus.retrieve", "memory.recall"]);
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

// spec: read.register.budget-arguments@5528cc89
// spec: read.register.duration-no-statement@68942eca
#[test]
fn read_tools_advertise_and_enforce_request_budgets() {
    let f = fixture();
    let clock = FixedClock(at("2030-01-01T00:06:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let tools = ask(&server, 1, "tools/list", json!({}));
    for tool in tools["result"]["tools"].as_array().unwrap() {
        let properties = &tool["inputSchema"]["properties"];
        if tool["name"] == json!("context.subscribe") {
            assert!(properties.get("max_duration_ms").is_none() && properties.get("max_response_bytes").is_none(), "{tool}");
            continue;
        }
        assert_eq!(properties["max_duration_ms"]["type"], json!("integer"), "{tool}");
        assert_eq!(properties["max_response_bytes"]["type"], json!("integer"), "{tool}");
    }
    let page = call(&server, "context.subscribe", json!({ "tables": ["research/notes"], "max_response_bytes": 10 }));
    assert_eq!(page["error"]["code"], json!(-32602), "{page}");
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

/// A covered template over a table outside the credential's raw read grants answers through
/// both template tools, and a raw query of that table stays refused.
#[test]
fn a_template_reads_its_tables_through_the_server_while_a_raw_query_cannot() {
    let manifest = format!("{MANIFEST}\n[[query_templates]]\nid = \"staff\"\nsql = \"SELECT employee FROM \\\"hr/salaries\\\" ORDER BY employee\"\n");
    let f = fixture_over(&manifest, |store| {
        let salaries = json!([{ "employee": "e1", "title": "Battery storage engineer" }]);
        put(store, &TableDecl::named("hr/salaries"), "run-0001", "2030-01-01T00:00:00Z", salaries, &[]);
    });
    let clock = FixedClock(at("2030-01-01T00:06:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    assert_eq!(rows(&call(&server, "context.execute_query", json!({ "id": "staff" }))), &json!([["e1"]]));
    assert_eq!(rows(&call(&server, "staff", json!({}))), &json!([["e1"]]));
    let raw = call(&server, "context.query", json!({ "sql": r#"SELECT employee FROM "hr/salaries""# }));
    assert_eq!(raw["result"]["isError"], json!(true), "{raw}");
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

/// Every read tool but `memory.recall` and `context.subscribe`, each template tool included, admits `as_of` and `valid_as_of` and echoes {{store.bound-time.echo}}; {{store.bound-time.valid-as-of}} wraps only the tables the read touches.
// spec: read.register.bound-arguments@0ce7bf68
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

    // A subscription reads the commit record, never rows under a bound.
    let subscribed = call(&server, "context.subscribe", json!({ "tables": ["research/rates"], "valid_as_of": when }));
    assert_eq!(subscribed["error"]["code"], json!(-32602), "{subscribed}");

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

const AVAILABLE: &str = r#"[[pipeline.tables]]
name = "research/filings"
primary_key = ["doc"]

[[pipeline.tables]]
name = "research/earnings"
primary_key = ["filing_id"]
content_hash_column = "digest"

[pipeline.tables.available_time]
column = "accepted_at"

[[query_templates]]
id = "earnings"
sql = "SELECT filing_id FROM \"research/earnings\" WHERE ticker = ? ORDER BY filing_id"
parameters = ["ticker:string"]
"#;

/// Two quarters' earnings, each accepted after its quarter closes, and a restatement of the
/// first, all landed on 1 September, beside a keyed table declaring no availability time.
fn available() -> Fixture {
    available_under(None)
}

/// [`available`] read by a credential carrying `horizon`.
pub(crate) fn available_under(horizon: Option<&str>) -> Fixture {
    let decls = TableDecl::parse_pipeline(AVAILABLE).unwrap();
    let decl = |name: &str| decls.iter().find(|d| d.name == name).unwrap().clone();
    fixture_bounded(AVAILABLE, horizon, |store| {
        put(store, &decl("research/filings"), "run-1", "2030-01-01T00:00:00Z", json!([{ "doc": "a", "v": 1 }]), &[]);
        let earnings = json!([
            { "filing_id": "acme-q1", "ticker": "ACME", "eps": "1.23", "digest": "q1", "accepted_at": "2030-05-02T16:05:00Z" },
            { "filing_id": "acme-q2", "ticker": "ACME", "eps": "1.31", "digest": "q2", "accepted_at": "2030-08-01T16:05:00Z" },
            { "filing_id": "acme-q1", "ticker": "ACME", "eps": "1.19", "digest": "q1r", "accepted_at": "2030-08-15T16:05:00Z" },
        ]);
        put(store, &decl("research/earnings"), "run-1", "2030-09-01T00:00:00Z", earnings, &[("accepted_at", ColumnType::Timestamp)]);
    })
}

/// A bounded read's {{disclosure.record.read-attributes}} entry adds `contextful.bounds` as {{store.bound-time.echo}} echoes it, the snapshot id the read resolved, the query text and the response's SHA-256 digest.
// spec: disclosure.record.bound-entry@81906c4f
#[test]
fn a_bounded_reads_entry_carries_its_bounds_snapshot_query_and_digest() {
    use contextful_core::run::journal::sha256_hex;
    let f = bounded();
    let clock = FixedClock(at("2030-01-01T04:00:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let sql = r#"SELECT CAST(v AS VARCHAR) AS v FROM "research/filings""#;

    let unbounded = call(&server, "context.query", json!({ "sql": sql }));
    let old = call(&server, "context.query", json!({ "sql": sql, "as_of": "2030-01-01T01:00:00Z" }));
    let later = call(&server, "context.query", json!({ "sql": sql, "as_of": "2030-01-01T02:30:00Z" }));
    let retrieved = call(&server, "corpus.retrieve", json!({ "prefix": "research/rates", "query": "eur", "valid_as_of": "2030-01-10T00:00:00Z" }));
    let entries = contextful_policy::audit::entries(&f.dir.path().join("audit")).unwrap();
    assert_eq!(entries.len(), 4);

    // An unbounded read's entry carries none of the four.
    for key in ["contextful.bounds", "contextful.read.snapshots", "contextful.read.query", "contextful.result.sha256"] {
        assert!(entries[0].attributes.get(key).is_none(), "{key}: {:?}", entries[0].attributes);
    }
    let _ = unbounded;

    let bounded_entry = &entries[1].attributes;
    assert_eq!(bounded_entry["contextful.bounds"], json!({ "as_of": "2030-01-01T01:00:00.000000000Z", "inclusive": true }));
    assert_eq!(bounded_entry["contextful.read.query"], json!(sql));
    let structured = serde_json::to_vec(&old["result"]["structuredContent"]).unwrap();
    assert_eq!(bounded_entry["contextful.result.sha256"], json!(sha256_hex(&structured)));
    let snapshot = bounded_entry["contextful.read.snapshots"]["research/filings"].as_str().unwrap().to_string();
    assert_eq!(snapshot.len(), 64, "{bounded_entry:?}");
    // Another `as_of` resolves another committed file set, so another snapshot id.
    assert_ne!(entries[2].attributes["contextful.read.snapshots"]["research/filings"], json!(snapshot));
    assert_ne!(entries[2].attributes["contextful.result.sha256"], bounded_entry["contextful.result.sha256"]);
    let _ = later;

    // A ranked read records its query string under its valid-time bound.
    assert_eq!(entries[3].attributes["contextful.read.query"], json!("eur"));
    assert_eq!(entries[3].attributes["contextful.bounds"]["valid_as_of"], json!("2030-01-10T00:00:00.000000000Z"));
    assert!(entries[3].attributes["contextful.read.snapshots"].get("research/rates").is_some(), "{:?}", entries[3].attributes);
    let _ = retrieved;
}

#[test]
fn a_sessions_snapshot_id_survives_a_later_commit() {
    use contextful_policy::enforce::session::Request;
    let f = fixture();
    let session = f.face.session(&f.authority, &Request { zone: None }, Default::default()).unwrap();
    let before = f.face.snapshot_ids(&session, ["research/notes"]).unwrap();
    put(f.face.store(), &TableDecl::named("research/notes"), "run-0002", "2030-01-02T00:00:00Z", json!([{ "note_id": "n3", "title": "Later note" }]), &[]);
    assert_eq!(f.face.snapshot_ids(&session, ["research/notes"]).unwrap(), before);
    let newer = f.face.session(&f.authority, &Request { zone: None }, Default::default()).unwrap();
    assert_ne!(f.face.snapshot_ids(&newer, ["research/notes"]).unwrap(), before);
}

/// Every read tool {{read.register.bound-arguments}} names also admits `available_as_of` and echoes it under {{store.available-time.bound-echo}}; {{store.available-time.available-as-of}} wraps only the tables the read touches.
// spec: read.register.available-bound-argument@8d2ed6a1
#[test]
fn available_as_of_wraps_only_the_tables_a_read_touches() {
    let f = available();
    let clock = FixedClock(at("2030-09-02T00:00:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let when = "2030-06-01T00:00:00Z";
    let echo = json!({ "available_as_of": "2030-06-01T00:00:00.000000000Z", "inclusive": true });
    let sql = r#"SELECT filing_id FROM "research/earnings" ORDER BY filing_id"#;

    // `research/filings` declares no availability time and sits in the session; the statement touches `research/earnings` alone.
    let june = call(&server, "context.query", json!({ "sql": sql, "available_as_of": when }));
    assert_eq!(rows(&june), &json!([["acme-q1"]]));
    assert_eq!(echoed(&june), &echo);
    let september = call(&server, "context.query", json!({ "sql": sql, "available_as_of": "2030-09-01" }));
    assert_eq!(rows(&september), &json!([["acme-q1"], ["acme-q2"]]));
    let april = call(&server, "context.query", json!({ "sql": sql, "available_as_of": "2030-04-30T00:00:00Z" }));
    assert_eq!(rows(&april), &json!([]));

    let executed = call(&server, "context.execute_query", json!({ "id": "earnings", "arguments": { "ticker": "ACME" }, "available_as_of": when }));
    assert_eq!(rows(&executed), &json!([["acme-q1"]]));
    assert_eq!(echoed(&executed), &echo);
    let template = call(&server, "earnings", json!({ "ticker": "ACME", "available_as_of": when }));
    assert_eq!(rows(&template), &json!([["acme-q1"]]));
    assert_eq!(echoed(&template), &echo);

    let undeclared = call(&server, "context.query", json!({ "sql": r#"SELECT v FROM "research/filings""#, "available_as_of": when }));
    assert_eq!(undeclared["result"]["isError"], json!(true), "{undeclared}");
    let text = undeclared["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("StoreAvailableTimeUndeclared") && text.contains("research/filings"), "{undeclared}");

    let described = call(&server, "context.describe", json!({ "table": "research/earnings", "available_as_of": when }));
    assert_eq!(described["result"]["structuredContent"]["row_count"], json!("1"), "{described}");
    assert_eq!(echoed(&described), &echo);

    let listed = call(&server, "context.files", json!({}));
    let part = rows(&listed).as_array().unwrap().iter().find(|r| r[0] == json!("research/earnings")).unwrap()[1].clone();
    let preview = call(&server, "context.file", json!({ "path": part, "available_as_of": when }));
    assert_eq!(rows(&preview).as_array().unwrap().len(), 1, "{preview}");
    assert_eq!(echoed(&preview), &echo);

    let retrieved = call(&server, "corpus.retrieve", json!({ "prefix": "research/earnings", "query": "acme", "available_as_of": when }));
    assert_eq!(rows(&retrieved).as_array().unwrap().len(), 1, "{retrieved}");
    assert_eq!(echoed(&retrieved), &echo);

    let recalled = call(&server, "memory.recall", json!({ "table": "research/earnings", "subject": "acme", "available_as_of": when }));
    assert!(recalled.get("error").is_some() || recalled["result"]["isError"] == json!(true), "{recalled}");
}

/// A read tool resolves a vintage table to its latest release, or to its first under `vintage = "first"`.
#[test]
fn every_read_tool_resolves_the_vintage_it_is_asked_for() {
    let f = available();
    let clock = FixedClock(at("2030-09-02T00:00:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let sql = r#"SELECT CAST(eps AS VARCHAR) FROM "research/earnings" WHERE filing_id = 'acme-q1'"#;
    assert_eq!(rows(&call(&server, "context.query", json!({ "sql": sql }))), &json!([["1.19"]]));
    assert_eq!(rows(&call(&server, "context.query", json!({ "sql": sql, "available_as_of": "2030-08-01" }))), &json!([["1.23"]]));
    assert_eq!(rows(&call(&server, "context.query", json!({ "sql": sql, "vintage": "first" }))), &json!([["1.23"]]));
    assert_eq!(rows(&call(&server, "context.query", json!({ "sql": sql, "vintage": "latest" }))), &json!([["1.19"]]));
    let template = call(&server, "earnings", json!({ "ticker": "ACME", "vintage": "first" }));
    assert_eq!(rows(&template), &json!([["acme-q1"], ["acme-q2"]]));
    let refused = call(&server, "context.query", json!({ "sql": sql, "vintage": "second" }));
    assert!(refused.get("error").is_some() || refused["result"]["isError"] == json!(true), "{refused}");
}

/// A listing {{read.register.bound-listing}} names ignores `available_as_of` and echoes no `available_as_of` part.
// spec: read.register.available-bound-listing@5895d568
#[test]
fn a_listing_ignores_available_as_of() {
    let f = available();
    let clock = FixedClock(at("2030-09-02T00:00:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let when = "2030-06-01T00:00:00Z";

    let unbounded = call(&server, "context.files", json!({}));
    let available_only = call(&server, "context.files", json!({ "available_as_of": when }));
    assert_eq!(rows(&available_only), rows(&unbounded));
    assert!(available_only["result"]["structuredContent"].get("contextful.bounds").is_none(), "{available_only}");
    let both = call(&server, "context.files", json!({ "as_of": "2030-09-02T00:00:00Z", "available_as_of": when }));
    assert_eq!(echoed(&both), &json!({ "as_of": "2030-09-02T00:00:00.000000000Z", "inclusive": true }));

    let tables = call(&server, "context.describe", json!({ "available_as_of": when }));
    assert!(tables["result"]["structuredContent"].get("contextful.bounds").is_none(), "{tables}");
    assert_eq!(tables["result"]["structuredContent"]["tables"].as_array().unwrap().len(), 2, "{tables}");
}

/// Under a credential carrying a horizon, every read tool but `context.subscribe`, which {{read.subscribe.horizon-refusal}} refuses, reads at `available_as_of` equal to the horizon when the call supplies none, capping {{read.as-of-join.horizon-caps-row}}, and echoes it as {{store.available-time.bound-echo}}.
// spec: read.horizon.inherited-bound@5177ce81
#[test]
fn every_read_tool_called_unbounded_reads_at_the_horizon() {
    let f = available_under(Some("2030-06-01T00:00:00Z"));
    let clock = FixedClock(at("2030-09-02T00:00:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let echo = json!({ "available_as_of": "2030-06-01T00:00:00.000000000Z", "inclusive": true });

    // The second quarter, accepted on 1 August, lies past the horizon on every tool.
    let query = call(&server, "context.query", json!({ "sql": r#"SELECT filing_id FROM "research/earnings" ORDER BY filing_id"# }));
    assert_eq!(rows(&query), &json!([["acme-q1"]]));
    assert_eq!(echoed(&query), &echo);
    let executed = call(&server, "context.execute_query", json!({ "id": "earnings", "arguments": { "ticker": "ACME" } }));
    assert_eq!(rows(&executed), &json!([["acme-q1"]]));
    assert_eq!(echoed(&executed), &echo);
    let template = call(&server, "earnings", json!({ "ticker": "ACME" }));
    assert_eq!(rows(&template), &json!([["acme-q1"]]));
    let described = call(&server, "context.describe", json!({ "table": "research/earnings" }));
    assert_eq!(described["result"]["structuredContent"]["row_count"], json!("1"), "{described}");
    assert_eq!(echoed(&described), &echo);
    let listed = call(&server, "context.files", json!({}));
    let part = rows(&listed).as_array().unwrap().iter().find(|r| r[0] == json!("research/earnings")).unwrap()[1].clone();
    let preview = call(&server, "context.file", json!({ "path": part }));
    assert_eq!(rows(&preview).as_array().unwrap().len(), 1, "{preview}");
    let retrieved = call(&server, "corpus.retrieve", json!({ "prefix": "research/earnings", "query": "acme" }));
    assert_eq!(rows(&retrieved).as_array().unwrap().len(), 1, "{retrieved}");
    assert_eq!(echoed(&retrieved), &echo);
    // A subscription reads no rows at a bound; it refuses under the horizon.
    let subscribed = call(&server, "context.subscribe", json!({ "tables": ["research/earnings"] }));
    assert_eq!(subscribed["result"]["structuredContent"]["error"]["identifier"], json!("SubscriptionUnderHorizon"), "{subscribed}");

    // An earlier bound narrows further; a table declaring no availability time refuses.
    let april = call(&server, "context.query", json!({ "sql": r#"SELECT filing_id FROM "research/earnings""#, "available_as_of": "2030-04-30T00:00:00Z" }));
    assert_eq!(rows(&april), &json!([]));
    let undeclared = call(&server, "context.query", json!({ "sql": r#"SELECT v FROM "research/filings""# }));
    assert!(undeclared["result"]["content"][0]["text"].as_str().unwrap().contains("StoreAvailableTimeUndeclared"), "{undeclared}");
}

#[test]
fn a_call_reaching_past_the_horizon_is_refused_naming_its_argument() {
    let f = available_under(Some("2030-06-01T00:00:00Z"));
    let clock = FixedClock(at("2030-09-02T00:00:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let identifier = |answer: &Value| answer["result"]["structuredContent"]["error"]["identifier"].clone();

    let later = call(&server, "context.query", json!({ "sql": r#"SELECT filing_id FROM "research/earnings""#, "available_as_of": "2030-09-01T00:00:00Z" }));
    assert_eq!(identifier(&later), json!("ReadBoundExceedsHorizon"), "{later}");
    assert!(later["result"]["content"][0]["text"].as_str().unwrap().contains("available_as_of"), "{later}");
    let since = call(&server, "corpus.retrieve", json!({ "prefix": "research/earnings", "query": "acme", "since": "2030-07-01T00:00:00Z" }));
    assert_eq!(identifier(&since), json!("ReadBoundExceedsHorizon"), "{since}");
    assert!(since["result"]["content"][0]["text"].as_str().unwrap().contains("since"), "{since}");

    // `as_of` passes beyond the horizon; the availability bound still holds the row back.
    let as_of = call(&server, "context.query", json!({ "sql": r#"SELECT filing_id FROM "research/earnings" ORDER BY filing_id"#, "as_of": "2031-01-01T00:00:00Z" }));
    assert_eq!(rows(&as_of), &json!([["acme-q1"]]));
}

/// Under a horizon, {{read.recall.keyed}} defaults an absent `observed_at` to the horizon and reads a claims table at `available_as_of` equal to it, so a table declaring no availability time refuses as {{store.available-time.undeclared}}.
// spec: read.horizon.recall-horizon@d4fc8ee7
#[test]
fn recall_under_a_horizon_reads_the_claims_table_at_it() {
    let f = fixture_bounded(CLAIMS, Some("2030-02-01T00:00:00Z"), seed_claims);
    let clock = FixedClock(at("2030-04-01T00:00:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let refused = call(&server, "memory.recall", json!({ "table": "research/facts", "subject": "acme" }));
    assert_eq!(refused["result"]["isError"], json!(true), "{refused}");
    let text = refused["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("StoreAvailableTimeUndeclared") && text.contains("research/facts"), "{refused}");

    // Unbounded by a horizon, the same recall answers at the call's instant.
    let f = claims();
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let answered = call(&server, "memory.recall", json!({ "table": "research/facts", "subject": "acme" }));
    assert_eq!(rows(&answered).as_array().unwrap().len(), 1, "{answered}");
}

const AS_OF_JOIN: &str = r#"[[pipeline.tables]]
name = "research/master"

[pipeline.tables.available_time]
column = "available_at"

[[pipeline.tables]]
name = "research/trades"

[pipeline.tables.available_time]
column = "booked_at"

[[pipeline.tables]]
name = "research/tickers"
"#;

/// A point-in-time security master renaming `sec-1` from `FB` to `META`, two trades of
/// it, and a ticker table declaring no availability time.
fn as_of_join() -> Fixture {
    let decls = TableDecl::parse_pipeline(AS_OF_JOIN).unwrap();
    let decl = |name: &str| decls.iter().find(|d| d.name == name).unwrap().clone();
    fixture_over(AS_OF_JOIN, |store| {
        let master = json!([
            { "sec_id": "sec-1", "ticker": "FB", "available_at": "2012-05-18T00:00:00Z" },
            { "sec_id": "sec-1", "ticker": "META", "available_at": "2022-06-09T00:00:00Z" },
        ]);
        put(store, &decl("research/master"), "run-1", "2030-01-01T00:00:00Z", master, &[("available_at", ColumnType::Timestamp)]);
        let trades = json!([
            { "trade_id": "t1", "sec_id": "sec-1", "traded_at": "2021-03-01T00:00:00Z", "booked_at": "2021-03-01T00:00:00Z" },
            { "trade_id": "t2", "sec_id": "sec-1", "traded_at": "2022-07-01T00:00:00Z", "booked_at": "2021-12-01T00:00:00Z" },
        ]);
        let ts = [("traded_at", ColumnType::Timestamp), ("booked_at", ColumnType::Timestamp)];
        put(store, &decl("research/trades"), "run-1", "2030-01-01T00:00:00Z", trades, &ts);
        put(store, &decl("research/tickers"), "run-1", "2030-01-01T00:00:00Z", json!([{ "sec_id": "sec-1", "ticker": "META" }]), &[]);
    })
}

const JOINED: &str = r#"SELECT t.trade_id, m.ticker FROM "research/trades" t ASOF JOIN "research/master" m ON t.sec_id = m.sec_id AND t.traded_at >= m.available_at ORDER BY t.trade_id"#;

/// An `ASOF JOIN` matches each driving row, per equality key, to the right relation's row whose declared availability instant is the latest at or before the driving instant its inequality names, its valid-time `from` within `max_age` when given.
// spec: read.as-of-join.per-row-bound@11522cb8
#[test]
fn an_as_of_join_matches_each_row_to_the_right_row_available_at_its_instant() {
    let f = as_of_join();
    let clock = FixedClock(at("2030-01-02T00:00:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let joined = call(&server, "context.query", json!({ "sql": JOINED }));
    assert_eq!(rows(&joined), &json!([["t1", "FB"], ["t2", "META"]]));
}

/// Under `available_as_of`, each driving row's instant is the earlier of the instant its inequality names and `available_as_of`, so no right row past the bound joins.
// spec: read.as-of-join.horizon-caps-row@1a2b0e44
#[test]
fn an_availability_bound_caps_each_driving_rows_instant() {
    let f = as_of_join();
    let clock = FixedClock(at("2030-01-02T00:00:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    // Trade t2, booked in December 2021 and dated July 2022, joins the name public on 1 January 2022.
    let bounded = call(&server, "context.query", json!({ "sql": JOINED, "available_as_of": "2022-01-01T00:00:00Z" }));
    assert_eq!(rows(&bounded), &json!([["t1", "FB"], ["t2", "FB"]]));
}

/// An `ASOF JOIN` whose right relation declares no `available_time` refuses as {{store.available-time.undeclared}} does, naming the table, whether or not the read carries `available_as_of`.
// spec: read.as-of-join.join-undeclared@ad07026c
#[test]
fn an_as_of_join_onto_an_undeclaring_relation_is_refused() {
    let f = as_of_join();
    let clock = FixedClock(at("2030-01-02T00:00:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    let sql = r#"SELECT t.trade_id, k.ticker FROM "research/trades" t ASOF JOIN "research/tickers" k ON t.sec_id = k.sec_id AND t.traded_at >= t.booked_at"#;
    let refused = call(&server, "context.query", json!({ "sql": sql }));
    assert_eq!(refused["result"]["isError"], json!(true), "{refused}");
    let text = refused["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("StoreAvailableTimeUndeclared") && text.contains("research/tickers"), "{refused}");
    // An ordinary join onto the same relation reads.
    let plain = call(&server, "context.query", json!({ "sql": r#"SELECT k.ticker FROM "research/trades" t JOIN "research/tickers" k ON t.sec_id = k.sec_id LIMIT 1"# }));
    assert_eq!(rows(&plain), &json!([["META"]]));
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
            (vec!["observed_at", "as_of_ingest"], vec!["as_of", "valid_as_of", "available_as_of", "vintage"])
        } else if tool["name"] == json!("context.subscribe") {
            (vec![], vec!["as_of", "valid_as_of", "available_as_of", "vintage", "observed_at", "as_of_ingest"])
        } else {
            (vec!["as_of", "valid_as_of", "available_as_of", "vintage"], vec!["observed_at", "as_of_ingest"])
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
    fixture_over(CLAIMS, seed_claims)
}

fn seed_claims(store: &Store) {
    {
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
    }
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

/// `corpus.retrieve({prefix, query, query_embedding?, filter?, kinds?, limit?, since?, min_score?, as_of?, valid_as_of?})` returns the top rows across the item and artifact genres under one prefix, each with a snippet and full provenance. It composes the query surface and memory recall, storing nothing.
// spec: read.retrieve.ranked-call@13d751b7
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
    build(&face, &BuildRequest { model: &spec, site_id: "site-a", started_at: at(now), completed_at: at(now), available_as_of: None }).unwrap().build_id
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
