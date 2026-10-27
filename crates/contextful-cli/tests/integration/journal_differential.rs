//! The executable journal model and the journal adapters agree after each interleaved step.
#![cfg(feature = "data-plane")]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_contextful");

fn lean() -> bool {
    if Command::new("lake").arg("--version").output().is_err() {
        assert!(std::env::var_os("CONTEXTFUL_REQUIRE_LEAN").is_none(), "Lean is required");
        return false;
    }
    true
}

fn differential(args: &[&str], regressions: &Path) -> Output {
    Command::new(BIN)
        .args(["formal", "journal-differential"])
        .args(args)
        .arg("--regressions")
        .arg(regressions)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .unwrap()
}

/// The journal model is a total, computable step function over three callers and two keys; each journal-store and blob-store port call, effect, lapse, crash, sweep and retirement is one step.
// spec: assurance.model.journal-model@12f1af8c
// spec: assurance.differential-test.journal-harness@50810f6d
// spec: assurance.differential-test.journal-command@ef733f6b
// spec: run.journal.step-output@78bc23e4
// spec: run.journal.entry-key@62b1a12d
// spec: run.journal.claim-takeover@9f58eef0
// spec: run.journal.blob-sweep@a62b9f09
#[test]
fn fixed_seed_replays_interleaved_journal_cases_against_every_adapter() {
    if !lean() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let regressions = dir.path().join("journal-regressions.jsonl");
    let first = differential(&["--seed", "51", "--cases", "8"], &regressions);
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    let second = differential(&["--seed", "51", "--cases", "8"], &regressions);
    assert_eq!(first.stdout, second.stdout);
    let report = String::from_utf8_lossy(&first.stdout);
    assert!(report.contains("journal-differential: 0 drift cases"), "{report}");
    assert!(report.contains("adapters=memory,file,sqlite"), "{report}");
}

/// `no_dangling_blob` assumes an aged sweep spares a blob between its put and its record; `protocol journal check --no-grace` prints the shortest sequence reaching {{run.journal.missing-blob}}.
// spec: assurance.model.journal-grace@bcc338d8
#[test]
fn without_the_grace_window_the_model_and_every_adapter_reach_blob_missing() {
    if !lean() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let regressions = dir.path().join("journal-regressions.jsonl");
    let out = differential(&["--no-grace"], &regressions);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(report.contains("BlobMissing reproduced"), "{report}");
    let saved = std::fs::read_to_string(&regressions).unwrap();
    assert_eq!(saved.lines().count(), 1, "{saved}");
    assert!(saved.contains("\"grace\":false") && saved.contains("sweep aged"), "{saved}");
    // The saved sequence replays in its own mode before any generated case.
    let replay = differential(&["--cases", "0"], &regressions);
    assert!(replay.status.success(), "{}", String::from_utf8_lossy(&replay.stderr));
    assert!(String::from_utf8_lossy(&replay.stdout).contains("regressions=1"));
}

/// A step after which the journal model and an adapter differ raises `JournalConformanceDrift`, printing the adapter, the minimized sequence and both states.
// spec: assurance.differential-test.journal-drift@2065542f
#[test]
fn a_journal_difference_prints_the_minimal_sequence_and_both_states() {
    let dir = tempfile::tempdir().unwrap();
    let reference = dir.path().join("reference.sh");
    std::fs::write(&reference, "#!/bin/sh\nwhile IFS= read -r step; do printf 'reference: %s\\n' \"$step\"; done\n").unwrap();
    std::fs::set_permissions(&reference, std::fs::Permissions::from_mode(0o755)).unwrap();
    let regressions = dir.path().join("journal-regressions.jsonl");
    std::fs::write(&regressions, "{\"grace\":true,\"steps\":[\"begin 0 0\",\"lapse 1\"]}\n").unwrap();
    let out = Command::new(BIN)
        .args(["formal", "journal-differential", "--cases", "0", "--reference"])
        .arg(&reference)
        .arg("--regressions")
        .arg(&regressions)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(error.contains("JournalConformanceDrift"), "{error}");
    assert!(error.contains("adapter=memory"), "{error}");
    assert!(error.contains("sequence=[\"lapse 1\"]"), "{error}");
    assert!(error.contains("model=reference: lapse 1"), "{error}");
    assert!(error.contains("store=rows[k0=none k1=none] blobs=000"), "{error}");
}

fn protocol_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../formal/protocol")
}

fn lake(args: &[&str]) -> Output {
    let built = Command::new("lake").arg("build").current_dir(protocol_root()).output().unwrap();
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    Command::new("lake").args(args).current_dir(protocol_root()).output().unwrap()
}

/// Seven journal properties are Lean theorems over every reachable state: recorded values stay, journaled resolutions agree, one record per key, effects under a claim or lapsed lease, takeover from a lapsed holder, no dangling blob, no {{run.journal.missing-blob}}.
// spec: assurance.model.journal-safety@f022f6e0
#[test]
fn the_journal_theorems_pass_the_protocol_audit() {
    if !lean() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let report_path = dir.path().join("report.json");
    let out = Command::new(BIN).args(["formal", "check", "--root"]).arg(protocol_root()).arg("--report").arg(&report_path).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let report: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    for theorem in [
        "recorded_stable",
        "resolutions_agree",
        "one_record_per_key",
        "effect_only_when_claimed",
        "takeover_only_from_lapsed",
        "no_dangling_blob",
        "no_blob_missing",
    ] {
        let name = format!("Protocol.Journal.{theorem}");
        let row = report["constants"].as_array().unwrap().iter().find(|r| r["name"] == name.as_str()).unwrap_or_else(|| panic!("{name} is inventoried"));
        assert_eq!(row["verdict"], "ok", "{row}");
    }
}

/// `protocol journal check` evaluates every journal property on each state three callers reach on one key; a breaking state raises `JournalInvariantViolated`, printing the shortest sequence reaching it.
// spec: assurance.model.journal-check@da283315
#[test]
fn the_journal_check_covers_three_callers_on_one_key_and_prints_the_shortest_break() {
    if !lean() {
        return;
    }
    let ok = lake(&["exe", "protocol", "journal", "check"]);
    let out = String::from_utf8_lossy(&ok.stdout);
    assert!(ok.status.success() && out.contains("6 journal invariants hold") && out.contains("over 3 callers and 1 key"), "{out}");
    // Without the grace hypothesis an invariant breaks, and the check prints the shortest
    // sequence reaching the break.
    let broken = lake(&["exe", "protocol", "journal", "check", "--no-grace"]);
    let err = String::from_utf8_lossy(&broken.stderr);
    assert_eq!(broken.status.code(), Some(1), "{err}");
    assert!(err.contains("JournalInvariantViolated: no caller reads BlobMissing"), "{err}");
    let steps: Vec<&str> = err.lines().filter(|l| l.starts_with("  ")).collect();
    let claimed: usize = err.split("after ").nth(1).unwrap().split(' ').next().unwrap().parse().unwrap();
    assert_eq!(steps.len(), claimed, "{err}");
    assert!(steps.contains(&"  sweep aged"), "{err}");
}
