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
        assert!(!tree.lines().any(|l| l.starts_with(engine)), "the store adapter's suites link `{engine}` without `read`");
    }
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
