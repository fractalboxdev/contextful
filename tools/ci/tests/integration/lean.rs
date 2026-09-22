//! The gate provisions the Lean toolchain `formal/lean-toolchain` pins before any stage
//! that runs tests, and holds Lean-backed tests to running rather than skipping.

use crate::{repo_root, stderr, Repo};
use std::process::Command;

/// A test that passes only when the gate provisioned Lean and asked for it.
const NEEDS_LEAN: &str = r#"#[test]
fn lean_is_provisioned() {
    assert_eq!(std::env::var("CONTEXTFUL_REQUIRE_LEAN").as_deref(), Ok("1"));
    let out = std::process::Command::new("lean").arg("--version").output().expect("lean on PATH");
    assert!(out.status.success());
}
"#;

/// A test that passes only when the gate did not ask for Lean.
const NO_LEAN: &str = r#"#[test]
fn lean_is_not_demanded() {
    assert!(std::env::var("CONTEXTFUL_REQUIRE_LEAN").is_err());
}
"#;

/// PATH with every elan directory removed, so only the gate's provisioning puts `lean` there.
fn path_without_elan() -> String {
    std::env::var("PATH").unwrap().split(':').filter(|d| !d.contains(".elan")).collect::<Vec<_>>().join(":")
}

fn installed_pin() -> Option<String> {
    let pin = std::fs::read_to_string(repo_root().join("formal/lean-toolchain")).ok()?;
    let home = std::env::var("HOME").ok()?;
    let dir = pin.trim().replace('/', "--").replace(':', "---");
    std::path::Path::new(&home).join(".elan/toolchains").join(dir).exists().then(|| pin.trim().to_string())
}

/// An elan home sharing this host's installed toolchains but configuring no default
/// toolchain, as a fresh CI install does, so `lean` resolves only through the gate's pin.
fn elan_home_without_default() -> tempfile::TempDir {
    let host = std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(".elan");
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("bin")).unwrap();
    for e in std::fs::read_dir(host.join("bin")).unwrap().flatten() {
        std::fs::copy(e.path(), dir.path().join("bin").join(e.file_name())).unwrap();
    }
    std::os::unix::fs::symlink(host.join("toolchains"), dir.path().join("toolchains")).unwrap();
    std::fs::write(dir.path().join("settings.toml"), "telemetry = false\nversion = \"12\"\n\n[overrides]\n").unwrap();
    dir
}

fn workspace_stage(r: &Repo) -> std::process::Output {
    workspace_stage_with(r, &[])
}

fn workspace_stage_with(r: &Repo, env: &[(&str, &std::path::Path)]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .args(["gate", "--stage", "workspace"])
        .envs(env.iter().copied())
        .current_dir(&r.root)
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CONTEXTFUL_REQUIRE_LEAN")
        .env("PATH", path_without_elan())
        .output()
        .unwrap()
}

#[test]
fn a_repository_pinning_lean_runs_its_tests_with_the_pinned_toolchain_required() {
    // Provisioning an uninstalled toolchain downloads it; the suite pins an installed one.
    let Some(pin) = installed_pin() else {
        eprintln!("skipped: the pinned Lean toolchain is not installed on this host");
        return;
    };
    let r = Repo::init();
    r.write("formal/lean-toolchain", &format!("{pin}\n"));
    r.write("crates/demo/tests/integration/main.rs", "mod double;\nmod lean;\n");
    r.write("crates/demo/tests/integration/lean.rs", NEEDS_LEAN);
    r.commit("a Lean model and a test that needs it");
    // The test runs `lean` from a package directory no `lean-toolchain` sits above.
    let elan = elan_home_without_default();
    let o = workspace_stage_with(&r, &[("ELAN_HOME", elan.path())]);
    assert!(o.status.success(), "{}", stderr(&o));
}

#[test]
fn a_repository_pinning_no_lean_demands_none() {
    let r = Repo::init();
    r.write("crates/demo/tests/integration/main.rs", "mod double;\nmod lean;\n");
    r.write("crates/demo/tests/integration/lean.rs", NO_LEAN);
    r.commit("no Lean model");
    let o = workspace_stage(&r);
    assert!(o.status.success(), "{}", stderr(&o));
}
