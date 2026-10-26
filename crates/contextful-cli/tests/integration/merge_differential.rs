//! The executable merge model and the store's merge agree on every generated case.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_contextful");

/// The protocol package's merge model decides each key of one writer's merge from that key's local entry, remote entry, remote tombstone, held flag and verification flag; one differing commit-log copy refuses the whole merge.
// spec: assurance.model.merge-model@36737c41
// spec: assurance.differential-test.merge-harness@d9a568a2
#[test]
fn fixed_seed_merges_agree_key_by_key() {
    if Command::new("lake").arg("--version").output().is_err() {
        assert!(std::env::var_os("CONTEXTFUL_REQUIRE_LEAN").is_none(), "Lean is required");
        return;
    }
    let run = || {
        Command::new(BIN)
            .args(["formal", "merge-differential", "--seed", "7", "--cases", "2048"])
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .output()
            .unwrap()
    };
    let first = run();
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    assert_eq!(first.stdout, run().stdout);
    let report = String::from_utf8_lossy(&first.stdout);
    assert!(report.contains("merge-differential: 0 drift cases"), "{report}");
    // The generated cases reach both refusals and the commit-log conflict.
    let count = |field: &str| -> u64 { report.split(&format!("{field}=")).nth(1).unwrap().split(|c: char| !c.is_ascii_digit()).next().unwrap().parse().unwrap() };
    assert!(count("conflicts") > 0 && count("refusals") > 0, "{report}");
}

/// A case on which the merge model and `merge_verified` differ raises `MergeConformanceDrift`, printing the case minimized to the fewest keys and both merges.
// spec: assurance.differential-test.merge-drift@f5be28fb
#[test]
fn a_merge_difference_prints_the_minimal_case_and_both_merges() {
    let dir = tempfile::tempdir().unwrap();
    let reference = dir.path().join("reference.sh");
    std::fs::write(&reference, "#!/bin/sh\nwhile IFS= read -r case; do echo 'reference'; done\n").unwrap();
    std::fs::set_permissions(&reference, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = Command::new(BIN).args(["formal", "merge-differential", "--cases", "4", "--reference"]).arg(&reference).output().unwrap();
    assert!(!out.status.success());
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(error.contains("MergeConformanceDrift"), "{error}");
    assert!(error.contains("model=reference"), "{error}");
    // Minimized to one key.
    let case = error.split("case=me=").nth(1).unwrap().split("; model=").next().unwrap();
    assert!(!case.contains(" ; "), "{error}");
}

/// Six merge properties are Lean theorems over every input: the scoped union, a foreign or unverified tombstone keeping its entry, only live tombstones listed, the commit-log refusal, and one writer's convergence at a fixed `now`.
// spec: assurance.model.merge-theorems@a7b2d69a
#[test]
fn the_merge_theorems_pass_the_protocol_audit() {
    if Command::new("lake").arg("--version").output().is_err() {
        assert!(std::env::var_os("CONTEXTFUL_REQUIRE_LEAN").is_none(), "Lean is required");
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../formal/protocol");
    let dir = tempfile::tempdir().unwrap();
    let report_path = dir.path().join("report.json");
    let out = Command::new(BIN).args(["formal", "check", "--root"]).arg(&root).arg("--report").arg(&report_path).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let report: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    for theorem in [
        "merge_scoped_union",
        "foreign_tombstone_keeps_entry",
        "unverified_tombstone_keeps_entry",
        "merged_tombstones_live",
        "merge_refuses_on_conflicts",
        "merge_converges",
    ] {
        let name = format!("Protocol.Merge.{theorem}");
        let row = report["constants"].as_array().unwrap().iter().find(|r| r["name"] == name.as_str()).unwrap_or_else(|| panic!("{name} is inventoried"));
        assert_eq!(row["verdict"], "ok", "{row}");
    }
}
