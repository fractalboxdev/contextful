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
        29,
        "the remote gate expects one check per part: {parts:?}"
    );
    assert!(parts.iter().any(|part| part == "workspace.cli"), "the CLI suite has no separate remote check: {parts:?}");
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
