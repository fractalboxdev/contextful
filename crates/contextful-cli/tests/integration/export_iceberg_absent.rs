//! An Iceberg export on a binary built without the `iceberg-export` feature.
#![cfg(not(feature = "iceberg-export"))]

use std::process::Command;

/// An Iceberg block parses on every data-plane build, and a build without `iceberg-export` refuses to run it with
/// the feature to rebuild with, before any credential is admitted or any request leaves.
#[test]
fn a_compiled_out_iceberg_export_answers_with_the_feature_to_rebuild_with() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::write(
        dir.path().join("contextful.toml"),
        "[[export]]\nname = \"spans-lake\"\ntable = \"spans\"\nformat = \"iceberg\"\ncredential = \"svc://mirror\"\n\
         catalog = \"http://127.0.0.1:9/catalog\"\nstorage = \"http://127.0.0.1:9\"\nwarehouse = \"exports\"\nnamespace = \"mirror\"\n\
         table_name = \"spans\"\nformat_version = 2\ntoken = \"${secret://catalog-token}\"\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["export", "run", "spans-lake", "--project", "research"])
        .current_dir(dir.path())
        .env_remove("CONTEXTFUL_NODE_ID")
        .env_remove("CONTEXTFUL_TOKEN")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "a compiled-out Iceberg export ran");
    assert!(stderr.contains("ProfileCapabilityAbsent") && stderr.contains("`iceberg-export`"), "{stderr}");
}
