//! `assurance.gate.remote-check`: the pull-request workflow dispatches every stage.

use crate::repo_root;
use std::process::Command;

#[test]
fn the_workflow_dispatches_every_gate_stage() {
    let out = Command::new(env!("CARGO_BIN_EXE_contextful-ci")).arg("stages").output().unwrap();
    assert!(out.status.success());
    let stages: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect();
    assert!(!stages.is_empty());

    let yml = std::fs::read_to_string(repo_root().join(".github/workflows/gate.yml")).unwrap();
    let matrix = yml
        .lines()
        .find_map(|l| l.trim().strip_prefix("stage: ["))
        .and_then(|l| l.strip_suffix(']'))
        .expect("a `stage: [...]` matrix in gate.yml");
    let dispatched: Vec<String> = matrix.split(',').map(|s| s.trim().to_string()).collect();
    assert_eq!(dispatched, stages);
    assert!(yml.contains("\"checkLabel\": \"${{ matrix.stage }}\""));
    assert!(yml.contains("contextful-ci -- gate --stage ${{ matrix.stage }}"));
}
