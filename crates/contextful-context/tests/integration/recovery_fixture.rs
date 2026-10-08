use std::process::{Command, Output};

fn run(cli: &std::path::Path, root: &std::path::Path, args: &[&str], token: Option<&str>, pins: Option<&str>) -> Output {
    let mut command = Command::new(cli);
    command.current_dir(root).args(args).env_remove("CONTEXTFUL_TOKEN").env_remove("CONTEXTFUL_ISSUER_PUBKEY").env_remove("CONTEXTFUL_NODE_ID");
    if let Some(token) = token { command.env("CONTEXTFUL_TOKEN", token); }
    if let Some(pins) = pins { command.env("CONTEXTFUL_ISSUER_PUBKEY", pins); }
    command.output().unwrap()
}
fn ok(output: Output) -> String {
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

pub fn process_crash_recovery(cli: &std::path::Path) {
    use std::os::unix::process::ExitStatusExt;
    use std::time::{Duration, Instant};
    struct OwnedChild(std::process::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
    }
    fn audit_contents(root: &std::path::Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
        let mut pending = vec![root.to_path_buf()];
        let mut files = std::collections::BTreeMap::new();
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() { pending.push(entry.path()); }
                else { files.insert(entry.path().strip_prefix(root).unwrap().to_path_buf(), std::fs::read(entry.path()).unwrap()); }
            }
        }
        files
    }
    let directory = tempfile::tempdir().unwrap(); let root = directory.path();
    ok(run(cli, root, &["init", "research", "--authoring-posture", "per_request"], None, None));
    std::fs::write(root.join("contextful.toml"), "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"notes\"\nprimary_key = [\"id\"]\nsubject_id = \"subject\"\n[[pipeline.tables]]\nname = \"keys\"\nprimary_key = [\"id\"]\nsubject_id = \"subject\"\n").unwrap();
    std::fs::write(root.join(".contextful/issuance.toml"), "default_audience = \"erasure-fixture\"\nmax_lifetime_secs = 3600\n").unwrap();
    std::fs::write(root.join("rows.jsonl"), "{\"id\":\"a\",\"subject\":\"alice\"}\n{\"id\":\"b\",\"subject\":\"bob\"}\n").unwrap();
    ok(run(cli, root, &["context", "land", "notes", "--project", "research", "--rows", "rows.jsonl", "--run-id", "run-1", "--site-id", "fixture"], None, None));
    ok(run(cli, root, &["context", "land", "keys", "--project", "research", "--rows", "rows.jsonl", "--run-id", "run-1", "--site-id", "fixture"], None, None));
    let pins = ok(run(cli, root, &["token", "keygen", "--out", ".contextful/issuer.seed"], None, None));
    let forget = ok(run(cli, root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://fixture", "--agent", "agent://fixture", "--zone", "on-prem:fixture", "--action", "forget", "--table", "*", "--ttl", "3600"], None, None));
    let store = root.join(".contextful/context/research");
    let retired = store.join("tables/notes");
    let padding = retired.join("owned-padding");
    std::fs::create_dir(&padding).unwrap();
    // Ordinary admitted files make pre-collection verification observable without a production fault hook.
    for index in 0..4096 { std::fs::write(padding.join(format!("file-{index:05}")), b"fixture-owned inventory").unwrap(); }
    let mut child = OwnedChild(Command::new(cli).current_dir(root)
        .args(["context", "erase", "--project", "research", "--subject", "alice", "--tables", "notes,keys", "--issuer-key", ".contextful/issuer.seed", "--public-key", &pins, "--audience", "erasure-fixture", "--json"])
        .env("CONTEXTFUL_TOKEN", &forget).env("CONTEXTFUL_ISSUER_PUBKEY", &pins).env_remove("CONTEXTFUL_NODE_ID")
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap());
    let until = Instant::now() + Duration::from_secs(60);
    let frontier = store.join("_erasure_frontier.json");
    while !frontier.is_file() {
        assert!(child.0.try_wait().unwrap().is_none(), "erasure exits before the committed crash boundary is observed");
        assert!(Instant::now() < until, "erasure publishes no frontier inside the fixture deadline");
        std::thread::sleep(Duration::from_millis(1));
    }
    child.0.kill().unwrap();
    assert_eq!(child.0.wait().unwrap().signal(), Some(9), "the owned erasure process is actually killed");
    assert!(retired.is_dir(), "collection completed before the crash fixture captured its boundary");
    let retired_keys = store.join("tables/keys");
    assert!(retired_keys.is_dir(), "the complete retirement scope remains before recovery");
    let committed = std::fs::read(&frontier).unwrap();
    let audit_files = audit_contents(&root.join(".contextful/audit"));
    let recovery = ["context", "erase", "--recover", "--project", "research", "--public-key", &pins, "--audience", "erasure-fixture", "--json"];
    let mint = |action, table| ok(run(cli, root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://fixture", "--agent", "agent://fixture", "--zone", "on-prem:fixture", "--action", action, "--table", table, "--ttl", "3600"], None, None));
    let expired = ok(run(cli, root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://fixture", "--agent", "agent://fixture", "--zone", "on-prem:fixture", "--action", "forget", "--table", "*", "--ttl", "3600", "--now", "2020-01-01T00:00:00Z"], None, None));
    let retired_before = audit_contents(&retired);
    let keys_before = audit_contents(&retired_keys);
    for (token, expected) in [(mint("forget", "notes"), "ErasureUngranted"), (mint("read", "*"), "ErasureUngranted"), (expired, "AuthorityExpired")] {
        let refused = run(cli, root, &recovery, Some(&token), Some(&pins));
        assert!(!refused.status.success(), "recovery admits incomplete, unauthorized or expired authority");
        assert!(String::from_utf8_lossy(&refused.stderr).contains(expected), "{}", String::from_utf8_lossy(&refused.stderr));
        assert_eq!(audit_contents(&retired), retired_before, "refused recovery changes retired files");
        assert_eq!(audit_contents(&retired_keys), keys_before, "refused recovery collects another signed table");
        assert_eq!(std::fs::read(&frontier).unwrap(), committed);
    }
    ok(run(cli, root, &["token", "revoke", "--audience", "erasure-fixture"], None, None));
    let revoked = run(cli, root, &recovery, Some(&forget), Some(&pins));
    assert!(!revoked.status.success(), "recovery admits revoked authority");
    assert!(String::from_utf8_lossy(&revoked.stderr).contains("AuthorityRevoked"), "{}", String::from_utf8_lossy(&revoked.stderr));
    assert_eq!(audit_contents(&retired), retired_before);
    let current_forget = mint("forget", "*");
    for extra in [["--tables", "notes"], ["--subject", "alice"], ["--issuer-key", ".contextful/issuer.seed"]] {
        let mut conflicting = recovery.to_vec(); conflicting.extend(extra);
        assert!(!run(cli, root, &conflicting, Some(&current_forget), Some(&pins)).status.success(), "recovery accepts new selection or signing input");
        assert_eq!(audit_contents(&retired), retired_before);
        assert_eq!(audit_contents(&retired_keys), keys_before);
    }
    let key_path = root.join(".contextful/audit.key");
    let audit_key = std::fs::read(&key_path).unwrap();
    std::fs::remove_file(&key_path).unwrap();
    assert!(!run(cli, root, &recovery, Some(&current_forget), Some(&pins)).status.success(), "recovery bootstraps a missing persisted audit key");
    assert!(!key_path.exists());
    assert_eq!(audit_contents(&retired), retired_before);
    assert_eq!(audit_contents(&retired_keys), keys_before);
    std::fs::write(&key_path, audit_key).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::remove_file(root.join(".contextful/issuer.seed")).unwrap();
    ok(run(cli, root, &recovery, Some(&current_forget), Some(&pins)));
    assert!(!retired.exists(), "signed retired content survives built-surface recovery");
    assert!(!retired_keys.exists());
    assert_eq!(std::fs::read(&frontier).unwrap(), committed, "recovery selects no new transaction");
    assert_eq!(audit_contents(&root.join(".contextful/audit")), audit_files, "recovery appends or re-signs prior history");
    let query = ["query", "--json", "--project", "research", "SELECT id FROM notes ORDER BY id"];
    assert_eq!(serde_json::from_str::<serde_json::Value>(&ok(run(cli, root, &query, None, Some(&pins)))).unwrap()["rows"], serde_json::json!([["b"]]));
    ok(run(cli, root, &recovery, Some(&current_forget), Some(&pins)));
    assert_eq!(std::fs::read(&frontier).unwrap(), committed);
}
