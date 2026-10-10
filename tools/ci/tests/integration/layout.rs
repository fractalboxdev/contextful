//! Test placement: one integration binary per package, a feature-gated suite gated at the
//! head of its file, and a top-level test file stating why it needs its own process.

use crate::{manifest, repo_root, stderr, Repo};
use std::process::Command;

fn layout(r: &Repo) -> std::process::Output {
    r.run_ci(&["test-layout"])
}

// spec: assurance.test.one-integration-binary@ac373ca9
#[test]
fn a_second_test_target_or_a_package_without_the_integration_root_is_refused() {
    let r = Repo::init();
    let o = layout(&r);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).contains("1 package(s) hold one integration target each"), "{}", stderr(&o));

    r.write("crates/demo/Cargo.toml", &format!("{}\n[[test]]\nname = \"bench\"\npath = \"tests/integration/bench.rs\"\n", manifest("demo", "")));
    let err = stderr(&layout(&r));
    assert!(err.contains("crates/demo/Cargo.toml declares test target `bench`"), "{err}");

    let r = Repo::init();
    r.write("crates/bare/Cargo.toml", &manifest("bare", ""));
    r.write("crates/bare/tests/integration/suite.rs", "#[test]\nfn runs() {}\n");
    let err = stderr(&layout(&r));
    assert!(err.contains("crates/bare has tests/ but no tests/integration/main.rs"), "{err}");
}

// spec: assurance.test.feature-gated-suite@3ef84c96
#[test]
fn a_suite_gated_only_at_its_declaration_is_refused() {
    let r = Repo::init();
    r.write("crates/demo/tests/integration/main.rs", "mod double;\n#[cfg(feature = \"extra\")]\nmod extra;\n");
    r.write("crates/demo/tests/integration/extra.rs", "//! The extra suite.\n#[test]\nfn extra() {}\n");
    let err = stderr(&layout(&r));
    assert!(err.contains("suite `extra` is feature-gated at its declaration"), "{err}");

    r.write("crates/demo/tests/integration/extra.rs", "//! The extra suite.\n#![cfg(feature = \"extra\")]\n#[test]\nfn extra() {}\n");
    let o = layout(&r);
    assert!(o.status.success(), "{}", stderr(&o));
}

// spec: assurance.test.own-process@d2179d55
#[test]
fn a_top_level_test_file_states_why_it_needs_its_own_process() {
    let r = Repo::init();
    r.write("crates/demo/tests/capture.rs", "//! Captures the global logger.\n#[test]\nfn captures() {}\n");
    let err = stderr(&layout(&r));
    assert!(err.contains("crates/demo/tests/capture.rs is a top-level test file"), "{err}");

    r.write("crates/demo/tests/capture.rs", "//! Runs in its own process: the thread-scoped log capture installs a global logger.\n#[test]\nfn captures() {}\n");
    let o = layout(&r);
    assert!(o.status.success(), "{}", stderr(&o));
}

// spec: assurance.test.global-state-lock@7953bb8a
#[test]
fn a_lock_over_process_global_state_lives_in_the_integration_root() {
    let r = Repo::init();
    r.write("crates/demo/tests/integration/double.rs", "static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());\n#[test]\nfn doubles() {\n    let _held = ENV.lock().unwrap();\n    assert_eq!(demo::double(2), 4);\n}\n");
    let err = stderr(&layout(&r));
    assert!(err.contains("crates/demo/tests/integration/double.rs:1: a lock over process-global state lives outside"), "{err}");

    r.write("crates/demo/tests/integration/main.rs", "mod double;\n\npub static ENV: std::sync::RwLock<()> = std::sync::RwLock::new(());\n");
    r.write("crates/demo/tests/integration/double.rs", "#[test]\nfn doubles() {\n    let _held = crate::ENV.read().unwrap();\n    assert_eq!(demo::double(2), 4);\n}\n");
    let o = layout(&r);
    assert!(o.status.success(), "{}", stderr(&o));
}

#[test]
fn the_live_tree_keeps_its_test_placement() {
    let o = Command::new(env!("CARGO_BIN_EXE_contextful-ci")).arg("test-layout").current_dir(repo_root()).output().unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    let err = stderr(&o);
    let held: usize = err.split(" package(s)").next().and_then(|s| s.rsplit(' ').next()).and_then(|n| n.parse().ok()).unwrap_or(0);
    assert!(held >= 10, "{err}");
}
