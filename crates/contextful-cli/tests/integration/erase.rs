//! Erasure through the built binary, with fixture-owned keys and disposable rows.
use std::process::{Command, Output};

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
