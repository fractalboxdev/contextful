use std::path::Path;
use std::process::Command;

const TEST: &str = "harness::runtime_manifest_selects_only_its_declared_acceptance_workspace";

#[test]
fn runtime_manifest_selects_only_its_declared_acceptance_workspace() {
    if let Ok(expected) = std::env::var("ACCEPTANCE_PROVENANCE_EXPECTED_ROOT") {
        assert_eq!(contextful_acceptance::workspace_root(), Path::new(&expected).canonicalize().unwrap());
        let output = Command::new(contextful_acceptance::bin("acceptance-provenance-fixture")).output().unwrap();
        assert!(output.status.success());
        let compiled = String::from_utf8(output.stdout).unwrap();
        assert_eq!(Path::new(compiled.trim()).canonicalize().unwrap(), Path::new(&expected).join("crates/acceptance").canonicalize().unwrap(), "nested build compiles the runtime package's source");
        return;
    }
    let owned = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    assert_eq!(contextful_acceptance::workspace_root(), owned);
    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("crates/acceptance");
    std::fs::create_dir_all(package.join("src")).unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\nmembers=['crates/acceptance']\n").unwrap();
    std::fs::write(package.join("Cargo.toml"), "[package]\nname='contextful-acceptance'\nversion='0.1.0'\nedition='2021'\n[[bin]]\nname='acceptance-provenance-fixture'\npath='src/probe.rs'\n").unwrap();
    std::fs::write(package.join("src/lib.rs"), "").unwrap();
    std::fs::write(package.join("src/probe.rs"), "fn main() { println!(\"{}\", env!(\"CARGO_MANIFEST_DIR\")); }\n").unwrap();
    std::fs::write(dir.path().join("Cargo.lock"), "version=4\n[[package]]\nname='contextful-acceptance'\nversion='0.1.0'\n").unwrap();
    let run = |manifest: Option<&Path>| {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", TEST, "--nocapture"]).env("ACCEPTANCE_PROVENANCE_EXPECTED_ROOT", dir.path());
        if let Some(manifest) = manifest { command.env("CARGO_MANIFEST_DIR", manifest); } else { command.env_remove("CARGO_MANIFEST_DIR"); }
        command.output().unwrap()
    };
    let positive = run(Some(&package));
    assert!(positive.status.success(), "runtime manifest must select its own checkout: {} {}", String::from_utf8_lossy(&positive.stdout), String::from_utf8_lossy(&positive.stderr));
    for manifest in [None, Some(dir.path()), Some(package.join("Cargo.toml").as_path()), Some(dir.path().join("absent").as_path())] {
        assert!(!run(manifest).status.success(), "missing, unrelated and malformed runtime package ancestry must refuse");
    }
    std::fs::write(package.join("Cargo.toml"), "[package]\nname='unrelated-package'\nversion='0.1.0'\nedition='2021'\n").unwrap();
    assert!(!run(Some(&package)).status.success(), "a similarly located unrelated package must refuse");
    std::fs::write(package.join("Cargo.toml"), "malformed runtime manifest").unwrap();
    assert!(!run(Some(&package)).status.success(), "malformed Cargo declarations must refuse");
}
