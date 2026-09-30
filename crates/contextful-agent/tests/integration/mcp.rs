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

struct Fixture {
    _dir: tempfile::TempDir,
    face: Face,
    authority: AdmittedAuthority,
}

/// Land one batch of `rows` into `decl`'s table as run `run`, committed at `now`.
fn put(store: &Store, decl: &TableDecl, run: &str, now: &str, rows: Value, types: &[(&str, ColumnType)]) {
    let rows = rows.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect();
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: run.into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint: None },
        committed_at: at(now),
    };
    let types = types.iter().map(|(c, t)| (c.to_string(), *t)).collect();
    land(store, decl, &Batch { rows, types }, &ctx).unwrap();
}

fn fixture() -> Fixture {
    fixture_over(MANIFEST, |store| {
        let notes = json!([{ "note_id": "n1", "title": "Solar battery storage" }, { "note_id": "n2", "title": "Hiring plan" }]);
        put(store, &TableDecl::named("research/notes"), "run-0001", "2030-01-01T00:00:00Z", notes, &[]);
        let salaries = json!([{ "employee": "e1", "title": "Battery storage engineer" }]);
        put(store, &TableDecl::named("hr/salaries"), "run-0001", "2030-01-01T00:00:00Z", salaries, &[]);
    })
}

fn fixture_over(manifest: &str, seed: impl FnOnce(&Store)) -> Fixture {
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
    };
    let mut req = MintRequest::custody(subject, vec![grant]);
    req.lifetime = Lifetime::Requested(900);
    let clock = FixedClock(at("2030-01-01T00:00:00Z"));
    let plan = policy.check(&req, &MintContext { node: NodeRole::Primary, signer: &signer, clock: &clock }).unwrap();
    let token = mint(&plan, &MintClaims::default(), &signer).unwrap();
    let keys = StaticPins::parse(&signer.public_key_text()).unwrap().keys().unwrap();
    let revocation = RevocationState::default();
    let authority = verify_inherited_pipe(&token, &keys, &Admission::new(at("2030-01-01T00:05:00Z"), &revocation).expecting(AUD)).unwrap();
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

/// The face exposes a closed tool set: `context.describe`, `context.query`, `context.execute_query` for templates, `context.files` and `context.file` over committed data files, `corpus.retrieve` for ranked reads across a prefix, and `memory.recall` for keyed claim reads.
// spec: read.register.tool-set@8a963be6
#[test]
fn the_tool_list_is_the_closed_read_set() {
    let f = fixture();
    let clock = FixedClock(at("2030-01-01T00:06:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock).unwrap();
    let tools = ask(&server, 1, "tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["context.describe", "context.query", "context.execute_query", "context.files", "context.file", "corpus.retrieve", "memory.recall"]);
    let unknown = call(&server, "memory.write", json!({}));
    assert_eq!(unknown["error"]["code"], json!(-32602), "{unknown}");
}

/// On the tool protocol a refusal arrives in-band, as a protocol error object or a result flagged as an error under transport success.
// spec: read.respond.in-band-error@57469a30
#[test]
fn a_refusal_arrives_in_band() {
    let f = fixture();
    let clock = FixedClock(at("2030-01-01T00:06:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock).unwrap();
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
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock).unwrap();
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

#[test]
fn the_handshake_reports_the_build_and_refuses_an_absent_face() {
    let f = fixture();
    let clock = FixedClock(at("2030-01-01T00:06:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock).unwrap();
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
    let clock = FixedClock(at("2030-01-01T00:06:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &boundary, &clock).unwrap();
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
fn bounded() -> Fixture {
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
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock).unwrap();
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
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock).unwrap();
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
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock).unwrap();
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
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock).unwrap();
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

/// `memory.recall` answers over the tool protocol with the subject's claims at `observed_at`,
/// takes `as_of_ingest` for transaction time, and takes no `as_of` or `valid_as_of`.
#[test]
fn memory_recall_answers_keyed_over_the_tool_protocol() {
    let f = claims();
    let clock = FixedClock(at("2030-06-01T00:00:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock).unwrap();
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
