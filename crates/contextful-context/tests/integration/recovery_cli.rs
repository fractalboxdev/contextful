//! Canonical recovery through the owning workspace's actual CLI process.

#[path = "recovery_fixture.rs"]
mod fixture;

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
