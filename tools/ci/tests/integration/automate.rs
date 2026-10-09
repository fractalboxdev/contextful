//! `assurance.automate`: automation is typed subcommands over compiled binaries, and the
//! automation toolchain stays out of every build profile.

use crate::{repo_root, stderr};
use std::process::Command;

/// The commands of one automation step: its text split at `&&`, `;`, `|` and unescaped
/// line breaks, blank parts dropped.
fn commands(step: &str) -> Vec<String> {
    step.replace("\\\n", " ")
        .split(['\n', ';', '|'])
        .flat_map(|part| part.split("&&"))
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect()
}

/// A step breaching `assurance.automate.typed-subcommand`: more than 5 commands, or a
/// branch, loop or retry.
fn breach(step: &str) -> Option<String> {
    let cmds = commands(step);
    if cmds.len() > 5 {
        return Some(format!("{} commands", cmds.len()));
    }
    let words = ["if", "then", "for", "while", "until", "retry", "case"];
    cmds.iter().flat_map(|c| c.split_whitespace()).find(|w| words.contains(w)).map(|w| format!("`{w}`"))
}

/// Every `RUN` instruction of the tracked Dockerfile and every `run` step of a tracked
/// workflow, with where it sits.
fn steps() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let docker = std::fs::read_to_string(repo_root().join("Dockerfile")).unwrap();
    let mut current: Option<String> = None;
    for line in docker.lines() {
        if let Some(step) = current.as_mut() {
            step.push('\n');
            step.push_str(line);
        } else if let Some(rest) = line.strip_prefix("RUN ") {
            current = Some(rest.split_whitespace().filter(|w| !w.starts_with("--mount=")).collect::<Vec<_>>().join(" "));
        }
        if current.is_some() && !line.trim_end().ends_with('\\') {
            out.push(("Dockerfile".to_string(), current.take().unwrap()));
        }
    }
    let workflows = repo_root().join(".github/workflows");
    for entry in std::fs::read_dir(workflows).into_iter().flatten().flatten() {
        let text = std::fs::read_to_string(entry.path()).unwrap();
        let flow: serde_json::Value = serde_json::from_str(&text).expect("a workflow written as JSON");
        for (job, body) in flow["jobs"].as_object().into_iter().flatten() {
            for step in body["steps"].as_array().into_iter().flatten() {
                if let Some(run) = step["run"].as_str() {
                    out.push((format!("{}:{job}", entry.file_name().to_string_lossy()), run.to_string()));
                }
            }
        }
    }
    out
}

// spec: assurance.automate.typed-subcommand@5fa528a3
#[test]
fn every_automation_step_is_at_most_five_commands_without_a_branch_loop_or_retry() {
    assert!(breach("for f in *; do\n  cargo test -p \"$f\"\ndone").is_some(), "a loop passes the guard");
    assert!(breach("a\nb\nc\nd\ne\nf").is_some(), "six commands pass the guard");
    let steps = steps();
    assert!(steps.len() >= 2, "{steps:?}");
    let breaches: Vec<String> = steps.iter().filter_map(|(at, s)| breach(s).map(|b| format!("{at}: {b} in {s:?}"))).collect();
    assert_eq!(breaches, Vec::<String>::new());
}

/// The subcommands `contextful-ci --help` lists.
fn subcommands() -> Vec<String> {
    let o = Command::new(env!("CARGO_BIN_EXE_contextful-ci")).arg("--help").output().unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    let help = String::from_utf8_lossy(&o.stdout).to_string();
    help.lines()
        .skip_while(|l| !l.starts_with("Commands:"))
        .skip(1)
        .take_while(|l| l.starts_with("  "))
        .filter_map(|l| l.split_whitespace().next())
        .filter(|c| *c != "help")
        .map(str::to_string)
        .collect()
}

// spec: assurance.automate.subcommand-surface@75c9143f
#[test]
fn every_subcommand_has_generated_help_and_refuses_an_untyped_option() {
    let commands = subcommands();
    for expected in ["gate", "stages", "shellcheck", "vacuous", "test-layout"] {
        assert!(commands.iter().any(|c| c == expected), "{expected} absent from {commands:?}");
    }
    for command in &commands {
        let o = Command::new(env!("CARGO_BIN_EXE_contextful-ci")).args([command.as_str(), "--help"]).output().unwrap();
        assert!(o.status.success(), "{command}: {}", stderr(&o));
        let help = String::from_utf8_lossy(&o.stdout);
        assert!(help.contains(&format!("Usage: contextful-ci {command}")), "{command}: {help}");
        let o = Command::new(env!("CARGO_BIN_EXE_contextful-ci")).args([command.as_str(), "--no-such-option"]).output().unwrap();
        assert_eq!(o.status.code(), Some(2), "{command} accepted an untyped option: {}", stderr(&o));
    }
}

// spec: assurance.automate.build-time-toolchain@3e937ae2
#[test]
fn no_runtime_package_depends_on_the_automation_toolchain() {
    let o = Command::new("cargo").args(["metadata", "--format-version", "1", "--no-deps", "--offline"]).current_dir(repo_root()).output().unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    let meta: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let packages = meta["packages"].as_array().unwrap();
    let path_of = |p: &serde_json::Value| p["manifest_path"].as_str().unwrap().replace('\\', "/");
    // The automation binaries. The evaluation harness under tools/ ships inside the binary's
    // `eval` command, so it is no automation.
    let tools = ["contextful-ci", "contextful-spec", "contextful-probe"];
    for tool in tools {
        assert!(packages.iter().any(|p| p["name"] == tool && path_of(p).contains("/tools/")), "{tool} is no package under tools/");
    }
    let runtime: Vec<&serde_json::Value> = packages.iter().filter(|p| path_of(p).contains("/crates/")).collect();
    assert!(runtime.iter().any(|p| p["name"] == "contextful-cli"), "no runtime package found");
    let mut linked = Vec::new();
    for p in &runtime {
        for d in p["dependencies"].as_array().unwrap() {
            let shipped = d["kind"].is_null() || d["kind"] == "build";
            if shipped && tools.iter().any(|t| d["name"] == *t) {
                linked.push(format!("{} -> {}", p["name"], d["name"]));
            }
        }
    }
    assert_eq!(linked, Vec::<String>::new());
}
