//! The gate's parts, tiers and folds, each one `contextful-ci gate --stage <part>`.

use crate::repo_root;
use std::process::Command;

fn listed(args: &[&str]) -> Vec<String> {
    listed_in(repo_root(), args)
}

fn listed_in(root: &std::path::Path, args: &[&str]) -> Vec<String> {
    let out = Command::new(env!("CARGO_BIN_EXE_contextful-ci")).args(args).current_dir(root).output().unwrap();
    assert!(out.status.success(), "{args:?}: {}", crate::stderr(&out));
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
}

fn nightly(part: &str) -> bool {
    ["features", "formal", "budget", "unwired"].iter().any(|stage| part == *stage || part.strip_prefix(stage).is_some_and(|rest| rest.starts_with('.')))
}

#[test]
fn the_committed_cli_shard_map_holds_every_shard_of_several_tests_within_budget() {
    let shards = listed(&["stages", "--parts"]).into_iter().filter(|p| p.strip_prefix("workspace.cli-").is_some_and(|k| k.parse::<usize>().is_ok())).count();
    let printed = listed(&["cli-shards"]);
    assert_eq!(printed.len(), shards, "{printed:?}");
    assert!(printed.iter().all(|line| !line.starts_with("cli-") || !line.contains(" 0 test(s)")), "a shard runs no mapped test: {printed:?}");
}

/// Each fold label `stages --folds` prints, mapped to the stage parts it runs.
fn folds() -> std::collections::BTreeMap<String, Vec<String>> {
    listed(&["stages", "--folds"])
        .iter()
        .map(|line| {
            let mut words = line.split_whitespace().map(str::to_string);
            (words.next().unwrap(), words.collect())
        })
        .collect()
}

/// The stage parts the required checks run, each fold expanded to its members.
fn unfolded(required: &[String]) -> Vec<String> {
    let folds = folds();
    required.iter().flat_map(|part| folds.get(part).cloned().unwrap_or_else(|| vec![part.clone()])).collect()
}

#[test]
fn every_gate_stage_has_a_runnable_part() {
    let required = listed(&["stages", "--parts"]);
    assert_eq!(required.len(), 17, "the pre-merge tier lists 17 parts: {required:?}");
    assert!(required.iter().any(|part| part == "workspace.cli-1"), "the CLI suite has no separate part: {required:?}");
    let mut parts = unfolded(&required);
    assert_eq!(parts.len(), 28, "a fold dropped or added a stage part: {parts:?}");
    parts.extend(listed(&["stages", "--parts", "--tier", "nightly"]));
    for stage in listed(&["stages"]) {
        let covered = parts.iter().any(|part| part == &stage || part.strip_prefix(stage.as_str()).is_some_and(|suffix| suffix.starts_with('.')));
        assert!(covered, "stage `{stage}` has no runnable part: {parts:?}");
    }
}

/// `stages --parts` folds pins, toolchain, schema and test-first validation into `preflight`; each package group's workspace and evaluate parts into `suite.<group>`, probes into `suite.ci`; crate graph, connectors and surfaces into `perimeter`; members stay runnable alone.
// spec: assurance.gate.folded-parts@6e10266d
#[test]
fn folded_parts_run_every_stage_part_once_in_run_order() {
    let folds = folds();
    let expect = |label: &str, members: &[&str]| assert_eq!(folds.get(label).map(Vec::as_slice), Some(&members.iter().map(|m| m.to_string()).collect::<Vec<_>>()[..]), "{folds:?}");
    expect("preflight", &["pins", "toolchain", "schema", "test-first.validate"]);
    expect("suite.cli", &["workspace.cli-formal", "evaluate.cli"]);
    expect("suite.foundation", &["workspace.foundation", "evaluate.foundation"]);
    expect("suite.runtime", &["workspace.runtime", "evaluate.runtime"]);
    expect("suite.read", &["workspace.read", "evaluate.read"]);
    expect("suite.ci", &["workspace.ci", "evaluate.ci", "evaluate.probe"]);
    expect("perimeter", &["crate-graph", "connectors", "surfaces"]);
    assert_eq!(folds.len(), 7, "{folds:?}");

    let required = listed(&["stages", "--parts"]);
    let parts = unfolded(&required);
    let mut once = parts.clone();
    once.sort();
    once.dedup();
    assert_eq!(once.len(), parts.len(), "a stage part runs in two checks: {parts:?}");
    for label in folds.keys() {
        assert!(required.contains(label), "fold `{label}` is no required check: {required:?}");
    }
    for member in folds.values().flatten() {
        assert!(!required.contains(member), "`{member}` runs both alone and folded: {required:?}");
    }
    // The members keep run order across the required list.
    let stages = listed(&["stages"]);
    let rank = |part: &String| stages.iter().position(|s| part.split('.').next() == Some(s.as_str())).unwrap();
    assert!(folds.values().all(|m| m.is_sorted_by_key(rank)), "{folds:?}");

    // A fold label selects every member's stage, the predecessors toolchain reads among them.
    let plan = listed(&["gate", "--plan", "--predecessors", "--base", "HEAD", "--stage", "preflight"]);
    assert_eq!(plan, ["pins", "toolchain", "schema", "test-first"]);
    assert_eq!(listed(&["gate", "--plan", "--base", "HEAD", "--stage", "suite.ci"]), ["workspace", "evaluate"]);
    assert_eq!(listed(&["gate", "--plan", "--base", "HEAD", "--stage", "perimeter"]), ["crate-graph", "connectors", "surfaces"]);
    // Each member stays runnable by its own name.
    assert_eq!(listed(&["gate", "--plan", "--base", "HEAD", "--stage", "evaluate.probe"]), ["evaluate"]);
}

/// `contextful-ci stages --parts --tier nightly` prints the features, formal, unwired and budget parts in run order; `stages --parts` prints the remaining parts, those a change runs before merge, and `gate --stage` runs a part of
/// either tier.
// spec: assurance.gate.nightly-tier@ec57ade2
#[test]
fn the_nightly_tier_holds_the_features_formal_unwired_and_budget_parts() {
    let required = listed(&["stages", "--parts"]);
    assert_eq!(required, listed(&["stages", "--parts", "--tier", "pr"]));
    assert!(!required.is_empty() && !required.iter().any(|p| nightly(p)), "a nightly part is a required check: {required:?}");
    let tier = listed(&["stages", "--parts", "--tier", "nightly"]);
    assert_eq!(tier.len(), 23, "{tier:?}");
    assert!(tier.iter().any(|p| p == "unwired"), "{tier:?}");
    assert!(tier.iter().all(|p| nightly(p)), "a required part runs nightly: {tier:?}");
    assert!(tier.first().is_some_and(|p| p.starts_with("features.")), "{tier:?}");
    assert!(tier.iter().any(|p| p == "formal") && tier.iter().any(|p| p.starts_with("budget.")), "{tier:?}");
    let rank = |p: &String| ["features", "formal", "unwired", "budget"].iter().position(|stage| p.split('.').next() == Some(*stage));
    assert!(tier.is_sorted_by_key(rank), "the nightly parts leave run order: {tier:?}");

    let bogus = Command::new(env!("CARGO_BIN_EXE_contextful-ci")).args(["stages", "--parts", "--tier", "weekly"]).current_dir(repo_root()).output().unwrap();
    assert!(!bogus.status.success(), "an unknown tier lists parts");

    // A nightly part stays runnable by name, a dotted part as `stages --parts` lists it
    // and a whole stage alike: the fixture declares no profile, so the budget stage
    // reports so and passes.
    let r = crate::Repo::init();
    r.write("crates/gated/Cargo.toml", &(crate::manifest("gated", "") + "\n[features]\nextra = []\n"));
    r.write("crates/gated/src/lib.rs", "");
    r.write("crates/gated/tests/integration/main.rs", "#[test]\nfn builds() {}\n");
    r.lock();
    r.commit("a featured package");
    let parts = listed_in(&r.root, &["stages", "--parts", "--tier", "nightly"]);
    assert_eq!(parts.first().map(String::as_str), Some("features.gated"), "{parts:?}");
    let o = r.gate(&["--stage", "features.gated"]);
    let err = crate::stderr(&o);
    assert!(o.status.success(), "{err}");
    assert!(err.contains("--- stage features") && !err.contains("--- stage pins"), "{err}");
    let o = r.gate(&["--stage", "budget"]);
    assert!(o.status.success(), "{}", crate::stderr(&o));
}

/// The gate invokes the subcommand a contributor invokes: every part `contextful-ci stages
/// --parts` prints, of either tier, is one `contextful-ci gate --stage <part>`.
// spec: assurance.automate.one-path@616b297c
#[test]
fn every_listed_part_is_one_gate_invocation() {
    let mut parts = listed(&["stages", "--parts"]);
    parts.extend(listed(&["stages", "--parts", "--tier", "nightly"]));
    assert!(parts.len() > 1, "{parts:?}");
    let stages = listed(&["stages"]);
    for part in &parts {
        let plan = listed(&["gate", "--plan", "--predecessors", "--base", "HEAD", "--stage", part]);
        assert!(!plan.is_empty() && plan.iter().all(|stage| stages.contains(stage)), "`gate --stage {part}` plans {plan:?}");
    }
}
