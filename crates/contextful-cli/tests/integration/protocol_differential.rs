//! The executable protocol model and Rust store agree after each generated step.

use std::path::Path;
use std::process::Command;
use std::os::unix::fs::PermissionsExt;

const BIN: &str = env!("CARGO_BIN_EXE_contextful");

fn run(seed: &str) -> std::process::Output {
    Command::new(BIN)
        .args([
            "formal",
            "protocol-differential",
            "--seed",
            seed,
            "--cases",
            "12",
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .unwrap()
}

#[test]
fn fixed_seed_replays_protocol_cases_against_the_store() {
    if Command::new("lake").arg("--version").output().is_err() {
        assert!(
            std::env::var_os("CONTEXTFUL_REQUIRE_LEAN").is_none(),
            "Lean is required"
        );
        return;
    }
    let first = run("76004");
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = run("76004");
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(first.stdout, second.stdout);
    let report = String::from_utf8_lossy(&first.stdout);
    assert!(report.contains("stale-fence-differential: 0"));
    let replayed: u64 = report.split("regressions=").nth(1).unwrap().trim().parse().unwrap();
    contextful_eval::record::emit("stale-fence-differential", 0.0, 12 + replayed, 76004);
}

#[test]
fn a_protocol_difference_prints_the_minimal_sequence_and_both_states() {
    let dir = tempfile::tempdir().unwrap();
    let reference = dir.path().join("reference.sh");
    std::fs::write(&reference, "#!/bin/sh\ncat >/dev/null\necho 'lease[none] catalog=999 cursor=0 n0(belief=none paused=false pending=none) n1(belief=none paused=false pending=none) n2(belief=none paused=false pending=none)'\n").unwrap();
    std::fs::set_permissions(&reference, std::fs::Permissions::from_mode(0o755)).unwrap();
    let regressions = dir.path().join("regressions.jsonl");
    std::fs::write(&regressions, "[{\"op\":\"expire\"}]\n").unwrap();
    let out = Command::new(BIN)
        .args(["formal", "protocol-differential", "--cases", "0", "--reference"])
        .arg(&reference)
        .arg("--regressions")
        .arg(&regressions)
        .output().unwrap();
    assert!(!out.status.success());
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(error.contains("ProtocolConformanceDrift"), "{error}");
    assert!(error.contains("sequence=[{\"op\":\"expire\"}]"), "{error}");
    assert!(error.contains("model=lease[none] catalog=999"), "{error}");
    assert!(error.contains("store=lease[none] catalog=0"), "{error}");
}
