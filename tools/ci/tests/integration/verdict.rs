//! `assurance.consumer`: the verdict a release cell ships, with `cargo build` answered by a
//! script writing a synthetic artifact and `contextful-conformance` by a script writing the
//! report it is handed.

use crate::repo_root;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ARTIFACT: &[u8] = b"synthetic release artifact\n";
pub(crate) const D1_KEYS: [&str; 7] = [
    "CONTEXTFUL_CONFORMANCE_D1_ENDPOINT", "CONTEXTFUL_CONFORMANCE_R2_ENDPOINT", "CONTEXTFUL_CONFORMANCE_R2_BUCKET",
    "CONTEXTFUL_CONFORMANCE_R2_ACCESS_KEY_ID", "CONTEXTFUL_CONFORMANCE_R2_SECRET_ACCESS_KEY",
    "CONTEXTFUL_CONFORMANCE_RECOVERY_ID", "CONTEXTFUL_CONFORMANCE_D1_DISPOSABLE",
];

fn version() -> String {
    let text = std::fs::read_to_string(repo_root().join("Cargo.toml")).unwrap();
    let doc: toml::Value = toml::from_str(&text).unwrap();
    doc["workspace"]["package"]["version"].as_str().unwrap().to_string()
}

fn native_target() -> String {
    match std::env::consts::OS {
        "macos" => format!("{}-apple-darwin", std::env::consts::ARCH),
        "linux" => format!("{}-unknown-linux-musl", std::env::consts::ARCH),
        other => panic!("no native release target for {other}"),
    }
}

fn registry_ids() -> Vec<String> {
    let text = std::fs::read_to_string(repo_root().join("crates/acceptance/src/conformance/manifest.json")).unwrap();
    let registry: serde_json::Value = serde_json::from_str(&text).unwrap();
    registry["scenarios"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap().to_string()).collect()
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn executable(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

/// One `contextful-ci release` cell whose consumer suite writes `scenarios`, or no report when
/// `scenarios` is null, and exits `exit`, appending each `--storage-profile` it receives to
/// `invocations`.
struct Cell {
    _bin: tempfile::TempDir,
    _target: tempfile::TempDir,
    dist: tempfile::TempDir,
    invocations: PathBuf,
    stem: String,
    run: Output,
}

impl Cell {
    fn package(profile: &str, scenarios: serde_json::Value, exit: i32, env: &[(&str, &str)]) -> Cell {
        Cell::package_on(profile, &native_target(), scenarios, exit, env)
    }

    fn package_on(profile: &str, triple: &str, scenarios: serde_json::Value, exit: i32, env: &[(&str, &str)]) -> Cell {
        let writes = if scenarios.is_null() { String::new() } else { String::from("printf '%s\\n' {} > \"$out\"\n") };
        let bin = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let dist = tempfile::tempdir().unwrap();
        let invocations = bin.path().join("invocations");
        let report = serde_json::json!({
            "schema_version":1, "suite_revision":"consumer-v3", "artifact_digest":format!("{:x}", Sha256::digest(ARTIFACT)),
            "feature_profile":profile, "scenarios":scenarios,
        });
        let suite = bin.path().join("suite");
        executable(&suite, &format!(
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do\n  case \"$1\" in\n    --output) shift; out=$1 ;;\n    --storage-profile) shift; printf '%s\\n' \"$1\" >> {} ;;\n  esac\n  shift\ndone\n{}exit {exit}\n",
            quote(invocations.to_str().unwrap()), writes.replace("{}", &quote(&report.to_string())),
        ));
        let real = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        executable(&bin.path().join("cargo"), &format!(
            "#!/bin/sh\nif [ \"$1\" = build ]; then\n  t=''; b=contextful; prev=''\n  for a in \"$@\"; do [ \"$prev\" = --target ] && t=\"$a\"; [ \"$prev\" = --bin ] && b=\"$a\"; prev=\"$a\"; done\n  d=\"$CARGO_TARGET_DIR/$t/release\"\n  mkdir -p \"$d\"\n  if [ \"$b\" = contextful ]; then printf 'synthetic release artifact\\n' > \"$d/$b\"; else cp {} \"$d/$b\"; fi\n  chmod +x \"$d/$b\"\n  exit 0\nfi\nexec {} \"$@\"\n",
            quote(suite.to_str().unwrap()), quote(&real),
        ));
        let mut command = Command::new(env!("CARGO_BIN_EXE_contextful-ci"));
        command.args(["release", "--profile", profile, "--target", triple, "--target-dir"]).arg(target.path())
            .arg("--out").arg(dist.path()).current_dir(repo_root())
            .env("PATH", format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap_or_default()));
        for key in D1_KEYS {
            command.env_remove(key);
        }
        for (key, value) in env {
            command.env(key, value);
        }
        let run = command.output().unwrap();
        let short = profile.strip_prefix("contextful-").unwrap();
        let stem = format!("contextful-{short}-{}-{triple}", version());
        Cell { _bin: bin, _target: target, dist, invocations, stem, run }
    }

    fn json(&self, name: &str) -> serde_json::Value {
        let path = self.dist.path().join(name);
        serde_json::from_slice(&std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))).unwrap()
    }

    fn report(&self) -> serde_json::Value {
        self.json(&format!("{}.compatibility.json", self.stem))
    }

    fn metadata(&self) -> serde_json::Value {
        self.json(&format!("{}.release.json", self.stem))
    }

    fn profiles_run(&self) -> Vec<String> {
        std::fs::read_to_string(&self.invocations).unwrap_or_default().lines().map(str::to_string).collect()
    }
}

/// The suite exits 1 over a report whose verdict fails; that exit is the verdict, not a crashed
/// process, so the shipped evidence records each scenario as the suite reported it.
// spec: assurance.consumer.verdict-exit@18128b98
#[test]
fn a_suite_exiting_on_its_failing_verdict_records_no_process_failure() {
    let unsupported = serde_json::json!([{
        "id":"pagination-checkpoint", "required":true, "outcome":"unsupported",
        "requirement":"run", "diagnostic":"ConformanceCapabilityMissing",
    }]);
    let cell = Cell::package("contextful-edge", unsupported.clone(), 1, &[]);
    assert!(cell.run.status.success(), "{}", String::from_utf8_lossy(&cell.run.stderr));
    let report = cell.report();
    let cases = report["scenarios"].as_array().unwrap();
    assert_eq!(cases[0], unsupported[0], "the suite's scenario lost its evidence: {report}");
    assert!(!cases.iter().any(|case| case["id"] == "suite-exit"), "the suite's own verdict exit gained a process failure: {report}");
    assert_eq!(report["compatible"], false);
    assert_eq!(cell.metadata()["compatibility"]["compatible"], false, "release metadata omits the verdict");

    let crashed = Cell::package("contextful-edge", unsupported, 2, &[]);
    assert!(crashed.run.status.success(), "{}", String::from_utf8_lossy(&crashed.run.stderr));
    let cases = crashed.report()["scenarios"].as_array().unwrap().clone();
    assert!(cases.iter().any(|case| case["id"] == "suite-exit" && case["outcome"] == "failed" && case["exit_code"] == 2),
        "a suite exiting 2 left no process failure: {cases:?}");

    let silent = Cell::package("contextful-edge", serde_json::Value::Null, 2, &[]);
    assert!(silent.run.status.success(), "{}", String::from_utf8_lossy(&silent.run.stderr));
    let report = silent.report();
    let cases = report["scenarios"].as_array().unwrap();
    assert!(cases.iter().any(|case| case["id"] == "suite-exit" && case["outcome"] == "failed" && case["exit_code"] == 2),
        "a suite exiting 2 without a report left no process failure: {report}");
    assert_eq!(report["compatible"], false);
}

/// A native full cell holding no D1 binding runs no `cloudflare-d1` pass and declares the
/// absence in its D1 report; a supplied binding still runs the pass.
// spec: assurance.consumer.d1-absent@601be0ea
#[test]
fn a_full_cell_without_a_d1_binding_declares_the_d1_pass_absent() {
    let passed = serde_json::json!([{
        "id":"pagination-checkpoint", "required":true, "outcome":"passed", "requirement":"run", "diagnostic":null,
    }]);
    let cell = Cell::package("contextful-full", passed.clone(), 0, &[]);
    assert!(cell.run.status.success(), "{}", String::from_utf8_lossy(&cell.run.stderr));
    assert_eq!(cell.profiles_run(), ["local"], "a cell without a D1 binding ran the D1 suite pass");
    let d1 = cell.json(&format!("{}.cloudflare-d1.compatibility.json", cell.stem));
    assert_eq!(d1["storage_profile"], "cloudflare-d1");
    assert_eq!(d1["binding_origin"], "absent");
    assert_eq!(d1["compatible"], false);
    let cases = d1["scenarios"].as_array().unwrap();
    assert_eq!(cases.iter().map(|c| c["id"].as_str().unwrap().to_string()).collect::<Vec<_>>(), registry_ids());
    assert!(cases.iter().all(|c| c["required"] == true && c["outcome"] == "skipped" && c["diagnostic"] == "ConformanceBindingAbsent"), "{d1}");
    let metadata = cell.metadata();
    assert_eq!(metadata["d1_compatibility"]["binding_origin"], "absent");
    assert_eq!(metadata["d1_compatibility"]["compatible"], false);
    assert_eq!(metadata["d1_compatibility"]["scenarios"], d1["scenarios"], "release metadata omits the D1 outcomes");
    let archived = Command::new("tar").arg("-xzOf").arg(cell.dist.path().join(format!("{}.tar.gz", cell.stem)))
        .arg(format!("{}/cloudflare-d1.compatibility.json", cell.stem)).output().unwrap();
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&archived.stdout).unwrap(), d1);

    let bound = Cell::package("contextful-full", passed, 0, &[("CONTEXTFUL_CONFORMANCE_D1_ENDPOINT", "http://contextful.d1")]);
    assert!(bound.run.status.success(), "{}", String::from_utf8_lossy(&bound.run.stderr));
    assert_eq!(bound.profiles_run(), ["local", "cloudflare-d1"]);
    let d1 = bound.json(&format!("{}.cloudflare-d1.compatibility.json", bound.stem));
    assert_eq!(d1["binding_origin"], "unverified", "an R2-less binding counts as a worker binding");
    assert_eq!(d1["compatible"], false);
    assert_eq!(bound.metadata()["d1_compatibility"]["scenarios"], d1["scenarios"], "release metadata omits the D1 outcomes");
}

/// One release cell's metadata, as `contextful-ci release` writes it, with the given verdicts.
fn cell_metadata(profile: &str, target: &str, local: serde_json::Value, d1: Option<serde_json::Value>) -> serde_json::Value {
    let short = profile.strip_prefix("contextful-").unwrap();
    let stem = format!("contextful-{short}-{}-{target}", version());
    let mut cell = serde_json::json!({
        "profile":profile, "target":target, "archive":format!("{stem}.tar.gz"), "sha256":"a".repeat(64),
        "sbom":format!("{stem}.cdx.json"), "compatibility":local,
    });
    if let Some(d1) = d1 {
        cell["d1_compatibility"] = d1;
    }
    cell
}

/// The formula step writes release notes stating every cell's local and `cloudflare-d1`
/// verdict, and refuses a manifest cell that carries none.
// spec: assurance.consumer.release-notes@9dc152b3
#[test]
fn formula_writes_release_notes_stating_every_cells_verdict() {
    let dist = tempfile::tempdir().unwrap();
    let manifest = dist.path().join("release-manifest.json");
    let plan = Command::new(env!("CARGO_BIN_EXE_contextful-ci")).args(["release", "--plan"]).current_dir(repo_root()).output().unwrap();
    assert!(plan.status.success());
    let mut cells: Vec<serde_json::Value> = String::from_utf8_lossy(&plan.stdout).lines().map(|line| {
        let (profile, target) = line.split_once(' ').unwrap();
        let local = match (profile, target) {
            ("contextful-full", "x86_64-unknown-linux-musl") => serde_json::json!({"compatible":true,
                "scenarios":[{"id":"pagination-checkpoint", "required":true, "outcome":"passed"}]}),
            (_, "x86_64-unknown-linux-musl") => serde_json::json!({"compatible":false, "scenarios":[
                {"id":"pagination-checkpoint", "required":true, "outcome":"unsupported", "diagnostic":"ConformanceCapabilityMissing"},
                {"id":"hard-crash-replay", "required":true, "outcome":"unsupported", "diagnostic":"ConformanceCapabilityMissing"}]}),
            _ => serde_json::json!({"compatible":false, "scenarios":[
                {"id":"pagination-checkpoint", "required":true, "outcome":"skipped", "diagnostic":"ConformanceTargetNotExecutable"}]}),
        };
        let d1 = (profile == "contextful-full").then(|| match target {
            "x86_64-unknown-linux-musl" => serde_json::json!({"compatible":false, "binding_origin":"unverified", "scenarios":[
                {"id":"pagination-checkpoint", "required":true, "outcome":"failed"}]}),
            "aarch64-apple-darwin" => serde_json::json!({"compatible":false, "binding_origin":"absent", "scenarios":[
                {"id":"pagination-checkpoint", "required":true, "outcome":"skipped", "diagnostic":"ConformanceTargetNotExecutable"}]}),
            _ => serde_json::json!({"compatible":false, "binding_origin":"absent", "scenarios":[
                {"id":"pagination-checkpoint", "required":true, "outcome":"skipped", "diagnostic":"ConformanceBindingAbsent"}]}),
        });
        cell_metadata(profile, target, local, d1)
    }).collect();
    std::fs::write(&manifest, serde_json::to_vec(&cells).unwrap()).unwrap();
    let formula = |dist: &Path| Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .args(["formula", "--manifest", manifest.to_str().unwrap(), "--dist", dist.to_str().unwrap(), "--base-url", "https://example.com/v"])
        .current_dir(repo_root()).output().unwrap();
    let run = formula(dist.path());
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    let notes = std::fs::read_to_string(dist.path().join("release-notes.md")).expect("the formula step writes release notes");
    let row = |profile: &str, target: &str| notes.lines().find(|l| l.contains(&format!("`{profile}`")) && l.contains(&format!("`{target}`")))
        .unwrap_or_else(|| panic!("no row for {profile} {target}:\n{notes}")).to_string();
    let rows = notes.lines().filter(|l| l.starts_with("| `contextful-")).count();
    assert_eq!(rows, 10, "{notes}");
    assert!(row("contextful-full", "x86_64-unknown-linux-musl").contains("| compatible |"), "{notes}");
    let control = row("contextful-control", "x86_64-unknown-linux-musl");
    assert!(control.contains("incompatible") && control.contains("2 unsupported"), "{control}");
    assert!(row("contextful-edge", "aarch64-apple-darwin").contains("ConformanceTargetNotExecutable"), "{notes}");
    let full_arm = row("contextful-full", "aarch64-unknown-linux-musl");
    assert!(full_arm.contains("absent"), "the D1 column hides the absent binding: {full_arm}");
    let full_x86 = row("contextful-full", "x86_64-unknown-linux-musl");
    assert!(full_x86.trim_end().ends_with("| incompatible: 1 failed |"), "the D1 column hides the failed outcomes: {full_x86}");
    let full_darwin = row("contextful-full", "aarch64-apple-darwin");
    assert!(full_darwin.trim_end().ends_with("| not executed: `ConformanceTargetNotExecutable` |"), "the D1 column hides the target diagnostic: {full_darwin}");
    assert!(control.trim_end().ends_with("| — |"), "a profile without a D1 pass reports one: {control}");

    cells[0]["compatibility"].as_object_mut().unwrap().remove("compatible");
    std::fs::write(&manifest, serde_json::to_vec(&cells).unwrap()).unwrap();
    let missing = tempfile::tempdir().unwrap();
    let refused = formula(missing.path());
    assert!(!refused.status.success(), "a cell without a verdict produced release notes");
    assert!(!missing.path().join("SHA256SUMS").exists() && !missing.path().join("Formula").exists(),
        "a refused manifest left formulae and checksums behind");
}
