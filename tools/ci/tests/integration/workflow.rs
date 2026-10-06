//! The gate exposes one local command per remote check, and the repository holds no Actions workflows after cutover.

use crate::repo_root;
use std::process::Command;

#[test]
fn every_gate_stage_has_a_dispatchable_part() {
    let out = Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .args(["stages", "--parts"])
        .current_dir(repo_root())
        .output()
        .unwrap();
    assert!(out.status.success());
    let parts: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        parts.len(),
        23,
        "the remote gate expects one check per part: {parts:?}"
    );
    let whole = Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .arg("stages")
        .output()
        .unwrap();
    assert!(whole.status.success());
    for stage in String::from_utf8_lossy(&whole.stdout).lines() {
        let covered = parts.iter().any(|part| {
            part == stage
                || part
                    .strip_prefix(stage)
                    .is_some_and(|suffix| suffix.starts_with('.'))
        });
        assert!(
            covered,
            "stage `{stage}` has no dispatchable part: {parts:?}"
        );
    }
}

#[test]
fn no_github_actions_workflows_remain() {
    let workflows = repo_root().join(".github/workflows");
    let entries = std::fs::read_dir(&workflows)
        .map(|paths| {
            paths
                .filter_map(Result::ok)
                .map(|path| path.path())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert!(
        entries.is_empty(),
        "GitHub Actions workflows remain: {entries:?}"
    );
}
