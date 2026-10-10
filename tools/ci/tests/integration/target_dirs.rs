//! `assurance.build`: each cargo stage's own target directory, the workspace stage's one
//! engine-linked invocation, and the debug information the development and test profiles
//! carry.

use crate::{manifest, repo_root, stderr, Repo};
use std::process::Command;

/// A `cargo` recording `<CARGO_TARGET_DIR> <args>` per invocation in `log`, leaving a file
/// in that directory, and exiting `code`; `metadata` and `tree` reach the real cargo.
fn recording(log: &std::path::Path, code: i32) -> String {
    let real = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    format!(
        "case \"$1\" in metadata|tree) exec '{real}' \"$@\" ;; esac\necho \"$CARGO_TARGET_DIR $*\" >> '{}'\n\
         mkdir -p \"$CARGO_TARGET_DIR\" && touch \"$CARGO_TARGET_DIR/built\"\nexit {code}\n",
        log.display()
    )
}

fn with_acceptance(r: &Repo) {
    r.write("crates/acceptance/Cargo.toml", &manifest("contextful-acceptance", ""));
    r.write("crates/acceptance/src/lib.rs", "");
    r.write("crates/acceptance/tests/integration/main.rs", "");
    r.commit("acceptance package");
}

#[test]
fn inherited_cargo_targets_keep_stages_off_the_checkout_and_preserve_the_pool() {
    let r = Repo::init();
    with_acceptance(&r);
    r.write("crates/featured/Cargo.toml", &(manifest("featured", "") + "\n[features]\nextra = []\n"));
    r.write("crates/featured/src/lib.rs", "");
    r.write("crates/featured/tests/integration/main.rs", "");
    r.commit("featured package");
    let pool = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let log = pool.path().join("cargo.log");
    let marker = pool.path().join("outer-build");
    std::fs::write(&marker, "keep").unwrap();
    let path = format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap_or_default());
    for code in [0, 1] {
        std::fs::write(bin.path().join("cargo"), format!("#!/bin/sh\n{}", recording(&log, code))).unwrap();
        std::fs::set_permissions(bin.path().join("cargo"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        for stage in ["workspace", "acceptance", "features", "schema"] {
            let _ = std::fs::remove_file(&log);
            let o = Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
                .args(["gate", "--stage", stage])
                .current_dir(&r.root)
                .env("CARGO_TARGET_DIR", pool.path())
                .env("PATH", &path)
                .output().unwrap();
            assert_eq!(o.status.success(), code == 0, "{stage}: {}", stderr(&o));
            let own = pool.path().join("contextful-ci").join(stage);
            let calls = std::fs::read_to_string(&log).unwrap();
            assert!(!calls.is_empty(), "{stage} ran no build");
            for line in calls.lines() {
                assert_eq!(std::path::Path::new(line.split_whitespace().next().unwrap()), own);
            }
            assert_eq!(own.join("built").exists(), code != 0, "{stage} cleanup");
            assert!(marker.exists(), "cleanup removed the enclosing pool");
            assert!(!r.root.join("target").exists(), "{stage} wrote into the checkout");
        }
    }
}

/// Each cargo stage builds into a target directory of its own, reclaimed once the stage passes.
// spec: assurance.build.target-dir-per-stage@e2f83b4f
#[test]
fn each_cargo_stage_builds_in_its_own_target_directory_reclaimed_on_pass() {
    let r = Repo::init();
    with_acceptance(&r);
    // A package declaring a feature, so the features stage runs cargo.
    r.write("crates/featured/Cargo.toml", &(manifest("featured", "") + "\n[features]\nextra = []\n"));
    r.write("crates/featured/src/lib.rs", "");
    r.write("crates/featured/tests/integration/main.rs", "");
    r.commit("featured package");
    let log = r.root.join("cargo.log");
    let target = r.root.canonicalize().unwrap().join("target");
    for stage in ["workspace", "acceptance", "features", "schema"] {
        let _ = std::fs::remove_file(&log);
        let o = r.gate_with_cargo(&recording(&log, 0), &["--stage", stage]);
        assert!(o.status.success(), "{stage}: {}", stderr(&o));
        let own = target.join(stage);
        let ran = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(!ran.is_empty(), "{stage} ran no cargo build");
        for line in ran.lines() {
            let dir = std::path::Path::new(line.split_whitespace().next().unwrap_or_default());
            let parent = dir.parent().and_then(|p| p.canonicalize().ok());
            let recorded = parent.zip(dir.file_name()).map(|(p, n)| p.join(n));
            assert_eq!(recorded.as_deref(), Some(own.as_path()), "{stage} ran `{line}` outside its own target directory");
        }
        assert!(!own.exists(), "{stage} passed and left {}", own.display());
    }

    let _ = std::fs::remove_file(&log);
    let failed = r.gate_with_cargo(&recording(&log, 1), &["--stage", "workspace"]);
    assert!(!failed.status.success());
    assert!(r.root.join("target/workspace/built").exists(), "a failing stage reclaimed its directory");
}

/// The workspace stage builds every engine-linked package in one cargo invocation over the union of their features, and the store adapter's suites resolved without `read` link no SQL engine, so the features stage compiles one copy.
// spec: assurance.build.one-engine-build@b60a26c3
#[test]
fn the_workspace_stage_runs_one_invocation_and_the_store_suites_link_no_engine_without_read() {
    let r = Repo::init();
    with_acceptance(&r);
    let log = r.root.join("cargo.log");
    let o = r.gate_with_cargo(&recording(&log, 0), &["--stage", "workspace"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let ran = std::fs::read_to_string(&log).unwrap();
    let invocations: Vec<&str> = ran.lines().map(|l| l.split_once(' ').map(|(_, a)| a).unwrap_or_default()).collect();
    assert_eq!(invocations, ["test --workspace --exclude contextful-acceptance"]);

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let tree = Command::new(&cargo)
        .args(["tree", "--locked", "-p", "contextful-context", "--no-default-features", "-e", "normal,dev", "--prefix", "none"])
        .current_dir(repo_root())
        .output()
        .unwrap();
    assert!(tree.status.success(), "{}", stderr(&tree));
    let tree = String::from_utf8_lossy(&tree.stdout);
    for engine in ["duckdb ", "libduckdb-sys "] {
        assert!(!tree.is_empty(), "the exclusion below ranges over no element");
        assert!(!tree.lines().any(|l| l.starts_with(engine)), "the store adapter's suites link `{engine}` without `read`");
    }
}

/// Workspace compilation and both CLI partitions run separately; four groups run every other package.
// spec: assurance.gate.workspace-parts@4ac089c3
#[test]
fn remote_workspace_parts_compile_the_union_and_run_each_package_suite() {
    let r = Repo::init();
    with_acceptance(&r);
    for name in ["sample", "contextful-engine", "contextful-context", "contextful-cli", "contextful-ci"] {
        r.write(&format!("crates/{name}/Cargo.toml"), &manifest(name, ""));
        r.write(&format!("crates/{name}/src/lib.rs"), "");
        r.write(&format!("crates/{name}/tests/integration/main.rs"), "");
    }
    r.lock();
    r.commit("one workspace package");
    let log = r.root.join("cargo.log");

    let compiled = r.gate_with_cargo(&recording(&log, 0), &["--stage", "workspace.compile"]);
    assert!(compiled.status.success(), "{}", stderr(&compiled));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("test --workspace --exclude contextful-acceptance --no-run"), "{calls}");
    assert_eq!(calls.lines().count(), 1, "compile runs no package suite: {calls}");

    std::fs::remove_file(&log).unwrap();
    let tested = r.gate_with_cargo(&recording(&log, 0), &["--stage", "workspace.cli"]);
    assert!(tested.status.success(), "{}", stderr(&tested));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("test --package contextful-cli"), "{calls}");
    assert!(calls.contains("--skip differential::"), "{calls}");
    assert_eq!(calls.lines().count(), 1, "CLI runs no workspace compile: {calls}");

    std::fs::remove_file(&log).unwrap();
    let formal = r.gate_with_cargo(&recording(&log, 0), &["--stage", "workspace.cli-formal"]);
    assert!(formal.status.success(), "{}", stderr(&formal));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("test --package contextful-cli --test integration differential::"), "{calls}");
    assert_eq!(calls.lines().count(), 1, "formal runs no workspace compile: {calls}");

    std::fs::remove_file(&log).unwrap();
    let tested = r.gate_with_cargo(&recording(&log, 0), &["--stage", "workspace.foundation"]);
    assert!(tested.status.success(), "{}", stderr(&tested));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("--package sample") && calls.contains("--package demo"), "{calls}");
    for name in ["contextful-acceptance", "contextful-engine", "contextful-context", "contextful-cli", "contextful-ci"] {
        assert!(!calls.contains(name), "{calls}");
    }
    for (part, package) in [("runtime", "contextful-engine"), ("read", "contextful-context")] {
        std::fs::remove_file(&log).unwrap();
        let tested = r.gate_with_cargo(&recording(&log, 0), &["--stage", &format!("workspace.{part}")]);
        assert!(tested.status.success(), "{part}: {}", stderr(&tested));
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(calls.contains(&format!("test --package {package}")), "{part}: {calls}");
        assert_eq!(calls.matches("--package").count(), 1, "{part}: {calls}");
    }
    std::fs::remove_file(&log).unwrap();
    let tested = r.gate_with_cargo(&recording(&log, 0), &["--stage", "workspace.ci"]);
    assert!(tested.status.success(), "{}", stderr(&tested));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("test --package contextful-ci"), "{calls}");
    assert_eq!(calls.matches("--package").count(), 1, "{calls}");
}

/// Development and test profiles carry line-tables-only debug information for workspace code and none for dependencies.
// spec: assurance.build.debug-info@94fc28bf
#[test]
fn development_and_test_builds_carry_line_tables_only() {
    let text = std::fs::read_to_string(repo_root().join("Cargo.toml")).unwrap();
    let doc: toml::Value = toml::from_str(&text).unwrap();
    let profile = &doc["profile"];
    assert_eq!(profile["dev"]["debug"].as_str(), Some("line-tables-only"));
    assert_eq!(profile["dev"]["package"]["*"]["debug"].as_bool(), Some(false));
    // The test profile inherits the development profile; an override of its own would differ.
    if let Some(test) = profile.get("test") {
        assert!(test.get("debug").is_none() && test.get("package").is_none(), "the test profile overrides debug information: {test:?}");
    }
}
