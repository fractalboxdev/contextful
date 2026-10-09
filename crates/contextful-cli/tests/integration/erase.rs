//! Erasure through the built binary, with fixture-owned keys and disposable rows.
#![cfg(feature = "data-plane")]
use std::process::{Command, Output};

#[test]
fn erasure_requires_forget_over_declared_memory_before_mutation() {
    let directory = tempfile::tempdir().unwrap(); let root = directory.path();
    ok(run(root, &["init", "research", "--authoring-posture", "per_request"], None, None));
    std::fs::write(root.join("contextful.toml"), r#"authoring_posture = "per_request"
[[pipeline.tables]]
name = "notes"
primary_key = ["id"]
columns = {subject="utf8"}
[[table]]
name = "memory/facts"
shape = "memory_facts"
columns = ["claim_id","subject","predicate","object","scope","tier","confidence","valid_from","valid_to","evidence","superseded_by","grant_id","agent"]
"#).unwrap();
    std::fs::write(root.join(".contextful/issuance.toml"), "default_audience = \"erasure-fixture\"\nmax_lifetime_secs = 3600\n").unwrap();
    std::fs::write(root.join("notes.jsonl"), "{\"id\":\"erased\",\"subject\":\"memory-forget-canary\"}\n").unwrap();
    ok(run(root, &["context", "land", "notes", "--project", "research", "--rows", "notes.jsonl", "--run-id", "r1", "--site-id", "fixture"], None, None));
    let pins = ok(run(root, &["token", "keygen", "--out", ".contextful/issuer.seed"], None, None));
    let limited = ok(run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://fixture", "--agent", "agent://fixture", "--zone", "on-prem:fixture", "--action", "forget", "--table", "notes", "--ttl", "3600"], None, None));
    std::fs::write(root.join("keys.json"), serde_json::json!({"subject_hash":format!("hmac-sha256:{}", "a".repeat(64)),"column":"subject","keys":["memory-forget-canary"]}).to_string()).unwrap();
    let store = root.join(".contextful/context/research");
    fn files(directory: &std::path::Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
        let mut out = std::collections::BTreeMap::new();
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() { out.extend(files(&path)); }
            else { out.insert(path.clone(), std::fs::read(path).unwrap()); }
        }
        out
    }
    let before = files(&store);
    let request = ["context", "erase", "--project", "research", "--key-set", "keys.json", "--issuer-key", ".contextful/issuer.seed", "--public-key", &pins, "--audience", "erasure-fixture", "--json"];
    let denied = run(root, &request, Some(&limited), Some(&pins));
    assert!(!denied.status.success(), "a notes-only grant admits a canonical universe containing memory/facts");
    assert!(String::from_utf8_lossy(&denied.stderr).contains("ErasureUngranted"));
    assert!(!store.join("_erasure_frontier.json").exists(), "refused admission publishes an erasure");
    assert!(!root.join(".contextful/audit.key").exists(), "refused admission bootstraps an audit key");
    assert_eq!(files(&store), before, "refused admission changes original store bytes");
    let rows: serde_json::Value = serde_json::from_str(&ok(run(root, &["query", "--json", "--project", "research", "SELECT id FROM notes"], None, None))).unwrap();
    assert_eq!(rows["rows"], serde_json::json!([["erased"]]));
}

struct LiveMcp {
    child: std::process::Child,
    input: std::process::ChildStdin,
    responses: std::sync::mpsc::Receiver<serde_json::Value>,
}
impl Drop for LiveMcp {
    fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); }
}
impl LiveMcp {
    fn open(root: &std::path::Path, pins: &str, token: &str) -> Self {
        use std::io::BufRead;
        let mut child = Command::new(env!("CARGO_BIN_EXE_contextful")).current_dir(root)
            .args(["mcp", "--project", "research", "--public-key", pins, "--audience", "erasure-fixture", "--issuer-key", ".contextful/issuer.seed"])
            .env("CONTEXTFUL_TOKEN", token).env_remove("CONTEXTFUL_ISSUER_PUBKEY").env_remove("CONTEXTFUL_NODE_ID")
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (send, responses) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(output).lines() {
                let Ok(line) = line else { break };
                let Ok(value) = serde_json::from_str(&line) else { break };
                if send.send(value).is_err() { break; }
            }
        });
        Self { child, input, responses }
    }
    fn reference(&mut self) -> serde_json::Value {
        use std::io::Write;
        writeln!(self.input, "{}", serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"context.reference","arguments":{"table":"notes","run":"run-1","seq":0}}})).unwrap();
        self.input.flush().unwrap();
        self.responses.recv_timeout(std::time::Duration::from_secs(10)).expect("the bounded MCP request has no response")
    }
}

fn run(root: &std::path::Path, args: &[&str], token: Option<&str>, pins: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_contextful"));
    command.current_dir(root).args(args).env_remove("CONTEXTFUL_TOKEN").env_remove("CONTEXTFUL_ISSUER_PUBKEY").env_remove("CONTEXTFUL_NODE_ID");
    if let Some(token) = token { command.env("CONTEXTFUL_TOKEN", token); }
    if let Some(pins) = pins { command.env("CONTEXTFUL_ISSUER_PUBKEY", pins); }
    command.output().unwrap()
}
fn ok(output: Output) -> String {
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

#[test]
fn column_keyset_erases_every_declaring_table_without_subject_identity() {
    let directory = tempfile::tempdir().unwrap(); let root = directory.path();
    ok(run(root, &["init", "research", "--authoring-posture", "per_request"], None, None));
    std::fs::write(root.join("contextful.toml"), r#"authoring_posture = "per_request"
[[pipeline.tables]]
name = "notes"
primary_key = ["id"]
subject_id = "subject"
erasure_key = "trace_id"
columns = { id = "utf8", trace_id = "utf8", subject = "utf8" }
[[pipeline.tables]]
name = "events"
primary_key = ["trace_id", "id"]
[[pipeline.tables]]
name = "unrelated"
primary_key = ["id"]
columns = { id = "utf8" }
[[pipeline.tables]]
name = "subject_only"
primary_key = ["id"]
subject_id = "trace_id"
[[pipeline.tables]]
name = "partition_only"
primary_key = ["id"]
partition_by = ["trace_id"]
[[pipeline.tables]]
name = "order_only"
primary_key = ["id"]
order_by = "trace_id"
[[pipeline.tables]]
name = "cluster_only"
primary_key = ["id"]
cluster_by = ["trace_id"]
[[pipeline.tables]]
name = "hash_only"
primary_key = ["id"]
content_hash_column = "trace_id"
[[pipeline.tables]]
name = "index_only"
primary_key = ["id"]
indexes = [{kind = "fulltext", column = "trace_id"}]
"#).unwrap();
    std::fs::write(root.join(".contextful/issuance.toml"), "default_audience = \"erasure-fixture\"\nmax_lifetime_secs = 3600\n").unwrap();
    for (table, rows) in [
        ("notes", "{\"id\":\"a\",\"trace_id\":\"erase-trace\",\"subject\":\"alice\"}\n{\"id\":\"b\",\"trace_id\":\"keep-trace\",\"subject\":\"bob\"}\n"),
        ("events", "{\"id\":\"c\",\"trace_id\":\"erase-trace\"}\n{\"id\":\"d\",\"trace_id\":\"keep-trace\"}\n"),
        ("unrelated", "{\"id\":\"erase-trace\"}\n"),
    ] {
        std::fs::write(root.join("rows.jsonl"), rows).unwrap();
        ok(run(root, &["context", "land", table, "--project", "research", "--rows", "rows.jsonl", "--run-id", "run-1", "--site-id", "fixture"], None, None));
    }
    for table in ["subject_only", "partition_only", "order_only", "cluster_only", "hash_only", "index_only"] {
        std::fs::write(root.join("rows.jsonl"), "{\"id\":\"a\",\"trace_id\":\"erase-trace\"}\n{\"id\":\"b\",\"trace_id\":\"keep-trace\"}\n").unwrap();
        ok(run(root, &["context", "land", table, "--project", "research", "--rows", "rows.jsonl", "--run-id", "run-1", "--site-id", "fixture"], None, None));
    }
    let pins = ok(run(root, &["token", "keygen", "--out", ".contextful/issuer.seed"], None, None));
    let forget = ok(run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://fixture", "--agent", "agent://fixture", "--zone", "on-prem:fixture", "--action", "forget", "--table", "*", "--ttl", "3600"], None, None));
    let hash = format!("hmac-sha256:{}", "a".repeat(64));
    let store = root.join(".contextful/context/research");
    for malformed in [
        serde_json::json!({"subject_hash":hash,"column":"unknown","keys":["erase-trace"]}),
        serde_json::json!({"subject_hash":hash,"column":"trace_id","keys":[]}),
        serde_json::json!({"subject_hash":hash,"column":"trace_id","keys":[null]}),
        serde_json::json!({"subject_hash":hash,"column":"trace_id","keys":[{"id":"erase-trace"}]}),
        serde_json::json!({"subject_hash":hash,"column":"trace_id","keys":["erase-trace"],"unexpected":true}),
    ] {
        std::fs::write(root.join("keyset.json"), serde_json::to_vec(&malformed).unwrap()).unwrap();
        assert!(!run(root, &["context", "erase", "--project", "research", "--key-set", "keyset.json", "--issuer-key", ".contextful/issuer.seed", "--public-key", &pins, "--audience", "erasure-fixture", "--json"], Some(&forget), Some(&pins)).status.success());
        assert!(!store.join("_erasure_frontier.json").exists());
        assert!(!root.join(".contextful/audit.key").exists(), "invalid column input bootstraps durable erasure state");
    }
    std::fs::write(root.join("keyset.json"), serde_json::to_vec(&serde_json::json!({"subject_hash":hash,"column":"trace_id","keys":["erase-trace"]})).unwrap()).unwrap();
    let subset = ok(run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://fixture", "--agent", "agent://fixture", "--zone", "on-prem:fixture", "--action", "forget", "--table", "notes", "--ttl", "3600"], None, None));
    let request = ["context", "erase", "--project", "research", "--key-set", "keyset.json", "--issuer-key", ".contextful/issuer.seed", "--public-key", &pins, "--audience", "erasure-fixture", "--json"];
    assert!(!run(root, &request, Some(&subset), Some(&pins)).status.success());
    let mut narrowed = request.to_vec(); narrowed.extend(["--tables", "notes"]);
    assert!(!run(root, &narrowed, Some(&forget), Some(&pins)).status.success());
    assert!(!root.join(".contextful/audit.key").exists());
    assert!(!store.join("_erasure_frontier.json").exists());
    let receipt: serde_json::Value = serde_json::from_str(&ok(run(root, &["context", "erase", "--project", "research", "--key-set", "keyset.json", "--issuer-key", ".contextful/issuer.seed", "--public-key", &pins, "--audience", "erasure-fixture", "--json"], Some(&forget), Some(&pins)))).unwrap();
    assert_eq!(receipt["subject_hash"], hash);
    assert_eq!(receipt["affected_counts"]["notes"], 1);
    assert_eq!(receipt["affected_counts"]["events"], 1);
    for table in ["subject_only", "partition_only", "order_only", "cluster_only", "hash_only", "index_only"] {
        assert_eq!(receipt["affected_counts"][table], 1, "structural declaration remains outside column erasure: {table}");
        let query = format!("SELECT id FROM {table} ORDER BY id");
        assert_eq!(serde_json::from_str::<serde_json::Value>(&ok(run(root, &["query", "--json", "--project", "research", &query], None, Some(&pins)))).unwrap()["rows"], serde_json::json!([["b"]]));
    }
    let query = ["query", "--json", "--project", "research", "SELECT (SELECT count(*) FROM notes), (SELECT count(*) FROM events), (SELECT count(*) FROM unrelated)"];
    assert_eq!(serde_json::from_str::<serde_json::Value>(&ok(run(root, &query, None, Some(&pins)))).unwrap()["rows"], serde_json::json!([["1","1","1"]]));
    std::fs::write(root.join("keyset.json"), serde_json::to_vec(&serde_json::json!({"subject_hash":hash,"keys":[{"table":"notes","keys":{"id":"b"}}]})).unwrap()).unwrap();
    let mut explicit = request.to_vec(); explicit.extend(["--tables", "notes"]);
    let receipt: serde_json::Value = serde_json::from_str(&ok(run(root, &explicit, Some(&forget), Some(&pins)))).unwrap();
    assert_eq!(receipt["affected_counts"]["notes"], 1);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&ok(run(root, &query, None, Some(&pins)))).unwrap()["rows"], serde_json::json!([["0","1","1"]]));
}

#[test]
fn unpublished_recovery_requires_complete_scope_and_preserves_live_rows() {
    let directory = tempfile::tempdir().unwrap(); let root = directory.path();
    ok(run(root, &["init", "research", "--authoring-posture", "per_request"], None, None));
    std::fs::write(root.join("contextful.toml"), "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"notes\"\nprimary_key = [\"id\"]\n[[pipeline.tables]]\nname = \"keys\"\nprimary_key = [\"id\"]\n").unwrap();
    std::fs::write(root.join(".contextful/issuance.toml"), "default_audience = \"erasure-fixture\"\nmax_lifetime_secs = 3600\n").unwrap();
    std::fs::write(root.join("rows.jsonl"), "{\"id\":\"a\"}\n").unwrap();
    for table in ["notes", "keys"] {
        ok(run(root, &["context", "land", table, "--project", "research", "--rows", "rows.jsonl", "--run-id", "run-1", "--site-id", "fixture"], None, None));
    }
    let pins = ok(run(root, &["token", "keygen", "--out", ".contextful/issuer.seed"], None, None));
    let mint = |action, table| ok(run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://fixture", "--agent", "agent://fixture", "--zone", "on-prem:fixture", "--action", action, "--table", table, "--ttl", "3600"], None, None));
    let forget = mint("forget", "*");
    let subset = mint("forget", "notes");
    let read = mint("read", "*");
    let key = root.join(".contextful/audit.key");
    std::fs::write(&key, [42; 32]).unwrap();
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let store = root.join(".contextful/context/research");
    let staged = store.join("_erasure/staging").join("a".repeat(64));
    std::fs::create_dir_all(staged.join("tables/notes")).unwrap();
    let marker = staged.join("tables/notes/owned-unpublished-file");
    std::fs::write(&marker, b"unpublished replacement").unwrap();
    let recovery = ["context", "erase", "--recover", "--project", "research", "--public-key", &pins, "--audience", "erasure-fixture", "--json"];
    for token in [&subset, &read] {
        assert!(!run(root, &recovery, Some(token), Some(&pins)).status.success());
        assert_eq!(std::fs::read(&marker).unwrap(), b"unpublished replacement");
    }
    std::fs::remove_file(root.join(".contextful/issuer.seed")).unwrap();
    for _ in 0..2 {
        let receipt: serde_json::Value = serde_json::from_str(&ok(run(root, &recovery, Some(&forget), Some(&pins)))).unwrap();
        assert!(receipt["transaction_id"].is_null(), "unpublished recovery invents a committed identity");
        assert_eq!(receipt["physical_collection"], "complete");
        assert!(!staged.exists());
        assert!(!store.join("_erasure_frontier.json").exists());
        assert!(!root.join(".contextful/audit").exists(), "unsigned recovery creates signed evidence");
        assert_eq!(std::fs::read(&key).unwrap(), [42; 32]);
    }
    let query = ["query", "--json", "--project", "research", "SELECT (SELECT count(*) FROM notes), (SELECT count(*) FROM keys)"];
    assert_eq!(serde_json::from_str::<serde_json::Value>(&ok(run(root, &query, None, Some(&pins)))).unwrap()["rows"], serde_json::json!([["1","1"]]));
}

#[test]
fn signed_only_scope_cannot_collect_an_unrelated_unsigned_stage() {
    let directory = tempfile::tempdir().unwrap(); let root = directory.path();
    ok(run(root, &["init", "research", "--authoring-posture", "per_request"], None, None));
    std::fs::write(root.join("contextful.toml"), "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"notes\"\nprimary_key = [\"id\"]\nsubject_id = \"subject\"\n[[pipeline.tables]]\nname = \"keys\"\nprimary_key = [\"id\"]\n").unwrap();
    std::fs::write(root.join(".contextful/issuance.toml"), "default_audience = \"erasure-fixture\"\nmax_lifetime_secs = 3600\n").unwrap();
    std::fs::write(root.join("rows.jsonl"), "{\"id\":\"a\",\"subject\":\"alice\"}\n{\"id\":\"b\",\"subject\":\"bob\"}\n").unwrap();
    for table in ["notes", "keys"] {
        ok(run(root, &["context", "land", table, "--project", "research", "--rows", "rows.jsonl", "--run-id", "run-1", "--site-id", "fixture"], None, None));
    }
    let pins = ok(run(root, &["token", "keygen", "--out", ".contextful/issuer.seed"], None, None));
    let mint = |table| ok(run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://fixture", "--agent", "agent://fixture", "--zone", "on-prem:fixture", "--action", "forget", "--table", table, "--ttl", "3600"], None, None));
    let complete = mint("*"); let signed_only = mint("notes");
    ok(run(root, &["context", "erase", "--project", "research", "--subject", "alice", "--tables", "notes", "--issuer-key", ".contextful/issuer.seed", "--public-key", &pins, "--audience", "erasure-fixture", "--json"], Some(&complete), Some(&pins)));
    let recovery = ["context", "erase", "--recover", "--project", "research", "--public-key", &pins, "--audience", "erasure-fixture", "--json"];
    std::fs::remove_file(root.join(".contextful/issuer.seed")).unwrap();
    ok(run(root, &recovery, Some(&signed_only), Some(&pins)));
    let staged = root.join(".contextful/context/research/_erasure/staging").join("b".repeat(64));
    std::fs::create_dir_all(staged.join("tables/keys")).unwrap();
    let marker = staged.join("tables/keys/owned-unpublished-file");
    std::fs::write(&marker, b"unrelated replacement").unwrap();
    let refused = run(root, &recovery, Some(&signed_only), Some(&pins));
    assert!(!refused.status.success(), "a notes-only frontier grant discards an unrelated keys replacement");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("ErasureUngranted"), "{}", String::from_utf8_lossy(&refused.stderr));
    assert_eq!(std::fs::read(&marker).unwrap(), b"unrelated replacement");
    ok(run(root, &recovery, Some(&complete), Some(&pins)));
    assert!(!staged.exists());
    for relative in ["tables/unknown/partial", "unowned-root-file", "tables/research/partial"] {
        let marker = staged.join(relative);
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, b"unadmitted staging origin").unwrap();
        let refused = run(root, &recovery, Some(&complete), Some(&pins));
        assert!(!refused.status.success(), "recovery admits an absent or ambiguous table origin: {relative}");
        assert!(String::from_utf8_lossy(&refused.stderr).contains("ErasureTransactionIncomplete"), "{}", String::from_utf8_lossy(&refused.stderr));
        assert_eq!(std::fs::read(&marker).unwrap(), b"unadmitted staging origin");
        std::fs::remove_dir_all(&staged).unwrap();
    }
}

#[test]
fn signed_frontier_recovery_replays_without_signer_and_preserves_the_key_lifecycle() {
    let directory = tempfile::tempdir().unwrap(); let root = directory.path();
    ok(run(root, &["init", "research", "--authoring-posture", "per_request"], None, None));
    std::fs::write(root.join("contextful.toml"), "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"notes\"\nprimary_key = [\"id\"]\nsubject_id = \"subject\"\n").unwrap();
    std::fs::write(root.join(".contextful/issuance.toml"), "default_audience = \"erasure-fixture\"\nmax_lifetime_secs = 3600\n").unwrap();
    std::fs::write(root.join("rows.jsonl"), "{\"id\":\"a\",\"subject\":\"alice\"}\n{\"id\":\"b\",\"subject\":\"bob\"}\n").unwrap();
    ok(run(root, &["context", "land", "notes", "--project", "research", "--rows", "rows.jsonl", "--run-id", "run-1", "--site-id", "fixture"], None, None));
    let pins = ok(run(root, &["token", "keygen", "--out", ".contextful/issuer.seed"], None, None));
    let forget = ok(run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://fixture", "--agent", "agent://fixture", "--zone", "on-prem:fixture", "--action", "forget", "--table", "*", "--ttl", "3600"], None, None));
    let key = root.join(".contextful/audit.key");
    std::fs::write(&key, [42; 32]).unwrap();
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    std::fs::remove_file(&key).unwrap();
    let store = root.join(".contextful/context/research");
    assert!(!store.join("_erasure_frontier.json").exists());
    let outcome = run(root, &["context", "erase", "--project", "research", "--subject", "alice", "--tables", "notes", "--issuer-key", ".contextful/issuer.seed", "--public-key", &pins, "--audience", "erasure-fixture", "--json"], Some(&forget), Some(&pins));
    let receipt: serde_json::Value = serde_json::from_str(&ok(outcome)).unwrap();
    assert_eq!(receipt["physical_collection"], "complete", "first-use creation has no prior signed key history");
    assert_eq!(std::fs::read(&key).unwrap().len(), 32);
    let frontier = std::fs::read(store.join("_erasure_frontier.json")).unwrap();
    let audit_before = serde_json::to_vec(&contextful_policy::audit::entries(&root.join(".contextful/audit")).unwrap()).unwrap();
    let seed_path = root.join(".contextful/issuer.seed");
    let private_seed = std::fs::read(&seed_path).unwrap();
    std::fs::remove_file(&seed_path).unwrap();
    let recovery = ["context", "erase", "--recover", "--project", "research", "--public-key", &pins, "--audience", "erasure-fixture", "--json"];
    for _ in 0..2 {
        let recovered: serde_json::Value = serde_json::from_str(&ok(run(root, &recovery, Some(&forget), Some(&pins)))).unwrap();
        assert_eq!(recovered["transaction_id"], receipt["transaction_id"]);
        assert_eq!(recovered["physical_collection"], "complete");
        assert_eq!(std::fs::read(store.join("_erasure_frontier.json")).unwrap(), frontier);
        assert_eq!(serde_json::to_vec(&contextful_policy::audit::entries(&root.join(".contextful/audit")).unwrap()).unwrap(), audit_before);
        assert!(!seed_path.exists(), "recovery recreates a signing key");
    }
    let query = ["query", "--json", "--project", "research", "SELECT id FROM notes ORDER BY id"];
    assert_eq!(serde_json::from_str::<serde_json::Value>(&ok(run(root, &query, None, Some(&pins)))).unwrap()["rows"], serde_json::json!([["b"]]));
    std::fs::write(&seed_path, private_seed).unwrap();
    std::fs::remove_file(&key).unwrap();
    let refused = run(root, &["context", "erase", "--project", "research", "--subject", "bob", "--tables", "notes", "--issuer-key", ".contextful/issuer.seed", "--public-key", &pins, "--audience", "erasure-fixture", "--json"], Some(&forget), Some(&pins));
    assert!(!refused.status.success(), "published erasure loses its key without refusing");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("ErasureTransactionIncomplete"));
    assert!(!key.exists(), "published history creates a replacement pseudonym key");
    assert_eq!(std::fs::read(store.join("_erasure_frontier.json")).unwrap(), frontier);
}

#[test]
fn erase_requires_forget_and_explicit_signing_and_preserves_normal_pinned_reads() {
    let directory = tempfile::tempdir().unwrap(); let root = directory.path();
    ok(run(root, &["init", "research", "--authoring-posture", "per_request"], None, None));
    std::fs::write(root.join("contextful.toml"), "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"notes\"\nprimary_key = [\"id\"]\nsubject_id = \"subject\"\nerasure_key = \"id\"\n").unwrap();
    std::fs::write(root.join(".contextful/issuance.toml"), "default_audience = \"erasure-fixture\"\nmax_lifetime_secs = 3600\n").unwrap();
    std::fs::write(root.join("rows.jsonl"), "{\"id\":\"a\",\"subject\":\"alice\"}\n{\"id\":\"b\",\"subject\":\"bob\"}\n").unwrap();
    ok(run(root, &["context", "land", "notes", "--project", "research", "--rows", "rows.jsonl", "--run-id", "run-1", "--site-id", "fixture"], None, None));
    let pins = ok(run(root, &["token", "keygen", "--out", ".contextful/issuer.seed"], None, None));
    let mint = |action| ok(run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://fixture", "--agent", "agent://fixture", "--zone", "on-prem:fixture", "--action", action, "--table", "*", "--ttl", "3600"], None, None));
    let read = mint("read"); let forget = mint("forget");
    let args = ["context", "erase", "--project", "research", "--subject", "alice", "--tables", "notes", "--issuer-key", ".contextful/issuer.seed", "--public-key", &pins, "--audience", "erasure-fixture", "--json"];
    let refused = run(root, &args, Some(&read), Some(&pins));
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("ErasureUngranted"), "{}", String::from_utf8_lossy(&refused.stderr));
    let receipt: serde_json::Value = serde_json::from_str(&ok(run(root, &args, Some(&forget), Some(&pins)))).unwrap();
    assert_eq!(receipt["physical_collection"], "complete");
    assert!(!receipt["subject_hash"].as_str().unwrap().contains("alice"));
    let reference = ["context", "reference", "notes", "run-1", "0", "--project", "research", "--public-key", &pins, "--audience", "erasure-fixture", "--issuer-key", ".contextful/issuer.seed", "--json"];
    let erased: serde_json::Value = serde_json::from_str(&ok(run(root, &reference, Some(&read), Some(&pins)))).unwrap();
    assert_eq!(erased["columns"], serde_json::json!(["available", "reason"]));
    assert_eq!(erased["rows"], serde_json::json!([[false, "erased"]]));
    let unsigned = reference.iter().copied().filter(|argument| *argument != "--issuer-key" && *argument != ".contextful/issuer.seed").collect::<Vec<_>>();
    let refusal = run(root, &unsigned, Some(&read), Some(&pins));
    assert!(!refusal.status.success(), "a held chain answers without explicit signing custody");
    assert!(String::from_utf8_lossy(&refusal.stderr).contains("AuditLogAnchored"));
    ok(run(root, &["token", "keygen", "--out", ".contextful/other.seed"], None, None));
    let wrong = reference.iter().map(|argument| if *argument == ".contextful/issuer.seed" { ".contextful/other.seed" } else { *argument }).collect::<Vec<_>>();
    assert!(!run(root, &wrong, Some(&read), Some(&pins)).status.success(), "a private key replaces independently configured audit pins");
    let missing = ["context", "reference", "notes", "absent-run", "0", "--project", "research", "--public-key", &pins, "--audience", "erasure-fixture", "--issuer-key", ".contextful/issuer.seed", "--json"];
    let absent: serde_json::Value = serde_json::from_str(&ok(run(root, &missing, Some(&read), Some(&pins)))).unwrap();
    assert_eq!(absent["rows"], serde_json::json!([[false, "missing"]]));
    assert!(!run(root, &reference, Some(&forget), Some(&pins)).status.success(), "Forget authority admits a Read reference");
    let entries = contextful_policy::audit::entries(&root.join(".contextful/audit")).unwrap();
    assert!(entries.iter().any(|entry| entry.attributes["contextful.tool"] == "context.reference" && entry.attributes["contextful.read.outcome"] == "refused"), "the admitted transport refusal has no durable audit entry");
    let query = ["query", "--json", "--project", "research", "SELECT id FROM notes ORDER BY id"];
    assert!(!run(root, &query, None, None).status.success(), "an unconfigured reader trusted erasure artifacts");
    let rows: serde_json::Value = serde_json::from_str(&ok(run(root, &query, None, Some(&pins)))).unwrap();
    assert_eq!(rows["rows"], serde_json::json!([["b"]]));
    ok(run(root, &["context", "compact", "notes", "--project", "research"], None, Some(&pins)));
    assert_eq!(serde_json::from_str::<serde_json::Value>(&ok(run(root, &query, None, Some(&pins)))).unwrap()["rows"], rows["rows"]);
    let mut mcp = LiveMcp::open(root, &pins, &read);
    assert_eq!(mcp.reference()["result"]["structuredContent"]["rows"], serde_json::json!([[false, "erased"]]));
    // The independently configured ledger retires the signer after this reader opens.
    ok(run(root, &["token", "rotate", "--compromise"], None, None));
    let refused = mcp.reference();
    assert!(refused.get("error").is_some() || refused["result"]["isError"] == true, "an opened MCP reader retains retired signing authority: {refused}");
    assert!(refused["result"]["structuredContent"].get("rows").is_none());
}
