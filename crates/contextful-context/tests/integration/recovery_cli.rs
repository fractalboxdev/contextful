//! Canonical recovery through the owning workspace's actual CLI process.

#[path = "recovery_fixture.rs"]
mod fixture;

#[test]
fn canonical_memory_universe_requires_complete_forget_before_collection() {
    let executable = executable();
    let directory = tempfile::tempdir().unwrap(); let root = directory.path();
    let run = |args: &[&str], token: Option<&str>| {
        let mut command = std::process::Command::new(&executable);
        command.current_dir(root).args(args).env_remove("CONTEXTFUL_TOKEN").env_remove("CONTEXTFUL_ISSUER_PUBKEY").env_remove("CONTEXTFUL_NODE_ID");
        if let Some(token) = token { command.env("CONTEXTFUL_TOKEN", token); }
        command.output().unwrap()
    };
    let ok = |output: std::process::Output| {
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    ok(run(&["init", "research", "--authoring-posture", "per_request"], None));
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
    std::fs::write(root.join("notes.jsonl"), "{\"id\":\"kept\",\"subject\":\"context-memory-canary\"}\n").unwrap();
    ok(run(&["context", "land", "notes", "--project", "research", "--rows", "notes.jsonl", "--run-id", "r1", "--site-id", "fixture"], None));
    let public = ok(run(&["token", "keygen", "--out", ".contextful/issuer.seed"], None));
    let token = ok(run(&["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://fixture", "--agent", "agent://fixture", "--zone", "on-prem:fixture", "--action", "forget", "--table", "notes", "--ttl", "3600"], None));
    std::fs::write(root.join("keys.json"), serde_json::json!({"subject_hash":format!("hmac-sha256:{}", "a".repeat(64)),"column":"subject","keys":["context-memory-canary"]}).to_string()).unwrap();
    let denied = run(&["context", "erase", "--project", "research", "--key-set", "keys.json", "--issuer-key", ".contextful/issuer.seed", "--public-key", &public, "--audience", "erasure-fixture", "--json"], Some(&token));
    assert!(!denied.status.success(), "canonical Context collection ignores the declared memory authority universe");
    assert!(String::from_utf8_lossy(&denied.stderr).contains("ErasureUngranted"));
    assert!(!root.join(".contextful/context/research/_erasure_frontier.json").exists());
    assert!(!root.join(".contextful/audit.key").exists());
}

#[test]
fn a_committed_process_crash_recovers_through_the_built_adapter() {
    fixture::process_crash_recovery(&executable());
}

#[test]
fn an_unpublished_process_crash_discards_only_owned_replacements() {
    fixture::unpublished_process_crash_recovery(&executable());
}

pub(super) fn executable() -> std::path::PathBuf {
    let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo supplies the owning package directory")).canonicalize().unwrap();
    let workspace = manifest.parent().unwrap().parent().unwrap();
    assert_eq!(workspace.join("crates/contextful-context").canonicalize().unwrap(), manifest);
    assert!(workspace.join("Cargo.toml").is_file());
    let output = std::process::Command::new("cargo").current_dir(workspace)
        .args(["build", "--locked", "--offline", "--message-format=json", "-p", "contextful-cli", "--no-default-features", "--features", "data-plane", "--bin", "contextful"])
        .output().unwrap();
    assert!(output.status.success(), "the actual owning CLI fails to build: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().lines().filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|artifact| artifact["reason"] == "compiler-artifact" && artifact["target"]["name"] == "contextful")
        .filter_map(|artifact| artifact["executable"].as_str().map(std::path::PathBuf::from)).next_back().expect("Cargo reports the actual built executable")
}
