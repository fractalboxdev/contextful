use crate::{manifest, stderr, Repo};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const PIN: &str = "leanprover/lean4:v4.29.1";

fn executable(path: &Path, script: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\nset -eu\n{script}")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The child command consumes a pinned toolchain and a provisioned target, rather than
/// inheriting either from the host. Mock executables keep provisioning deterministic.
fn run(part: &str, required: bool) {
    run_with_package(part, required, Some("contextful-cli"));
}

fn run_with_package(part: &str, required: bool, package: Option<&str>) {
    let r = Repo::init();
    let tools = tempfile::tempdir().unwrap();
    let elan = tools.path().join("elan");
    let bin = tools.path().join("bin");
    let wasm = tools.path().join("wasm-lib");
    r.write("formal/lean-toolchain", &format!("{PIN}\n"));
    if let Some(package) = package {
        r.write(&format!("crates/{package}/Cargo.toml"), &manifest(package, ""));
        r.write(&format!("crates/{package}/tests/integration/main.rs"), "mod measured;\n");
        r.write(&format!("crates/{package}/tests/integration/measured.rs"), "#[test]\nfn emits() {}\n");
    }
    r.write("spec/spec.lock.json", "{\"clauses\":[{\"id\":\"assurance.measure.record\"}]}\n");
    r.write("evals/ledger.toml", "[entry.mandatory-proof]\nclause = \"assurance.measure.record\"\nmetric = \"proof.value\"\nkind = \"test\"\ntier = \"gate\"\nmethod = { test = \"contextful_cli::measured::emits\" }\ntarget = { op = \"==\", value = 0 }\n");
    let packages: Vec<_> = package.into_iter().map(|name| serde_json::json!({
        "name": name, "manifest_path": r.root.join(format!("crates/{name}/Cargo.toml")),
        "features": {"data-plane": [], "contextful-edge": []},
        "metadata": {"contextful": {"feature-runs": ["data-plane", "contextful-edge"]}}
    })).collect();
    let meta = serde_json::json!({"packages": packages});
    r.write("metadata.json", &meta.to_string());
    r.commit("a method requiring pinned Lean and WebAssembly");
    executable(&elan.join("bin/elan"), r#"
case "$1 $2" in
  'toolchain list') if test -f "$ELAN_HOME/installed"; then cat "$ELAN_HOME/installed"; fi ;;
  'toolchain install') test "$3" = "$EXPECTED_PIN"; printf '%s\n' "$3" > "$ELAN_HOME/installed" ;;
  *) exit 91 ;;
esac
"#);
    executable(&elan.join("bin/lean"), r#"
test "$ELAN_TOOLCHAIN" = "$EXPECTED_PIN"
test "$(cat "$ELAN_HOME/installed")" = "$EXPECTED_PIN"
printf '%s\n' "$ELAN_TOOLCHAIN"
"#);
    executable(&bin.join("rustc"), "printf '%s\\n' \"$WASM_LIB\"\n");
    executable(&bin.join("rustup"), r#"
test "$*" = 'target add wasm32-unknown-unknown'
mkdir -p "$WASM_LIB"
touch "$WASM_LIB/libstd.rlib"
"#);
    executable(&bin.join("cargo"), r#"
if test "$1" = metadata; then cat metadata.json; exit 0; fi
test "$1" = test
if test "$EXPECT_REQUIRED" = 1; then
  test "${CONTEXTFUL_REQUIRE_LEAN:-}" = 1 || { echo 'mandatory Lean flag absent' >&2; exit 92; }
  test "${CONTEXTFUL_REQUIRE_WASM:-}" = 1 || { echo 'mandatory Wasm flag absent' >&2; exit 93; }
  test "$(lean --version)" = "$EXPECTED_PIN"
  test -f "$(rustc --print target-libdir --target wasm32-unknown-unknown)/libstd.rlib"
else
  test -z "${CONTEXTFUL_REQUIRE_LEAN:-}"
  test -z "${CONTEXTFUL_REQUIRE_WASM:-}"
fi
printf '%s\n' "$*" >> child-commands
if test -n "${CONTEXTFUL_MEASURE_DIR:-}"; then
  mkdir -p "$CONTEXTFUL_MEASURE_DIR"
  printf '%s\n' '{"id":"mandatory-proof","value":0,"n":1,"seed":7,"run":{"processor":"fixture","nproc":1,"memory_limit":null}}' > "$CONTEXTFUL_MEASURE_DIR/mandatory-proof.json"
fi
"#);
    let host_path = std::env::var("PATH").unwrap();
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&host_path))).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .args(["gate", "--stage", part])
        .current_dir(&r.root)
        .env("PATH", path)
        .env("ELAN_HOME", &elan)
        .env("WASM_LIB", &wasm)
        .env("EXPECTED_PIN", PIN)
        .env("EXPECT_REQUIRED", if required { "1" } else { "0" })
        .env_remove("ELAN_TOOLCHAIN")
        .env_remove("CONTEXTFUL_REQUIRE_LEAN")
        .env_remove("CONTEXTFUL_REQUIRE_WASM")
        .env_remove("CARGO_TARGET_DIR")
        .output().unwrap();
    assert!(output.status.success(), "{part}: {}", stderr(&output));
    let commands = std::fs::read_to_string(r.root.join("child-commands")).unwrap_or_default();
    assert_eq!(!commands.is_empty(), package.is_some(), "{part} selected child tests");
    assert_eq!(elan.join("installed").exists(), required, "{part} toolchain provisioning");
    assert_eq!(wasm.join("libstd.rlib").exists(), required, "{part} target provisioning");
    if part == "evaluate" {
        assert!(stderr(&output).contains("measure: mandatory-proof = 0"), "{}", stderr(&output));
    }
}

#[test]
fn evaluate_requires_pinned_lean_and_wasm_before_collecting_a_record() { run("evaluate", true); }

#[test]
fn formal_none_requires_pinned_lean_and_wasm() { run("features.formal-none", true); }

#[test]
fn formal_all_requires_pinned_lean_and_wasm() { run("features.formal-all", true); }

#[test]
fn formal_data_plane_requires_pinned_lean_and_wasm() { run("features.formal-data-plane", true); }

#[test]
fn formal_contextful_edge_requires_pinned_lean_and_wasm() { run("features.formal-contextful-edge", true); }

#[test]
fn whole_features_requires_pinned_lean_and_wasm() { run("features", true); }

#[test]
fn binary_only_features_preserve_the_narrow_toolchain_cost() { run("features.binary-none", false); }

#[test]
fn whole_features_without_a_binary_preserve_the_narrow_toolchain_cost() {
    run_with_package("features", false, Some("demo"));
}

#[test]
fn whole_features_without_featured_packages_provision_no_toolchains() {
    run_with_package("features", false, None);
}
