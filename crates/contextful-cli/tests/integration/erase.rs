//! Erasure through the built binary, with fixture-owned keys and disposable rows.
use std::process::{Command, Output};

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
fn erase_requires_forget_and_explicit_signing_and_preserves_normal_pinned_reads() {
    let directory = tempfile::tempdir().unwrap(); let root = directory.path();
    ok(run(root, &["init", "research", "--authoring-posture", "per_request"], None, None));
    std::fs::write(root.join("contextful.toml"), "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"notes\"\nprimary_key = [\"id\"]\nsubject_id = \"subject\"\nerasure_key = \"id\"\n").unwrap();
    std::fs::write(root.join(".contextful/issuance.toml"), "default_audience = \"erasure-fixture\"\nmax_lifetime_secs = 3600\n").unwrap();
    std::fs::write(root.join("rows.jsonl"), "{\"id\":\"a\",\"subject\":\"alice\"}\n{\"id\":\"b\",\"subject\":\"bob\"}\n").unwrap();
    ok(run(root, &["context", "land", "notes", "--project", "research", "--rows", "rows.jsonl", "--run-id", "run-1", "--site-id", "fixture"], None, None));
    let pins = ok(run(root, &["token", "keygen", "--out", ".contextful/issuer.seed"], None, None));
    let mint = |action| ok(run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://fixture", "--agent", "agent://fixture", "--zone", "fixture", "--action", action, "--table", "*", "--ttl", "3600"], None, None));
    let read = mint("read"); let forget = mint("forget");
    let args = ["context", "erase", "--project", "research", "--subject", "alice", "--tables", "notes", "--issuer-key", ".contextful/issuer.seed", "--public-key", &pins, "--audience", "erasure-fixture", "--json"];
    let refused = run(root, &args, Some(&read), Some(&pins));
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("ErasureUngranted"), "{}", String::from_utf8_lossy(&refused.stderr));
    let receipt: serde_json::Value = serde_json::from_str(&ok(run(root, &args, Some(&forget), Some(&pins)))).unwrap();
    assert_eq!(receipt["physical_collection"], "complete");
    assert!(!receipt["subject_hash"].as_str().unwrap().contains("alice"));
    let query = ["query", "--json", "--project", "research", "SELECT id FROM notes ORDER BY id"];
    assert!(!run(root, &query, None, None).status.success(), "an unconfigured reader trusted erasure artifacts");
    let rows: serde_json::Value = serde_json::from_str(&ok(run(root, &query, None, Some(&pins)))).unwrap();
    assert_eq!(rows["rows"], serde_json::json!([["b"]]));
    ok(run(root, &["context", "compact", "notes", "--project", "research"], None, Some(&pins)));
    assert_eq!(serde_json::from_str::<serde_json::Value>(&ok(run(root, &query, None, Some(&pins)))).unwrap()["rows"], rows["rows"]);
}
