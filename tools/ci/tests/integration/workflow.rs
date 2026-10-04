//! `assurance.gate.remote-check`: the pull-request workflow dispatches every stage.

use crate::repo_root;
use std::process::Command;

/// The pull-request workflow dispatches every stage the gate subcommand defines to a remote runner, a split stage one part at a time, each as one status check labelled with its name.
// spec: assurance.gate.remote-check@1a47dbaa
#[test]
fn the_workflow_dispatches_every_gate_stage() {
    let out = Command::new(env!("CARGO_BIN_EXE_contextful-ci")).args(["stages", "--parts"]).current_dir(repo_root()).output().unwrap();
    assert!(out.status.success());
    let stages: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect();
    assert!(!stages.is_empty());
    let whole = Command::new(env!("CARGO_BIN_EXE_contextful-ci")).arg("stages").output().unwrap();
    for stage in String::from_utf8_lossy(&whole.stdout).lines() {
        let dispatched = stages.iter().any(|s| s == stage || s.strip_prefix(stage).is_some_and(|p| p.starts_with('.')));
        assert!(dispatched, "stage `{stage}` is not dispatched: {stages:?}");
    }

    let yml = std::fs::read_to_string(repo_root().join(".github/workflows/gate.yml")).unwrap();
    assert!(stages.contains(&"test-first.validate".to_string()), "{stages:?}");
    assert!(yml.contains("stage: ${{ fromJson(needs.prepare.outputs.stages) }}"), "{yml}");
    assert!(yml.contains("stages --parts --base"), "{yml}");
    assert!(yml.contains("ref: ${{ github.event.pull_request.head.sha }}"), "{yml}");
    assert!(yml.contains("BASE: ${{ github.event.pull_request.base.sha }}"), "{yml}");
    assert!(yml.contains("stages=%s\\n' \"$stages\" >> \"$GITHUB_OUTPUT\""), "{yml}");
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
