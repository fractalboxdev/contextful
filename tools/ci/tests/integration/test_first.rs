//! `assurance.test.test-first` and `assurance.test.refactor-trailer`.

use crate::{stderr, Repo};

const TRIPLE: &str = "pub fn double(x: i32) -> i32 {\n    x * 2\n}\n\npub fn triple(x: i32) -> i32 {\n    x * 3\n}\n";

#[test]
fn a_change_touching_no_source_passes() {
    let r = Repo::init();
    let base = r.head();
    r.write("README.md", "demo\n");
    r.commit("docs");
    let o = r.gate(&["--stage", "test-first", "--base", &base]);
    assert!(o.status.success(), "{}", stderr(&o));
}

#[test]
fn source_change_without_test_is_refused() {
    let r = Repo::init();
    let base = r.head();
    r.write("crates/demo/src/lib.rs", TRIPLE);
    r.commit("triple");
    let o = r.gate(&["--stage", "test-first", "--base", &base]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("TestNotFirst"), "{}", stderr(&o));
}

#[test]
fn a_rust_change_outside_src_without_test_is_refused() {
    for path in ["crates/demo/build.rs", "crates/demo/benches/speed.rs", "crates/demo/examples/show.rs"] {
        let r = Repo::init();
        let base = r.head();
        r.write(path, "fn main() {}\n");
        r.commit("a Rust file outside src/");
        let o = r.gate(&["--stage", "test-first", "--base", &base]);
        assert!(!o.status.success(), "{path} passed with no test");
        assert!(stderr(&o).contains("TestNotFirst"), "{path}: {}", stderr(&o));
    }
}

#[test]
fn a_test_already_green_on_the_base_is_refused() {
    let r = Repo::init();
    let base = r.head();
    r.write("crates/demo/src/lib.rs", TRIPLE);
    r.write(
        "crates/demo/tests/integration/double.rs",
        "#[test]\nfn doubles() {\n    assert_eq!(demo::double(2), 4);\n    assert_eq!(demo::double(0), 0);\n}\n",
    );
    r.commit("triple, with a test that specifies nothing new");
    let o = r.gate(&["--stage", "test-first", "--base", &base]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("TestNotFirst"), "{}", stderr(&o));
}

#[test]
fn a_test_failing_on_the_base_passes() {
    let r = Repo::init();
    let base = r.head();
    r.write("crates/demo/src/lib.rs", TRIPLE);
    r.write("crates/demo/tests/integration/main.rs", "mod double;\nmod triple;\n");
    r.write(
        "crates/demo/tests/integration/triple.rs",
        "#[test]\nfn triples() {\n    assert_eq!(demo::triple(2), 6);\n}\n",
    );
    r.commit("triple, test first");
    let o = r.gate(&["--stage", "test-first", "--base", &base]);
    assert!(o.status.success(), "{}", stderr(&o));
}

#[test]
fn a_refactor_trailer_exempts_the_range() {
    let r = Repo::init();
    let base = r.head();
    r.write("crates/demo/src/lib.rs", "pub fn double(x: i32) -> i32 {\n    x + x\n}\n");
    r.commit("double by addition\n\nTest-First: refactor");
    let o = r.gate(&["--stage", "test-first", "--base", &base]);
    assert!(o.status.success(), "{}", stderr(&o));
}

#[test]
fn a_refactor_commit_does_not_exempt_the_rest_of_the_range() {
    let r = Repo::init();
    let base = r.head();
    r.write("crates/demo/src/lib.rs", "pub fn double(x: i32) -> i32 {\n    x + x\n}\n");
    r.commit("double by addition\n\nTest-First: refactor");
    r.write("crates/demo/src/lib.rs", "pub fn double(x: i32) -> i32 {\n    x + x\n}\n\npub fn triple(x: i32) -> i32 {\n    x * 3\n}\n");
    r.commit("triple, untested");
    let o = r.gate(&["--stage", "test-first", "--base", &base]);
    assert!(!o.status.success(), "an untested commit rode on a refactor trailer");
    assert!(stderr(&o).contains("TestNotFirst"), "{}", stderr(&o));
}

#[test]
fn a_new_package_counts_red_alone_and_leaves_the_base_workspace_loadable() {
    let r = Repo::init();
    let base = r.head();
    // A new package with its own tests, beside a green-on-base test for a source change in `demo`.
    r.write("crates/fresh/Cargo.toml", &crate::manifest("fresh", ""));
    r.write("crates/fresh/src/lib.rs", "pub fn one() -> i32 {\n    1\n}\n");
    r.write("crates/fresh/tests/integration/main.rs", "#[test]\nfn one() {\n    assert_eq!(fresh::one(), 1);\n}\n");
    r.write("crates/demo/src/lib.rs", TRIPLE);
    r.write("crates/demo/tests/integration/double.rs", "#[test]\nfn doubles() {\n    assert_eq!(demo::double(3), 6);\n}\n");
    r.commit("a new package, and a demo test that specifies nothing new");
    let o = r.gate(&["--stage", "test-first", "--base", &base]);
    let err = stderr(&o);
    assert!(o.status.success(), "{err}");
    assert!(err.contains("crates/fresh (absent at base)"), "{err}");
    assert!(!err.contains("crates/demo"), "demo's test passes at base and counts green: {err}");
    assert!(!err.contains("failed to load manifest"), "{err}");
}
