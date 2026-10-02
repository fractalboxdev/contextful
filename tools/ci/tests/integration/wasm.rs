//! The gate provisions the `wasm32-unknown-unknown` target the decision module's
//! WebAssembly build compiles for, and holds WebAssembly-backed tests to running rather
//! than skipping.

use crate::{stderr, Repo};
use std::process::Command;

/// A test that passes only when the gate provisioned the target and asked for it.
const NEEDS_WASM: &str = r#"#[test]
fn wasm_is_provisioned() {
    assert_eq!(std::env::var("CONTEXTFUL_REQUIRE_WASM").as_deref(), Ok("1"));
    let out = std::process::Command::new("rustc")
        .args(["--print", "target-libdir", "--target", "wasm32-unknown-unknown"])
        .output()
        .expect("rustc on PATH");
    let dir = String::from_utf8(out.stdout).unwrap();
    assert!(std::fs::read_dir(dir.trim()).expect("the target's library directory").next().is_some());
}
"#;

#[test]
fn the_workspace_stage_runs_its_tests_with_the_webassembly_target_required() {
    let r = Repo::init();
    r.write("crates/demo/tests/integration/main.rs", "mod double;\nmod wasm;\n");
    r.write("crates/demo/tests/integration/wasm.rs", NEEDS_WASM);
    r.commit("a test needing the WebAssembly target");
    let o = Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .args(["gate", "--stage", "workspace"])
        .current_dir(&r.root)
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CONTEXTFUL_REQUIRE_WASM")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
}
