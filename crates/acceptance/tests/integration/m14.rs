//! Milestone 14 — assurance.
//!
//! Reach: the gate holds its resource budget and the evaluation floors are measured on
//! every change.

use contextful_acceptance::{bin, GitRepo};

#[test]
#[ignore = "milestone 14 is open: the gate carries no budget stage"]
fn m14_assurance() {
    let ci = bin("contextful-ci");
    let r = GitRepo::init();
    r.write(".env", "# demo token — read on the demo repository\nTOKEN=plaintext\n");
    r.commit("a plaintext secret of record");
    let schema = r.run(&ci, &["secrets"]);
    assert!(!schema.status.success(), "a plaintext secret of record passed the schema checks");
    assert!(String::from_utf8_lossy(&schema.stderr).contains("SecretPlaintext"));

    let stages = r.run(&ci, &["stages"]);
    let stages = String::from_utf8_lossy(&stages.stdout);
    for stage in ["pins", "toolchain", "schema", "test-first", "workspace", "acceptance", "crate-graph", "formal", "budget"] {
        assert!(stages.lines().any(|s| s == stage), "the gate defines no `{stage}` stage");
    }
}
