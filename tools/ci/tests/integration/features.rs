//! `assurance.build.staged-feature-runs`: the features stage runs each package declaring a
//! feature with no features and with every feature, combinations the workspace stage's
//! unified build never reaches: the store adapter with its read face off among them.

use crate::{manifest, stderr, Repo};

/// A store adapter whose one write-half test passes only while `read` is off, recording
/// each run in `ran-without-read` at the repository root.
fn store_adapter(repo: &Repo) {
    let m = manifest("contextful-context", "").replace(
        "[[test]]",
        "[features]\ndefault = [\"read\"]\nread = []\n\n[[test]]",
    );
    repo.write("crates/contextful-context/Cargo.toml", &m);
    repo.write("crates/contextful-context/src/lib.rs", "pub fn reads() -> bool {\n    cfg!(feature = \"read\")\n}\n");
    repo.write("crates/contextful-context/tests/integration/main.rs", "mod land;\n");
    repo.write(
        "crates/contextful-context/tests/integration/land.rs",
        "#[test]\nfn lands_without_read() {\n    if contextful_context::reads() {\n        return;\n    }\n    \
         std::fs::write(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../ran-without-read\"), \"\").unwrap();\n}\n",
    );
    repo.commit("store adapter");
}

/// A defect appearing under one feature combination alone is reached by a staged run, one command per container.
// spec: assurance.build.staged-feature-runs@2e9106bf
#[test]
fn the_features_stage_runs_the_store_adapter_with_read_off() {
    let repo = Repo::init();
    store_adapter(&repo);
    let marker = repo.root.join("ran-without-read");

    let workspace = repo.gate(&["--stage", "workspace"]);
    assert!(workspace.status.success(), "{}", stderr(&workspace));
    assert!(!marker.exists(), "the workspace stage ran the store adapter with read off");

    let features = repo.gate(&["--stage", "features"]);
    assert!(features.status.success(), "{}", stderr(&features));
    assert!(marker.exists(), "the features stage did not run the store adapter with read off");
    assert!(!repo.root.join("target/features").exists(), "the features stage left its target directory behind");
}

#[test]
fn a_defect_under_read_off_alone_fails_the_features_stage() {
    let repo = Repo::init();
    store_adapter(&repo);
    repo.write(
        "crates/contextful-context/tests/integration/land.rs",
        "#[test]\nfn lands_without_read() {\n    assert!(contextful_context::reads(), \"the write half needs read\");\n}\n",
    );
    repo.commit("a write half that needs read");

    let workspace = repo.gate(&["--stage", "workspace"]);
    assert!(workspace.status.success(), "{}", stderr(&workspace));
    let features = repo.gate(&["--stage", "features"]);
    assert!(!features.status.success(), "the features stage passed a defect present only with read off");
}

/// `crates/gated`, whose `extra` feature gates `lib_extra` into the library and `test_extra`
/// into the integration suite.
fn gated(r: &Repo, lib_extra: &str, test_extra: &str) {
    r.write("crates/gated/Cargo.toml", &format!("{}\n[features]\nextra = []\n", manifest("gated", "")));
    r.write(
        "crates/gated/src/lib.rs",
        &format!("#[cfg(feature = \"extra\")]\npub mod extra {{\n    pub const X: u8 = 1;\n}}\n{lib_extra}"),
    );
    r.write("crates/gated/tests/integration/main.rs", &format!("#[test]\nfn builds() {{}}\n{test_extra}"));
}

fn features(r: &Repo) -> std::process::Output {
    r.gate(&["--stage", "features"])
}

/// Each package declaring a feature runs its suite twice, with no features and with all.
#[test]
fn the_features_stage_tests_each_featured_package_with_none_and_with_all() {
    let r = Repo::init();
    gated(&r, "", "#[cfg(feature = \"extra\")]\n#[test]\nfn extra() {\n    assert_eq!(gated::extra::X, 1);\n}\n");
    let o = features(&r);
    let err = stderr(&o);
    assert!(o.status.success(), "{err}");
    assert!(err.contains("features: gated --no-default-features"), "{err}");
    assert!(err.contains("features: gated --all-features"), "{err}");
    assert!(!err.contains("features: demo"), "a package declaring no feature is tested once, in the workspace stage: {err}");
}

#[test]
fn a_default_build_reaching_feature_gated_code_reds_the_features_stage() {
    let r = Repo::init();
    gated(&r, "pub fn x() -> u8 {\n    extra::X\n}\n", "");
    let o = features(&r);
    let err = stderr(&o);
    assert!(!o.status.success(), "a library reaching a gated module compiles without the feature: {err}");
    assert!(err.contains("`cargo test -p gated --no-default-features` exited"), "{err}");
}

#[test]
fn a_failing_feature_gated_test_reds_the_features_stage() {
    let r = Repo::init();
    gated(&r, "", "#[cfg(feature = \"extra\")]\n#[test]\nfn extra() {\n    assert_eq!(gated::extra::X, 2);\n}\n");
    let o = features(&r);
    let err = stderr(&o);
    assert!(!o.status.success(), "{err}");
    assert!(err.contains("`cargo test -p gated --all-features` exited"), "{err}");
}

/// `crates/gated` with the profile bundle `contextful-edge = ["edge"]`, whose library holds
/// `lib_edge` only while `edge` is on without `extra`, and whose manifest lists `runs` under
/// `feature-runs`.
fn bundled(r: &Repo, lib_edge: &str, runs: &str) {
    r.write(
        "crates/gated/Cargo.toml",
        &format!(
            "{}\n[features]\nextra = []\nedge = []\ncontextful-edge = [\"edge\"]\n\n[package.metadata.contextful]\nfeature-runs = [{runs}]\n",
            manifest("gated", "")
        ),
    );
    r.write("crates/gated/src/lib.rs", &format!("#[cfg(all(feature = \"edge\", not(feature = \"extra\")))]\n{lib_edge}\n"));
    r.write("crates/gated/tests/integration/main.rs", "#[test]\nfn builds() {}\n");
}

/// The features stage builds the binary under each profile bundle alone and tests each package under every feature set its manifest lists in `feature-runs`; a failing build or test reds the stage.
// spec: assurance.build.profile-build@4e7803a1
#[test]
fn the_features_stage_builds_each_profile_bundle_alone_and_tests_each_listed_run() {
    let r = Repo::init();
    bundled(&r, "pub fn edge() {}", "\"edge\"");
    let o = features(&r);
    let err = stderr(&o);
    assert!(o.status.success(), "{err}");
    assert!(err.contains("features: gated build --no-default-features --features contextful-edge"), "{err}");
    assert!(err.contains("features: gated --no-default-features --features edge"), "{err}");
    assert!(!err.contains("--features contextful-full"), "a bundle the package does not declare is built: {err}");

    // A defect reached by the bundle alone reds the stage, though no and all features pass.
    bundled(&r, "compile_error!(\"the edge bundle alone\");", "");
    let o = features(&r);
    let err = stderr(&o);
    assert!(!o.status.success(), "{err}");
    assert!(err.contains("`cargo build -p gated --no-default-features --features contextful-edge` exited"), "{err}");
}

#[test]
fn a_failing_listed_feature_run_reds_the_features_stage() {
    let r = Repo::init();
    bundled(&r, "pub fn edge() {}", "\"edge\"");
    r.write("crates/gated/tests/integration/main.rs", "#[test]\nfn builds() {\n    assert!(!cfg!(feature = \"edge\") || cfg!(feature = \"extra\"), \"edge alone\");\n}\n");
    let o = features(&r);
    let err = stderr(&o);
    assert!(!o.status.success(), "{err}");
    assert!(err.contains("`cargo test -p gated --no-default-features --features edge` exited"), "{err}");
}
