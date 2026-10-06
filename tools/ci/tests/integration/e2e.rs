//! The release consumer flow is an explicit CI command with a MinIO variant.

use super::Repo;

#[test]
fn e2e_command_exposes_release_and_minio_modes() {
    let repo = Repo::init();
    let output = repo.run_ci(&["e2e", "--help"]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("--minio"), "{help}");
}

#[cfg(unix)]
#[test]
fn e2e_refuses_a_successful_cargo_run_selecting_zero_tests() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let repo = Repo::init();
    let bin = tempfile::tempdir().unwrap();
    let cargo = bin.path().join("cargo");
    std::fs::write(&cargo, "#!/bin/sh\nprintf 'running 0 tests\\n\\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out\\n'\n").unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap_or_default());
    let output = Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .arg("e2e")
        .current_dir(&repo.root)
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(!output.status.success(), "the consumer flow was not selected: {}", String::from_utf8_lossy(&output.stdout));
    assert!(String::from_utf8_lossy(&output.stderr).contains("no consumer flow test ran"));
}
