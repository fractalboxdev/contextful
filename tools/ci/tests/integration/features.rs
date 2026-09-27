//! `assurance.build.staged-feature-runs`: the features stage runs the store adapter with its
//! read face off, a combination the workspace stage's unified build never reaches.

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
