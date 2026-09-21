//! Milestone 0 — the test-first gate.
//!
//! Reach: a change altering source with no test failing against its base reds the gate
//! before merge.

use contextful_acceptance::{bin, GitRepo};

fn crate_with_double() -> GitRepo {
    let r = GitRepo::init();
    r.write("Cargo.toml", "[workspace]\nresolver = \"2\"\nmembers = [\"crates/*\"]\n");
    r.write(
        "crates/calc/Cargo.toml",
        "[package]\nname = \"calc\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[[test]]\nname = \"integration\"\npath = \"tests/integration/main.rs\"\n",
    );
    r.write("crates/calc/src/lib.rs", "pub fn double(x: i32) -> i32 {\n    x * 2\n}\n");
    r.write("crates/calc/tests/integration/main.rs", "#[test]\nfn doubles() {\n    assert_eq!(calc::double(3), 6);\n}\n");
    r.write(".gitignore", "/target\n");
    r
}

#[test]
fn m00_test_first_gate() {
    let ci = bin("contextful-ci");
    let r = crate_with_double();
    let base = r.commit("base");

    r.write("crates/calc/src/lib.rs", "pub fn double(x: i32) -> i32 {\n    x * 2\n}\n\npub fn half(x: i32) -> i32 {\n    x / 2\n}\n");
    r.commit("half, untested");
    let untested = r.run(&ci, &["gate", "--stage", "test-first", "--base", &base]);
    assert!(!untested.status.success(), "a source change with no test passed the gate");
    assert!(String::from_utf8_lossy(&untested.stderr).contains("TestNotFirst"));

    r.write(
        "crates/calc/tests/integration/main.rs",
        "#[test]\nfn doubles() {\n    assert_eq!(calc::double(3), 6);\n}\n\n#[test]\nfn halves() {\n    assert_eq!(calc::half(6), 3);\n}\n",
    );
    r.commit("half's test");
    let tested = r.run(&ci, &["gate", "--stage", "test-first", "--base", &base]);
    assert!(tested.status.success(), "{}", String::from_utf8_lossy(&tested.stderr));
}
