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
