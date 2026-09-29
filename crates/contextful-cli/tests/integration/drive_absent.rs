//! The Google Drive source on a binary built without the `drive` feature.
#![cfg(not(feature = "drive"))]

use std::process::Command;

/// The names resolving to compiled-in sources form one enumerated list. A listed name carries no manifest or
/// artifact, and a feature-gated entry stays listed and answers with a rebuild hint.
// spec: connector.package.built-in-registry@229f53b6
#[test]
fn a_compiled_out_drive_source_answers_with_the_feature_to_rebuild_with() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::write(dir.path().join("contextful.toml"), "[[pipeline]]\nid = \"team\"\ntables = [\"files\"]\n\n[pipeline.source]\nname = \"drive\"\n").unwrap();
    for args in [&["pipeline", "validate"][..], &["pipeline", "run", "team", "--project", "research", "--run-id", "run-1", "--site-id", "site-a"][..]] {
        let out = Command::new(env!("CARGO_BIN_EXE_contextful")).args(args).current_dir(dir.path()).env_remove("CONTEXTFUL_NODE_ID").output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{args:?} ran a compiled-out source");
        assert!(stderr.contains("source `drive` is compiled out of this build; rebuild with `--features drive`"), "{args:?}: {stderr}");
    }
    // An unlisted name is answered with the list, the compiled-out entry on it.
    std::fs::write(dir.path().join("contextful.toml"), "[[pipeline]]\nid = \"team\"\ntables = [\"files\"]\n\n[pipeline.source]\nname = \"gdrive\"\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_contextful")).args(["pipeline", "validate"]).current_dir(dir.path()).env_remove("CONTEXTFUL_NODE_ID").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("http, derive, drive"), "{}", String::from_utf8_lossy(&out.stderr));
}
