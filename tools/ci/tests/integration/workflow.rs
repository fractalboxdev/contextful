//! `assurance.gate.remote-check`: the pull-request workflow dispatches every stage.

use crate::repo_root;
use std::process::Command;

/// The pull-request workflow dispatches every stage the gate subcommand defines to a remote runner, each as one status check labelled with the stage's name.
// spec: assurance.gate.remote-check@feec2cdf
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
    assert!(yml.contains("\"command\": \"cargo run --locked -q -p contextful-ci -- gate --predecessors --stage ${{ matrix.stage }}"));
}

/// The pull-request workflow dispatches only a head commit pushed to the repository itself; a pull request from a fork dispatches no stage and so carries none of the required checks.
// spec: assurance.gate.fork-dispatch@e05736d5
#[test]
fn a_fork_pull_request_dispatches_no_stage() {
    let yml = std::fs::read_to_string(repo_root().join(".github/workflows/gate.yml")).unwrap();
    let guard = yml
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("if: "))
        .expect("an `if:` guard on the dispatch job");
    assert!(
        guard.contains("github.event.pull_request.head.repo.full_name == github.repository"),
        "the dispatch job runs for a fork's head commit: {guard}"
    );
}
