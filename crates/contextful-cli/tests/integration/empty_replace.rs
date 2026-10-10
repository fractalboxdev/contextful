//! Complete empty snapshots replace rows; unchanged and failed pulls keep the frontier.
#![cfg(feature = "data-plane")]

use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const AUD: &str = "contextful://empty-replace";

fn cf(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful")).args(args).current_dir(dir).env_remove("CONTEXTFUL_NODE_ID").output().unwrap()
}

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn rows(dir: &Path) -> Value {
    serde_json::from_str(&ok(&cf(dir, &["query", "--json", "--project", "research", "SELECT sku FROM inventory ORDER BY sku"]))).unwrap()
}

fn earlier_rows(dir: &Path) -> Value {
    let scan: Value = serde_json::from_str(&ok(&cf(dir, &["context", "scan", "inventory", "--project", "research", "--as-of", "2030-01-01T00:02:30Z"]))).unwrap();
    let sql = format!("SELECT sku FROM ({}) AS bounded_inventory", scan["relation"].as_str().unwrap());
    serde_json::from_str(&ok(&cf(dir, &["query", "--json", "--project", "research", &sql]))).unwrap()
}

fn mcp_rows(dir: &Path) -> Value {
    let public = ok(&cf(dir, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let token = ok(&cf(dir, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@example.test", "--zone", "on-prem:hq", "--table", "inventory", "--ttl", "600"]));
    let mut child = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["mcp", "--project", "research", "--public-key", &public, "--audience", AUD])
        .current_dir(dir)
        .env("CONTEXTFUL_TOKEN", token)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let call = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "context.query", "arguments": { "sql": "SELECT sku FROM inventory ORDER BY sku" } } });
    writeln!(child.stdin.take().unwrap(), "{call}").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_ne!(answer["result"]["isError"], json!(true), "{answer}");
    answer["result"]["structuredContent"]["rows"].clone()
}

fn start(dir: &Path, run: &str, now: &str) -> Output {
    cf(dir, &["run", "start", "--plan", "feed.toml", "--project", "research", "--run-id", run, "--site-id", "site-a", "--now", now])
}

// spec: store.declare.empty-frontier-preserved@4e5e03e7
// spec: run.advance.snapshot-completion@9ce96da7
// spec: run.advance.zero-row-commit@7f0ed685
#[test]
fn a_complete_empty_snapshot_replaces_but_a_skip_and_failed_pull_do_not() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join(".contextful/context/research")).unwrap();
    std::fs::write(root.join(".contextful/issuance.toml"), format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n")).unwrap();
    let config = |node: &str| format!("[node]\nid = \"{node}\"\n\n[sync]\nendpoint = \"file://{}\"\nbucket = \"team\"\nprefix = \"research\"\ncoordination = \"cas\"\n", bucket.path().display());
    std::fs::write(root.join(".contextful/context/research/config.toml"), config("ingest-a")).unwrap();
    std::fs::write(root.join("contextful.toml"), "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"inventory\"\nwrite_mode = \"replace\"\n").unwrap();
    std::fs::write(root.join("feed.toml"), "pipeline = \"feed\"\ntable = \"inventory\"\n[cursor]\nkind = \"snapshot-id\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\ncommand = [\"sh\", \"source.sh\"]\n").unwrap();
    std::fs::write(root.join("source.sh"), "cat payload.json\n").unwrap();
    let input = |body: Value| std::fs::write(root.join("payload.json"), body.to_string()).unwrap();

    input(json!({ "rows": [{ "sku": "a" }], "cursor": "v1", "more": false }));
    ok(&start(root, "filled", "2030-01-01T00:00:00Z"));
    assert_eq!(rows(root)["rows"], json!([["a"]]));

    // A partial inventory cannot replace the complete inventory already visible.
    input(json!({ "rows": [{ "sku": "b" }], "cursor": "v2", "more": false, "snapshot_complete": false }));
    ok(&start(root, "incomplete", "2030-01-01T00:00:30Z"));
    assert_eq!(rows(root)["rows"], json!([["a"]]));
    assert!(!root.join(".contextful/context/research/tables/inventory/data/runs/incomplete/ingest-a/_manifest.json").exists());

    input(json!({ "rows": [{ "sku": "c" }], "cursor": "v2", "more": false, "skipped": 1, "snapshot_complete": true }));
    ok(&start(root, "partially-skipped", "2030-01-01T00:00:40Z"));
    assert_eq!(rows(root)["rows"], json!([["a"]]));
    assert!(!root.join(".contextful/context/research/tables/inventory/data/runs/partially-skipped/ingest-a/_manifest.json").exists());

    // A skipped input retains rows even if its source incorrectly claims completion.
    input(json!({ "rows": [], "cursor": "v1", "more": false, "skipped": 1, "snapshot_complete": true }));
    ok(&start(root, "skipped", "2030-01-01T00:01:00Z"));
    assert_eq!(rows(root)["rows"], json!([["a"]]));
    assert!(!root.join(".contextful/context/research/tables/inventory/data/runs/skipped").exists());

    std::fs::write(root.join("source.sh"), "exit 2\n").unwrap();
    assert!(!start(root, "failed", "2030-01-01T00:02:00Z").status.success());
    assert_eq!(rows(root)["rows"], json!([["a"]]));
    assert!(!root.join(".contextful/context/research/tables/inventory/data/runs/failed").exists());

    std::fs::write(root.join("source.sh"), "cat payload.json\n").unwrap();
    input(json!({ "rows": [], "cursor": "v2", "more": false, "snapshot_complete": true }));
    ok(&start(root, "empty", "2030-01-01T00:03:00Z"));
    assert_eq!(rows(root)["rows"], json!([]));
    assert_eq!(mcp_rows(root), json!([]));
    let marker: Value = serde_json::from_slice(&std::fs::read(root.join(".contextful/context/research/tables/inventory/data/runs/empty/ingest-a/_manifest.json")).unwrap()).unwrap();
    assert_eq!(marker["replace_frontier"], json!(true), "{marker}");
    assert_eq!(marker["parts"], json!([]));
    assert_eq!(marker["cursor"], json!("v2"));

    let earlier = ok(&cf(root, &["context", "files", "inventory", "--project", "research", "--as-of", "2030-01-01T00:02:30Z"]));
    assert!(earlier.contains("/runs/filled/"), "{earlier}");
    assert_eq!(earlier_rows(root)["rows"], json!([["a"]]));

    ok(&cf(root, &["sync", "push", "--project", "research"]));
    let replica = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(replica.path().join(".contextful/context/research")).unwrap();
    std::fs::copy(root.join("contextful.toml"), replica.path().join("contextful.toml")).unwrap();
    std::fs::copy(root.join(".contextful/issuance.toml"), replica.path().join(".contextful/issuance.toml")).unwrap();
    std::fs::write(replica.path().join(".contextful/context/research/config.toml"), config("ingest-b")).unwrap();
    ok(&cf(replica.path(), &["sync", "pull", "--project", "research"]));
    assert_eq!(rows(replica.path())["rows"], json!([]));
    assert_eq!(mcp_rows(replica.path()), json!([]));
    assert_eq!(earlier_rows(replica.path())["rows"], json!([["a"]]));

    ok(&cf(root, &["context", "compact", "inventory", "--project", "research", "--now", "2030-01-01T00:04:00Z"]));
    assert_eq!(rows(root)["rows"], json!([]));
    assert_eq!(earlier_rows(root)["rows"], json!([["a"]]));
    ok(&cf(root, &["sync", "push", "--project", "research"]));
    ok(&cf(replica.path(), &["sync", "pull", "--project", "research"]));
    assert_eq!(rows(replica.path())["rows"], json!([]));
    assert_eq!(earlier_rows(replica.path())["rows"], json!([["a"]]));
}
