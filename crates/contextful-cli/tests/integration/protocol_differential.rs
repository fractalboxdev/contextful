//! The executable protocol model and the store's two lease paths agree after each generated
//! step.
#![cfg(feature = "data-plane")]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_contextful");

/// Generated cases per run: 1000 cases on both lease paths take about 15 s on a developer
/// machine, and the run repeats once for determinism inside `workspace.cli-formal`.
const CASES: u64 = 1000;

const STEPS: &str = "grant\nraise\nrenew\nexpire\nrelease\nsend\ndeliver\npause\nresume\ncrash\n";

fn run(seed: &str) -> std::process::Output {
    Command::new(BIN)
        .args(["formal", "protocol-differential", "--seed", seed, "--cases", &CASES.to_string()])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .unwrap()
}

#[test]
fn fixed_seed_replays_protocol_cases_against_the_store() {
    if Command::new("lake").arg("--version").output().is_err() {
        assert!(std::env::var_os("CONTEXTFUL_REQUIRE_LEAN").is_none(), "Lean is required");
        return;
    }
    let first = run("76004");
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    let second = run("76004");
    assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
    assert_eq!(first.stdout, second.stdout);
    let report = String::from_utf8_lossy(&first.stdout);
    assert!(report.contains("stale-fence-differential: 0"), "{report}");
    assert!(report.contains("paths=compaction,run"), "both lease paths replay every case: {report}");
    let replayed: u64 = report.split("regressions=").nth(1).unwrap().split(';').next().unwrap().trim().parse().unwrap();
    assert!(replayed >= 4, "{report}");
    contextful_eval::record::emit("stale-fence-differential", 0.0, CASES + replayed, 76004);
}

/// A stand-in model listing `steps` and echoing each step it reads.
fn reference(dir: &Path, steps: &str) -> std::path::PathBuf {
    let path = dir.join("reference.sh");
    std::fs::write(
        &path,
        format!("#!/bin/sh\nif [ \"$1\" = steps ]; then printf '{}'; exit 0; fi\nwhile IFS= read -r step; do printf 'reference: %s\\n' \"$step\"; done\n", steps.replace('\n', "\\n")),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn a_protocol_difference_prints_the_minimal_sequence_and_both_states() {
    let dir = tempfile::tempdir().unwrap();
    let reference = reference(dir.path(), STEPS);
    let regressions = dir.path().join("regressions.jsonl");
    std::fs::write(&regressions, "[{\"op\":\"grant\",\"node\":0},{\"op\":\"expire\"}]\n").unwrap();
    let out = Command::new(BIN)
        .args(["formal", "protocol-differential", "--cases", "0", "--reference"])
        .arg(&reference)
        .arg("--regressions")
        .arg(&regressions)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(error.contains("ProtocolConformanceDrift"), "{error}");
    assert!(error.contains("path=compaction"), "{error}");
    assert!(error.contains("sequence=[{\"op\":\"expire\"}]"), "{error}");
    assert!(error.contains("model=reference: expire"), "{error}");
    assert!(error.contains("store=lease[none] fence=0 head=0"), "{error}");
    assert!(std::fs::read_to_string(&regressions).unwrap().lines().any(|l| l == "[{\"op\":\"expire\"}]"), "the minimized case is kept");
}

/// A step kind the model lists and the harness lacks, or the reverse, stops the run before
/// any case.
// spec: assurance.differential-test.protocol-vocabulary@fe569caf
#[test]
fn a_step_the_model_and_the_harness_do_not_share_raises_vocabulary_drift() {
    let dir = tempfile::tempdir().unwrap();
    let regressions = dir.path().join("regressions.jsonl");
    std::fs::write(&regressions, "").unwrap();
    for (steps, named) in [(format!("{STEPS}checkpoint\n"), "checkpoint"), (STEPS.replace("renew\n", ""), "renew")] {
        let reference = reference(dir.path(), &steps);
        let out = Command::new(BIN)
            .args(["formal", "protocol-differential", "--cases", "1", "--reference"])
            .arg(&reference)
            .arg("--regressions")
            .arg(&regressions)
            .output()
            .unwrap();
        assert!(!out.status.success());
        let error = String::from_utf8_lossy(&out.stderr);
        assert!(error.contains("ProtocolVocabularyDrift") && error.contains(named), "{error}");
    }
}

/// The model's step listing equals the harness's vocabulary.
#[test]
fn the_model_lists_the_steps_the_harness_drives() {
    if Command::new("lake").arg("--version").output().is_err() {
        assert!(std::env::var_os("CONTEXTFUL_REQUIRE_LEAN").is_none(), "Lean is required");
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../formal/protocol");
    let out = Command::new("lake").args(["exe", "protocol", "steps"]).current_dir(&root).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), STEPS);
}

#[test]
fn generated_protocol_cases_cover_the_three_nodes_and_every_step() {
    let dump = || Command::new(BIN).args(["formal", "protocol-differential", "--seed", "76004", "--cases", "64", "--dump-cases"]).output().unwrap();
    let first = dump();
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    assert_eq!(first.stdout, dump().stdout);
    let cases: Vec<serde_json::Value> =
        first.stdout.split(|b| *b == b'\n').filter(|line| !line.is_empty()).map(|line| serde_json::from_slice(line).unwrap()).collect();
    assert_eq!(cases.len(), 64);
    let mut nodes = std::collections::BTreeSet::new();
    let mut operations = std::collections::BTreeSet::new();
    for case in cases {
        let steps = case.as_array().unwrap();
        assert_eq!(steps.len(), 32);
        for step in steps {
            if let Some(node) = step["node"].as_u64() {
                assert!(node < 3);
                nodes.insert(node);
            }
            operations.insert(step["op"].as_str().unwrap().to_string());
        }
    }
    assert_eq!(nodes, [0, 1, 2].into_iter().collect());
    assert_eq!(operations, STEPS.lines().map(str::to_string).collect());
}
