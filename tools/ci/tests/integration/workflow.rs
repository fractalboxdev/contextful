//! FlareDispatch owns gate checks; the sole Actions executor transports native Windows work.

use crate::repo_root;
use std::process::Command;
#[test]
fn native_transport_refuses_broad_triggers_credentials_and_unbound_runners() {
    let r = crate::Repo::init();
    let run = || r.run_ci(&["native-transport"]);
    // A test fixture is deliberately independent of the repository's executor.
    let mut valid = serde_json::json!({
        "name": "Native Windows executor",
        "on": {"workflow_dispatch": {"inputs": {"request": {"required": true, "type": "string"}}}},
        "permissions": {"contents": "read"},
        "jobs": {
            "x86_64": {"runs-on": "windows-2025", "timeout-minutes": 45,
                "steps": [{"run": "pwsh -File executor/.github/native-windows.ps1", "env": {"NATIVE_REQUEST": "${{ inputs.request }}", "NATIVE_TARGET": "x86_64-pc-windows-msvc"}}]},
            "aarch64": {"runs-on": "windows-11-arm", "timeout-minutes": 45,
                "steps": [{"run": "pwsh -File executor/.github/native-windows.ps1", "env": {"NATIVE_REQUEST": "${{ inputs.request }}", "NATIVE_TARGET": "aarch64-pc-windows-msvc"}}]}
        }
    });
    valid["run-name"] = serde_json::json!("native-${{ fromJSON(inputs.request).nonce }}");
    for (job, target) in [("x86_64", "x86_64-pc-windows-msvc"), ("aarch64", "aarch64-pc-windows-msvc")] {
        valid["jobs"][job]["if"] = serde_json::json!(format!("${{{{ fromJSON(inputs.request).target == '{target}' }}}}"));
        valid["jobs"][job]["steps"] = serde_json::json!([
            {"uses":"actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683", "with":{"ref":"${{ github.sha }}", "path":"executor", "persist-credentials":false}},
            {"uses":"actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683", "with":{"ref":"${{ fromJSON(inputs.request).head }}", "path":"workload", "persist-credentials":false,"fetch-depth":0}},
            {"run":"pwsh -File executor/.github/native-windows.ps1", "env":{"NATIVE_REQUEST":"${{ inputs.request }}","NATIVE_TARGET":target}},
            {"uses":"actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02", "if":"${{ always() }}", "with":{"name":"native-${{ fromJSON(inputs.request).nonce }}","path":"native-output","if-no-files-found":"error","retention-days":7}}
        ]);
    }
    let path = ".github/workflows/native-windows.yml";
    r.write(path, &serde_json::to_string(&valid).unwrap());
    r.write(".github/native-windows.ps1", "# trusted executor fixture\n");
    assert!(run().status.success(), "the dispatch-only native executor is refused: {}", crate::stderr(&run()));
    for mutation in ["push", "write", "runner", "unbound", "secret", "executor"] {
        let mut invalid = valid.clone();
        match mutation {
            "push" => invalid["on"]["push"] = serde_json::json!({}),
            "write" => invalid["permissions"]["contents"] = serde_json::json!("write"),
            "runner" => invalid["jobs"]["aarch64"]["runs-on"] = serde_json::json!("windows-2025"),
            "unbound" => invalid["jobs"]["x86_64"]["steps"][2]["env"]["NATIVE_REQUEST"] = serde_json::json!("{}"),
            "secret" => invalid["env"] = serde_json::json!({"TOKEN": "${{ secrets.PUBLISH_TOKEN }}"}),
            "executor" => invalid["jobs"]["x86_64"]["steps"][0]["with"]["ref"] = serde_json::json!("${{ fromJSON(inputs.request).head }}"),
            _ => unreachable!(),
        }
        r.write(path, &serde_json::to_string(&invalid).unwrap());
        assert!(!run().status.success(), "unsafe native transport {mutation} passes");
    }
    r.write(path, &serde_json::to_string(&valid).unwrap());
    r.write(".github/workflows/other.yml", "on: push\n");
    assert!(!run().status.success(), "a second Actions transport passes");
}



fn listed(args: &[&str]) -> Vec<String> {
    listed_in(repo_root(), args)
}

fn listed_in(root: &std::path::Path, args: &[&str]) -> Vec<String> {
    let out = Command::new(env!("CARGO_BIN_EXE_contextful-ci")).args(args).current_dir(root).output().unwrap();
    assert!(out.status.success(), "{args:?}: {}", crate::stderr(&out));
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
}

fn nightly(part: &str) -> bool {
    ["features", "formal", "budget"].iter().any(|stage| part == *stage || part.strip_prefix(stage).is_some_and(|rest| rest.starts_with('.')))
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
fn every_gate_stage_has_a_dispatchable_part() {
    let required = listed(&["stages", "--parts"]);
    assert_eq!(required.len(), 17, "the remote gate expects one required check per part: {required:?}");
    assert!(required.iter().any(|part| part == "workspace.cli-1"), "the CLI suite has no separate remote check: {required:?}");
    let mut parts = unfolded(&required);
    assert_eq!(parts.len(), 28, "a fold dropped or added a stage part: {parts:?}");
    parts.extend(listed(&["stages", "--parts", "--tier", "nightly"]));
    for stage in listed(&["stages"]) {
        let covered = parts.iter().any(|part| part == &stage || part.strip_prefix(stage.as_str()).is_some_and(|suffix| suffix.starts_with('.')));
        assert!(covered, "stage `{stage}` has no dispatchable part: {parts:?}");
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

/// `contextful-ci stages --parts --tier nightly` prints the features, formal and budget parts in run order; `stages --parts` prints the remaining parts, the required pull-request checks, and `gate --stage` runs a part of either tier.
// spec: assurance.gate.nightly-tier@be8be372
#[test]
fn the_nightly_tier_holds_the_features_formal_and_budget_parts() {
    let required = listed(&["stages", "--parts"]);
    assert_eq!(required, listed(&["stages", "--parts", "--tier", "pr"]));
    assert!(!required.is_empty() && !required.iter().any(|p| nightly(p)), "a nightly part is a required check: {required:?}");
    let tier = listed(&["stages", "--parts", "--tier", "nightly"]);
    assert_eq!(tier.len(), 22, "{tier:?}");
    assert!(tier.iter().all(|p| nightly(p)), "a required part runs nightly: {tier:?}");
    assert!(tier.first().is_some_and(|p| p.starts_with("features.")), "{tier:?}");
    assert!(tier.iter().any(|p| p == "formal") && tier.iter().any(|p| p.starts_with("budget.")), "{tier:?}");
    let rank = |p: &String| ["features", "formal", "budget"].iter().position(|stage| p.split('.').next() == Some(*stage));
    assert!(tier.is_sorted_by_key(rank), "the nightly parts leave run order: {tier:?}");

    let bogus = Command::new(env!("CARGO_BIN_EXE_contextful-ci")).args(["stages", "--parts", "--tier", "weekly"]).current_dir(repo_root()).output().unwrap();
    assert!(!bogus.status.success(), "an unknown tier lists parts");

    // A nightly part stays runnable by name, a dotted part as FlareDispatch dispatches it
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

/// The remote gate's required checks are exactly the parts `contextful-ci stages --parts`
/// prints, each one `contextful-ci gate --stage <part>`, the command a contributor runs.
// spec: assurance.automate.one-path@616b297c
#[test]
fn proposed_required_checks_match_every_gate_part() {
    let out = Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .args(["stages", "--parts"])
        .current_dir(repo_root())
        .output()
        .unwrap();
    assert!(out.status.success());
    let mut expected = vec!["flare-dispatch/contextful-gate".to_string()];
    expected.extend(String::from_utf8_lossy(&out.stdout).lines().map(|part| format!("flare-dispatch/check:{part}")));

    let proposal: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(repo_root().join(".github/rulesets/contextful-gate.proposed.json")).unwrap()).unwrap();
    assert_eq!(proposal["enforcement"], "disabled");
    let actual: Vec<String> = proposal["rules"][0]["parameters"]["required_status_checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|check| check["context"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn github_actions_contains_only_the_flare_governed_native_transport() {
    let out = Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .arg("native-transport").current_dir(repo_root()).output().unwrap();
    assert!(out.status.success(), "{}", crate::stderr(&out));
}
