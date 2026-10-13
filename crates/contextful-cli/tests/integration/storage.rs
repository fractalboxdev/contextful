//! Disposable container storage refuses missing capabilities before opening a project.
#![cfg(feature = "data-plane")]

use std::process::Command;

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_contextful"));
    command
        .env_remove("CONTEXTFUL_STORAGE_PROFILE")
        .env_remove("CONTEXTFUL_CATALOG_URL")
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
