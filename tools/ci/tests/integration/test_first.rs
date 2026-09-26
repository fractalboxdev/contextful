//! `assurance.test.test-first`, its scope, bound and unrunnable base, and `assurance.test.refactor-trailer`.

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

/// The test-first stage builds each changed test file's target against the base source, then runs exactly the
/// tests under that file's top-level module; a target failing to compile there counts as failing.
// spec: assurance.test.test-first-scope@6055ef81
#[test]
fn only_the_changes_test_modules_run_against_the_base() {
    let r = Repo::init();
    // The base carries a suite that already fails there; the change does not touch it.
    r.write("crates/demo/tests/integration/main.rs", "mod double;\nmod stale;\n");
    r.write("crates/demo/tests/integration/stale.rs", "#[test]\nfn stale() {\n    assert_eq!(demo::double(2), 5);\n}\n");
    r.commit("a base whose stale suite fails");
    let base = r.head();
    r.write("crates/demo/src/lib.rs", TRIPLE);
    r.write("crates/demo/tests/integration/double.rs", "#[test]\nfn doubles() {\n    assert_eq!(demo::double(5), 10);\n}\n");
    r.commit("triple, with a double test green at base");
    let o = r.gate(&["--stage", "test-first", "--base", &base]);
    assert!(!o.status.success(), "the base's unrelated failing suite read as the change's red test: {}", stderr(&o));
    assert!(stderr(&o).contains("TestNotFirst"), "{}", stderr(&o));
}

/// One test-first execution against the base, its build excluded, runs for at most 300 s; a run past the bound is
/// killed with its process group and counts as failing.
// spec: assurance.test.base-run-bound@f1d1a672
#[test]
fn a_base_run_past_its_bound_is_killed_and_counts_red() {
    let r = Repo::init();
    r.write("crates/demo/src/lib.rs", "pub fn double(x: i32) -> i32 {\n    x * 2\n}\n\npub fn ready() -> bool {\n    false\n}\n");
    r.commit("a base that is never ready");
    let base = r.head();
    r.write("crates/demo/src/lib.rs", "pub fn double(x: i32) -> i32 {\n    x * 2\n}\n\npub fn ready() -> bool {\n    true\n}\n");
    r.write("crates/demo/tests/integration/main.rs", "mod double;\nmod ready;\n");
    r.write(
        "crates/demo/tests/integration/ready.rs",
        "#[test]\nfn settles() {\n    while !demo::ready() {\n        std::thread::sleep(std::time::Duration::from_millis(10));\n    }\n}\n",
    );
    r.commit("ready");
    let started = std::time::Instant::now();
    let o = r.gate(&["--stage", "test-first", "--base", &base, "--base-bound-secs", "20"]);
    let err = stderr(&o);
    assert!(o.status.success(), "{err}");
    assert!(err.contains("20 s") && err.contains("crates/demo"), "{err}");
    assert!(started.elapsed() < std::time::Duration::from_secs(120), "{:?}", started.elapsed());
}

/// A base invocation whose output reports a full disk, an unloadable manifest or an unfetchable dependency raises
/// `TestFirstBaseUnrunnable` instead of a verdict.
// spec: assurance.test.base-unrunnable@60e4830e
#[test]
fn an_unloadable_base_fails_the_stage_instead_of_reading_red() {
    let r = Repo::init();
    r.write("crates/demo/Cargo.toml", "[package\nname = \"demo\"\n");
    r.commit("a base whose manifest does not parse");
    let base = r.head();
    r.write("crates/demo/Cargo.toml", &crate::manifest("demo", ""));
    r.write("crates/demo/src/lib.rs", TRIPLE);
    r.write("crates/demo/tests/integration/main.rs", "mod double;\nmod triple;\n");
    r.write("crates/demo/tests/integration/triple.rs", "#[test]\nfn triples() {\n    assert_eq!(demo::triple(2), 6);\n}\n");
    r.commit("triple, test first");
    let o = r.gate(&["--stage", "test-first", "--base", &base]);
    let err = stderr(&o);
    assert!(!o.status.success(), "an infrastructure failure read as red: {err}");
    assert!(err.contains("TestFirstBaseUnrunnable") && err.contains("manifest"), "{err}");
}

#[test]
fn a_test_quoting_an_infrastructure_fault_still_reads_as_red() {
    let r = Repo::init();
    let base = r.head();
    r.write("crates/demo/src/lib.rs", TRIPLE);
    r.write(
        "crates/demo/tests/integration/double.rs",
        "#[test]\nfn doubles() {\n    assert_eq!(demo::double(2), 5, \"No space left on device; failed to load manifest\");\n}\n",
    );
    r.commit("a demo test failing at base with a message that quotes infrastructure faults");
    let o = r.gate(&["--stage", "test-first", "--base", &base]);
    let err = stderr(&o);
    assert!(o.status.success(), "{err}");
    assert!(err.contains("red against the base in crates/demo"), "{err}");
}

#[test]
fn a_slow_base_build_is_not_charged_to_the_bound() {
    let r = Repo::init();
    // The base's build script outlasts the bound; only test execution is bounded.
    r.write("crates/demo/build.rs", "fn main() {\n    std::thread::sleep(std::time::Duration::from_secs(8));\n}\n");
    r.commit("a slow base build");
    let base = r.head();
    r.write("crates/demo/src/lib.rs", "pub fn double(x: i32) -> i32 {\n    x * 2\n}\n\npub fn triple(x: i32) -> i32 {\n    x * 3\n}\n");
    r.write("crates/demo/tests/integration/double.rs", "#[test]\nfn doubles() {\n    assert_eq!(demo::double(4), 8);\n}\n");
    r.commit("triple, with a double test green at base");
    let o = r.gate(&["--stage", "test-first", "--base", &base, "--base-bound-secs", "3"]);
    let err = stderr(&o);
    assert!(!o.status.success(), "a build killed at the bound read as red: {err}");
    assert!(err.contains("TestNotFirst"), "{err}");
}

#[test]
fn a_compile_error_quoting_a_manifest_fault_still_reads_as_red() {
    let r = Repo::init();
    let base = r.head();
    r.write("crates/demo/src/lib.rs", TRIPLE);
    r.write("crates/demo/tests/integration/main.rs", "mod double;\nmod triple;\n");
    r.write(
        "crates/demo/tests/integration/triple.rs",
        "#[test]\nfn triples() {\n    assert_eq!(demo::triple(2), 6, \"failed to load manifest; No space left on device\");\n}\n",
    );
    r.commit("triple, whose test line quotes cargo faults");
    let o = r.gate(&["--stage", "test-first", "--base", &base]);
    let err = stderr(&o);
    assert!(o.status.success(), "{err}");
    assert!(err.contains("red against the base in crates/demo"), "{err}");
}

#[test]
fn a_module_filter_selects_that_module_alone() {
    let r = Repo::init();
    // `redouble` fails at base and shares the suffix `double::` with the changed module.
    r.write("crates/demo/tests/integration/main.rs", "mod double;\nmod redouble;\n");
    r.write("crates/demo/tests/integration/redouble.rs", "#[test]\nfn stale() {\n    assert_eq!(demo::double(2), 5);\n}\n");
    r.commit("a base whose redouble suite fails");
    let base = r.head();
    r.write("crates/demo/src/lib.rs", TRIPLE);
    r.write("crates/demo/tests/integration/double.rs", "#[test]\nfn doubles() {\n    assert_eq!(demo::double(6), 12);\n}\n");
    r.commit("triple, with a double test green at base");
    let o = r.gate(&["--stage", "test-first", "--base", &base]);
    let err = stderr(&o);
    assert!(!o.status.success(), "`redouble::` ran as the change's test: {err}");
    assert!(err.contains("TestNotFirst"), "{err}");
}
