//! Disposable container storage refuses missing capabilities before opening a project.
#![cfg(feature = "data-plane")]

use std::process::Command;

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_contextful"));
    command
        .env_remove("CONTEXTFUL_STORAGE_PROFILE")
        .env_remove("CONTEXTFUL_CATALOG_URL")
        .env_remove("CONTEXTFUL_D1_ENDPOINT")
        .env_remove("CONTEXTFUL_RECOVERY_ID");
    command
}

#[test]
fn an_incomplete_container_profile_names_its_missing_durable_binding() {
    let out = command()
        .args(["storage", "check"])
        .env("CONTEXTFUL_STORAGE_PROFILE", "postgres")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(
        error.contains("StorageCapabilityMissing") && error.contains("CONTEXTFUL_CATALOG_URL"),
        "{error}"
    );
}

#[test]
fn ordinary_storage_check_reports_its_execution_capabilities_as_json() {
    let dir = tempfile::tempdir().unwrap();
    let out = command()
        .args(["storage", "check"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["profile"], "local");
    assert_eq!(value["replacement_safe"], false);
    assert_eq!(value["strict_execution_guards"],false);
    assert!(value["capabilities"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("journal")));
}

#[test]
fn unsupported_storage_profiles_refuse_before_admitting_a_command() {
    let out = command()
        .args(["run", "history", "--project", "missing"])
        .env("CONTEXTFUL_STORAGE_PROFILE", "unknown")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(
        error.contains("StorageProfileUnsupported") && error.contains("unknown"),
        "{error}"
    );
}

#[test]
fn cloudflare_postgres_is_not_an_execution_profile() {
    let out = command()
        .args(["run", "history", "--project", "missing"])
        .env("CONTEXTFUL_STORAGE_PROFILE", "cloudflare-postgres")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(error.contains("StorageProfileUnsupported"), "{error}");
    assert!(error.contains("cloudflare-postgres"), "{error}");
}

#[test]
fn d1_profile_requires_a_private_binding_before_project_access() {
    let out = command()
        .args(["run", "history", "--project", "missing"])
        .env("CONTEXTFUL_STORAGE_PROFILE", "cloudflare-d1")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(error.contains("StorageCapabilityMissing"), "{error}");
    assert!(error.contains("CONTEXTFUL_D1_ENDPOINT"), "{error}");
    assert!(!error.contains("missing"), "{error}");
}

#[test]
#[cfg(feature = "d1-catalog")]
fn a_postgres_url_cannot_impersonate_the_d1_private_binding() {
    let out = command()
        .args(["run", "history", "--project", "missing"])
        .env("CONTEXTFUL_STORAGE_PROFILE", "cloudflare-d1")
        .env("CONTEXTFUL_D1_ENDPOINT", "postgres://localhost/execution")
        .env("CONTEXTFUL_RECOVERY_ID", "generation")
        .output().unwrap();
    assert!(!out.status.success());
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(error.contains("StorageBindingRouteInvalid"), "{error}");
    assert!(!error.contains("project `missing`"), "{error}");
}

#[test]
#[cfg(all(feature = "d1-catalog", feature = "s3-sync"))]
fn d1_storage_initialization_and_history_survive_a_new_cli_process() {
    use contextful_acceptance::s3::{S3Server, ACCESS_KEY, SECRET_KEY};
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut binding = Command::new("node")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../contextful-d1/tests/fixture.mjs"))
        .arg(root)
        .stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut line = String::new();
    BufReader::new(binding.stdout.take().unwrap()).read_line(&mut line).unwrap();
    let d1_port: u16 = line.trim().parse().unwrap();
    let s3 = S3Server::start("conformance");
    let store = root.join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(root.join("contextful.toml"), "[[pipeline.tables]]\nname = 'filings'\n").unwrap();
    std::fs::write(store.join("config.toml"), format!("[node]\nid = 'site'\n[sync]\nendpoint = '{}'\nbucket = 'conformance'\nprefix = 'd1-test'\ncoordination = 'cas'\npull_before_run = true\npush_after_run = true\naccess_key_id = 'env://D1_TEST_ACCESS_KEY'\nsecret_access_key = 'env://D1_TEST_SECRET_KEY'\n", s3.endpoint)).unwrap();
    let invoke = |operation: &[&str]| command()
        .args(operation).current_dir(root)
        .env("CONTEXTFUL_STORAGE_PROFILE", "cloudflare-d1")
        .env("CONTEXTFUL_D1_ENDPOINT", format!("http://127.0.0.1:{d1_port}"))
        .env("CONTEXTFUL_RECOVERY_ID", "generation")
        .env("D1_TEST_ACCESS_KEY", ACCESS_KEY)
        .env("D1_TEST_SECRET_KEY", SECRET_KEY)
        .output().unwrap();
    let initialize = invoke(&["storage", "initialize", "--project", "research"]);
    assert!(initialize.status.success(), "{}", String::from_utf8_lossy(&initialize.stderr));
    let checked = invoke(&["storage", "check", "--project", "research"]);
    assert!(checked.status.success(), "{}", String::from_utf8_lossy(&checked.stderr));
    let report: serde_json::Value = serde_json::from_slice(&checked.stdout).unwrap();
    assert_eq!(report["profile"], "cloudflare-d1");
    let history = invoke(&["run", "history", "--project", "research"]);
    assert!(history.status.success(), "{}", String::from_utf8_lossy(&history.stderr));
    let db = contextful_d1::D1::connect(&format!("http://127.0.0.1:{d1_port}")).unwrap();
    use sha2::{Digest, Sha256};
    let captured_key = "d1-test/captured-object";
    let captured_bytes = b"captured corpus";
    s3.seed(captured_key, captured_bytes);
    let captured = invoke(&["storage", "capture", "--project", "research"]);
    assert!(captured.status.success(), "{}", String::from_utf8_lossy(&captured.stderr));
    let identity = db.query("SELECT frontier_sha256 FROM contextful_recovery WHERE id = 1", vec![]).unwrap();
    let digest = identity.rows[0]["frontier_sha256"].as_str().unwrap();
    let marker = db.get_blob(digest).unwrap().unwrap();
    let marker: serde_json::Value = serde_json::from_slice(&marker).unwrap();
    assert_eq!(marker["objects"][0]["key"], captured_key);
    assert_eq!(marker["objects"][0]["sha256"], format!("{:x}", Sha256::digest(captured_bytes)));
    s3.seed(captured_key, b"changed after capture");
    let changed_capture = invoke(&["storage", "check", "--project", "research"]);
    assert!(!changed_capture.status.success(), "a changed captured object passed preflight");
    s3.seed(captured_key, captured_bytes);
    let intact_capture = invoke(&["storage", "check", "--project", "research"]);
    assert!(intact_capture.status.success(), "{}", String::from_utf8_lossy(&intact_capture.stderr));
    let corpus_key = "d1-test/corpus-object";
    let corpus_bytes = b"recoverable corpus";
    let corpus_digest = format!("{:x}", Sha256::digest(corpus_bytes));
    let frontier = serde_json::to_vec(&serde_json::json!({
        "project": "research", "generation": "generation",
        "objects": [{"key": corpus_key, "sha256": corpus_digest}],
    })).unwrap();
    let frontier_digest = format!("{:x}", Sha256::digest(&frontier));
    db.put_blob(&frontier_digest, &frontier).unwrap();
    db.query("UPDATE contextful_recovery SET frontier_sha256 = ? WHERE id = 1", vec![serde_json::json!(frontier_digest)]).unwrap();
    let missing = invoke(&["storage", "check", "--project", "research"]);
    assert!(!missing.status.success(), "a missing recorded corpus object passed preflight");
    assert!(String::from_utf8_lossy(&missing.stderr).contains("ExecutionRecoveryMismatch"));
    s3.seed(corpus_key, b"changed corpus");
    let changed = invoke(&["storage", "check", "--project", "research"]);
    assert!(!changed.status.success(), "a changed recorded corpus object passed preflight");
    assert!(String::from_utf8_lossy(&changed.stderr).contains("ExecutionRecoveryMismatch"));
    s3.seed(corpus_key, corpus_bytes);
    let recovered = invoke(&["storage", "check", "--project", "research"]);
    assert!(recovered.status.success(), "{}", String::from_utf8_lossy(&recovered.stderr));
    let token = "a".repeat(64);
    db.query("INSERT INTO contextful_guard (scope, token, admitted_at) VALUES ('global', ?, 1)", vec![serde_json::json!(token)]).unwrap();
    let guarded = invoke(&["storage", "guard", "--project", "research"]);
    assert!(guarded.status.success(), "{}", String::from_utf8_lossy(&guarded.stderr));
    let state: serde_json::Value = serde_json::from_slice(&guarded.stdout).unwrap();
    assert_eq!(state["token"], token);
    let cleared = invoke(&["storage", "reconcile-guard", "--project", "research", "--token", &token, "--evidence", "idempotency:fixture-retry"]);
    assert!(cleared.status.success(), "{}", String::from_utf8_lossy(&cleared.stderr));
    let guard_after = invoke(&["storage", "guard", "--project", "research"]);
    let state: serde_json::Value = serde_json::from_slice(&guard_after.stdout).unwrap();
    assert!(state["token"].is_null());
    let config_path = store.join("config.toml");
    let config = std::fs::read_to_string(&config_path).unwrap();
    std::fs::write(&config_path, config.replace(&s3.endpoint, "http://127.0.0.1:1")).unwrap();
    let unavailable = invoke(&["storage", "check", "--project", "research"]);
    assert!(!unavailable.status.success(), "an unreachable R2 bucket passed preflight");
    assert!(String::from_utf8_lossy(&unavailable.stderr).contains("StorageCapabilityMissing: R2 corpus bucket"));
    std::fs::write(&config_path, &config).unwrap();
    let blob = b"recorded callback value";
    let blob_digest = format!("{:x}", Sha256::digest(blob));
    db.put_blob(&blob_digest, blob).unwrap();
    db.query("INSERT INTO contextful_blob (sha256, put_at) VALUES (?, 1)", vec![serde_json::json!(blob_digest)]).unwrap();
    db.query("INSERT INTO contextful_run (run_id, pipeline_id, started_ns, row) VALUES ('backup-run', 'pipeline', '1791918353123456789', '{}')", vec![]).unwrap();
    db.query("INSERT INTO contextful_awakeable (token, version, body) VALUES ('awake', 1, '{}')", vec![]).unwrap();
    db.query("INSERT INTO contextful_export (name, version, body) VALUES ('receiver', 1, '{\"cursor\":\"delivery-1\"}')", vec![]).unwrap();
    db.query("INSERT INTO contextful_guard (scope, token, admitted_at) VALUES ('backup-guard', ?, 2)", vec![serde_json::json!("b".repeat(64))]).unwrap();
    db.query("INSERT INTO contextful_lease (key, holder, expires_us, fence) VALUES ('backup-lease', 'owner', 7, 3)", vec![]).unwrap();
    let captured = invoke(&["storage", "capture", "--project", "research"]);
    assert!(captured.status.success(), "{}", String::from_utf8_lossy(&captured.stderr));
    let _ = binding.kill();
    let _ = binding.wait();
    let replacement = tempfile::tempdir().unwrap();
    std::fs::copy(root.join("execution.sqlite"), replacement.path().join("execution.sqlite")).unwrap();
    let replacement_blobs = replacement.path().join("blobs");
    std::fs::create_dir_all(&replacement_blobs).unwrap();
    for entry in std::fs::read_dir(root.join("blobs")).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), replacement_blobs.join(entry.file_name())).unwrap();
    }
    let replacement_s3 = S3Server::start("conformance");
    for key in s3.keys() {
        replacement_s3.seed(&key, &s3.object(&key).unwrap());
    }
    let replacement_store = replacement.path().join(".contextful/context/research");
    std::fs::create_dir_all(&replacement_store).unwrap();
    std::fs::write(replacement.path().join("contextful.toml"), "[[pipeline.tables]]\nname = 'filings'\n").unwrap();
    std::fs::write(replacement_store.join("config.toml"), config.replace(&s3.endpoint, &replacement_s3.endpoint)).unwrap();
    let mut replacement_binding = Command::new("node")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../contextful-d1/tests/fixture.mjs"))
        .arg(replacement.path())
        .stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut line = String::new();
    BufReader::new(replacement_binding.stdout.take().unwrap()).read_line(&mut line).unwrap();
    let replacement_port: u16 = line.trim().parse().unwrap();
    let replacement_invoke = |operation: &[&str]| command()
        .args(operation).current_dir(replacement.path())
        .env("CONTEXTFUL_STORAGE_PROFILE", "cloudflare-d1")
        .env("CONTEXTFUL_D1_ENDPOINT", format!("http://127.0.0.1:{replacement_port}"))
        .env("CONTEXTFUL_RECOVERY_ID", "generation")
        .env("D1_TEST_ACCESS_KEY", ACCESS_KEY)
        .env("D1_TEST_SECRET_KEY", SECRET_KEY)
        .output().unwrap();
    let restored = replacement_invoke(&["storage", "check", "--project", "research"]);
    assert!(restored.status.success(), "{}", String::from_utf8_lossy(&restored.stderr));
    let restored_db = contextful_d1::D1::connect(&format!("http://127.0.0.1:{replacement_port}")).unwrap();
    assert_eq!(restored_db.get_blob(&blob_digest).unwrap().unwrap(), blob);
    assert_eq!(restored_db.query("SELECT started_ns FROM contextful_run WHERE run_id = 'backup-run'", vec![]).unwrap().rows[0]["started_ns"], "1791918353123456789");
    assert_eq!(restored_db.query("SELECT body FROM contextful_export WHERE name = 'receiver'", vec![]).unwrap().rows[0]["body"], "{\"cursor\":\"delivery-1\"}");
    assert_eq!(restored_db.query("SELECT token FROM contextful_awakeable WHERE token = 'awake'", vec![]).unwrap().rows.len(), 1);
    assert_eq!(restored_db.query("SELECT token FROM contextful_guard WHERE scope = 'backup-guard'", vec![]).unwrap().rows.len(), 1);
    assert_eq!(restored_db.query("SELECT fence FROM contextful_lease WHERE key = 'backup-lease'", vec![]).unwrap().rows[0]["fence"], 3);
    let _ = replacement_binding.kill();
    let _ = replacement_binding.wait();
}

#[test]
fn a_container_profile_requires_a_recovery_identity_before_connecting() {
    let out = command()
        .args(["storage", "check"])
        .env("CONTEXTFUL_STORAGE_PROFILE", "postgres")
        .env(
            "CONTEXTFUL_CATALOG_URL",
            "host=127.0.0.1 port=1 password=canary-secret dbname=execution",
        )
        .output()
        .unwrap();
    assert!(!out.status.success());
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(
        error.contains("StorageCapabilityMissing") && error.contains("CONTEXTFUL_RECOVERY_ID"),
        "{error}"
    );
    assert!(!error.contains("canary-secret"), "{error}");
}
