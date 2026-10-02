//! `contextful query` through the built binary: operator text, run raw, printed as the
//! one response projection.

use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const AUD: &str = "contextful://acme-research";

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful")).args(args).current_dir(dir).env_remove("CONTEXTFUL_NODE_ID").output().unwrap()
}

fn stdout(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn query(dir: &Path, args: &[&str]) -> Value {
    let mut all = vec!["query", "--json"];
    all.extend_from_slice(args);
    serde_json::from_str(&stdout(&run(dir, &all))).unwrap()
}

/// A project holding one landed table.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join(".contextful")).unwrap();
    std::fs::write(p.join(".contextful/issuance.toml"), format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n")).unwrap();
    std::fs::write(p.join("contextful.toml"), "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"research/notes\"\n\n[[pipeline.tables]]\nname = \"research/quiet\"\n").unwrap();
    std::fs::write(p.join("notes.jsonl"), "{\"note_id\":\"n1\",\"title\":\"Solar battery storage\"}\n{\"note_id\":\"n2\",\"title\":\"Grid inertia\"}\n")
        .unwrap();
    stdout(&run(p, &["context", "land", "research/notes", "--project", "research", "--rows", "notes.jsonl", "--run-id", "run-0001", "--site-id", "site-a"]));
    dir
}

/// Run the tool server with a credential reading `research/*`, and return the answer to
/// one `context.query` call.
fn mcp_query(dir: &Path, sql: &str) -> Value {
    let public = stdout(&run(dir, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let token = stdout(&run(
        dir,
        &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--table", "research/*", "--ttl", "600"],
    ));
    let mut child = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["mcp", "--project", "research", "--public-key", &public, "--audience", AUD])
        .current_dir(dir)
        .env("CONTEXTFUL_TOKEN", token)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        let call = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "context.query", "arguments": { "sql": sql } } });
        writeln!(stdin, "{call}").unwrap();
    }
    let out = child.wait_with_output().unwrap();
    serde_json::from_str(stdout(&out).lines().next().unwrap()).unwrap()
}

fn keys(v: &Value) -> Vec<&str> {
    v.as_object().unwrap().keys().map(String::as_str).collect()
}

/// `contextful query --json <sql>` prints the one projection for a statement over no store.
// spec: read.query.operator-verb@70205979
#[test]
fn a_statement_prints_the_one_projection() {
    let dir = tempfile::tempdir().unwrap();
    let out = query(dir.path(), &["SELECT 1 AS one, 'a' AS letter"]);
    assert_eq!(out, json!({ "columns": ["one", "letter"], "rows": [[1, "a"]], "truncated": false }));
}

/// The command line and the tool protocol serialize one projection for the same statement.
// spec: read.respond.one-projection@2ab01c50
#[test]
fn the_command_line_and_the_tool_protocol_print_one_projection() {
    let p = project();
    let sql = "SELECT note_id, title FROM \"research/notes\" ORDER BY note_id";
    let cli = query(p.path(), &["--project", "research", sql]);
    let answer = mcp_query(p.path(), sql);
    let tool = &answer["result"]["structuredContent"];
    assert_eq!(keys(&cli), vec!["columns", "rows", "truncated"]);
    assert_eq!(&cli, tool, "{answer}");
    assert_eq!(cli["rows"], json!([["n1", "Solar battery storage"], ["n2", "Grid inertia"]]));
}

/// Operator text runs raw: a table function the guard refuses to a token caller runs here.
// spec: read.guard.statement-provenance@a99896a0
#[test]
fn operator_text_runs_raw_where_token_text_is_gated() {
    let p = project();
    let json_dir = p.path().join("objects");
    std::fs::create_dir_all(&json_dir).unwrap();
    std::fs::write(json_dir.join("a.json"), "{\"k\":1}\n{\"k\":2}\n").unwrap();
    std::fs::write(json_dir.join("b.json"), "{\"k\":3}\n").unwrap();
    let sql = format!("SELECT count(*) AS n FROM read_json_objects('{}/*.json')", json_dir.display());

    let cli = query(p.path(), &[&sql]);
    assert_eq!(cli, json!({ "columns": ["n"], "rows": [["3"]], "truncated": false }));

    let answer = mcp_query(p.path(), &sql);
    assert_eq!(answer["result"]["isError"], json!(true), "{answer}");
    assert!(answer["result"]["content"][0]["text"].as_str().unwrap().contains("TableFunctionRefused"), "{answer}");
}

/// `--project` registers every table under its bare name; a quiet table reads as zero rows.
// spec: read.query.project-relations@239bf3fa
#[test]
fn a_project_registers_every_table_under_its_bare_name() {
    let p = project();
    let out = query(p.path(), &["--project", "research", "SELECT count(*) AS n FROM \"research/notes\""]);
    assert_eq!(out["rows"], json!([["2"]]));
    let quiet = query(p.path(), &["--project", "research", "SELECT * FROM \"research/quiet\""]);
    assert_eq!(quiet["rows"], json!([]));
    assert_eq!(quiet["truncated"], json!(false));
}

/// `--limit` bounds delivered rows and sets `truncated` from the over-fetched probe row.
// spec: read.query.limit-truncates@2f389281
#[test]
fn a_limit_truncates_exactly() {
    let dir = tempfile::tempdir().unwrap();
    // A 64-bit integer column encodes as exact decimal strings (`read.respond.wide-number-shape`).
    let cut = query(dir.path(), &["--limit", "2", "SELECT range AS n FROM range(5)"]);
    assert_eq!(cut, json!({ "columns": ["n"], "rows": [["0"], ["1"]], "truncated": true }));
    let exact = query(dir.path(), &["--limit", "5", "SELECT range AS n FROM range(5)"]);
    assert_eq!(exact["rows"].as_array().unwrap().len(), 5);
    assert_eq!(exact["truncated"], json!(false));
    let all = query(dir.path(), &["SELECT range AS n FROM range(5)"]);
    assert_eq!(all["rows"].as_array().unwrap().len(), 5);
    assert_eq!(all["truncated"], json!(false));
}

/// A statement the engine rejects exits non-zero, its message on standard error, nothing on standard output.
// spec: read.query.engine-fault@66605c15
#[test]
fn a_rejected_statement_prints_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(dir.path(), &["query", "--json", "SELEC 1"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("syntax"), "{}", String::from_utf8_lossy(&out.stderr));
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// Text holding other than one statement is refused before any statement runs.
// spec: read.query.one-statement@ecc01844
#[test]
fn text_holding_two_statements_runs_none() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(dir.path(), &["query", "--json", "SELECT 1 AS a; SELECT 2 AS b"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(stderr(&out).contains("QueryNotOneStatement"), "{}", stderr(&out));

    let copied = dir.path().join("copied.csv");
    let sql = format!("COPY (SELECT 1 AS a) TO '{}'; SELECT 2 AS b", copied.display());
    let out = run(dir.path(), &["query", "--json", &sql]);
    assert!(!out.status.success());
    assert!(!copied.exists(), "the first statement ran");

    let empty = run(dir.path(), &["query", "--json", " ; "]);
    assert!(!empty.status.success());
    assert!(stderr(&empty).contains("QueryNotOneStatement"), "{}", stderr(&empty));

    let trailing = query(dir.path(), &["SELECT 1 AS a;"]);
    assert_eq!(trailing["rows"], json!([[1]]));
}

/// A `--project` naming no store on disk is refused rather than answered with quiet tables;
/// a discovered project without one runs over no store (`read.query.discovered-project`).
// spec: read.query.project-store@bb44fa8e
#[test]
fn a_project_with_no_store_is_refused() {
    let p = project();
    let out = run(p.path(), &["query", "--json", "--project", "reserch", "SELECT count(*) AS n FROM \"research/notes\""]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(stderr(&out).contains("QueryProjectAbsent"), "{}", stderr(&out));
    assert!(!p.path().join(".contextful/context/reserch").exists());
}

/// `--declaration` names the manifest whose declared tables register beside the store's.
// spec: read.query.declaration-default@64338b30
#[test]
fn a_declaration_path_supplies_the_manifest() {
    let p = project();
    std::fs::write(p.path().join("other.toml"), "[[pipeline.tables]]\nname = \"research/elsewhere\"\n").unwrap();
    let sql = "SELECT * FROM \"research/elsewhere\"";
    let out = query(p.path(), &["--project", "research", "--declaration", "other.toml", sql]);
    assert_eq!(out["rows"], json!([]));
    let absent = run(p.path(), &["query", "--json", "--project", "research", sql]);
    assert!(!absent.status.success());

    // With no project, `--declaration` refuses as discovery does.
    let elsewhere = tempfile::tempdir().unwrap();
    let refused = run(elsewhere.path(), &["query", "--json", "--declaration", "other.toml", "SELECT 1"]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("StoreProjectUndiscovered"), "{}", stderr(&refused));
}

/// Without `--project`, the project discovery finds registers its tables from any directory
/// beneath its `contextful.toml`; with none found, or its store absent, the statement runs
/// over no store.
// spec: read.query.discovered-project@5e7424e2
#[test]
fn a_discovered_project_registers_its_tables() {
    let p = project();
    let notes = "SELECT count(*) AS n FROM \"research/notes\"";

    // A nearest `contextful.toml` naming no project registers nothing.
    let nameless = run(p.path(), &["query", "--json", notes]);
    assert!(!nameless.status.success());
    assert!(stderr(&nameless).contains("Catalog Error"), "{}", stderr(&nameless));

    let declaration = p.path().join("contextful.toml");
    let text = std::fs::read_to_string(&declaration).unwrap();
    std::fs::write(&declaration, format!("{text}\n[project]\nname = \"research\"\n")).unwrap();
    let below = p.path().join("drafts/june");
    std::fs::create_dir_all(&below).unwrap();

    for dir in [p.path(), below.as_path()] {
        assert_eq!(query(dir, &[notes])["rows"], json!([["2"]]));
        // The discovered `contextful.toml` is the manifest: a declared quiet table registers.
        assert_eq!(query(dir, &["SELECT * FROM \"research/quiet\""])["rows"], json!([]));
    }

    // `--declaration` names the discovered project's manifest.
    std::fs::write(below.join("other.toml"), "[[pipeline.tables]]\nname = \"research/elsewhere\"\n").unwrap();
    assert_eq!(query(&below, &["--declaration", "other.toml", "SELECT * FROM \"research/elsewhere\""])["rows"], json!([]));

    // A literal statement runs where no `contextful.toml` is found.
    let elsewhere = tempfile::tempdir().unwrap();
    assert_eq!(query(elsewhere.path(), &["SELECT 1 AS one"])["rows"], json!([[1]]));

    // A discovered project whose store is absent registers nothing: literal and file
    // statements run over no store, and a table reads as unknown rather than as quiet.
    let clone = tempfile::tempdir().unwrap();
    std::fs::write(clone.path().join("contextful.toml"), "[project]\nname = \"absent\"\n\n[[pipeline.tables]]\nname = \"absent/notes\"\n").unwrap();
    std::fs::write(clone.path().join("d.csv"), "a,b\n1,2\n").unwrap();
    let below = clone.path().join("sub");
    std::fs::create_dir_all(&below).unwrap();

    assert_eq!(query(&below, &["SELECT 1 AS one"])["rows"], json!([[1]]));
    let csv = format!("SELECT a FROM '{}'", clone.path().join("d.csv").display());
    assert_eq!(query(&below, &[&csv])["rows"].as_array().unwrap().len(), 1);

    let table = run(&below, &["query", "--json", "SELECT * FROM \"absent/notes\""]);
    assert!(!table.status.success());
    assert!(stderr(&table).contains("Catalog Error"), "{}", stderr(&table));
    assert!(!stderr(&table).contains("QueryProjectAbsent"), "{}", stderr(&table));
    assert!(!clone.path().join(".contextful").exists());

    // A malformed `contextful.toml` refuses as discovery does.
    std::fs::write(clone.path().join("contextful.toml"), "[project]\nname = [\n").unwrap();
    assert!(!run(&below, &["query", "--json", "SELECT 1"]).status.success());
}
