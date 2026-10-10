//! A Postgres catalog named to a binary built without the `pg-catalog` feature.
#![cfg(all(feature = "data-plane", not(feature = "pg-catalog")))]

use std::process::Command;

/// A build linking no Postgres catalog refuses a named one, rather than coordinating
/// through the local catalog file while its peers coordinate through the database.
#[test]
fn a_named_postgres_catalog_on_a_build_without_it_refuses_naming_the_subcommand_and_profile() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".contextful/context/research")).unwrap();
    std::fs::write(dir.path().join("contextful.toml"), "[[pipeline.tables]]\nname = \"filings\"\n").unwrap();
    let history = |url: Option<&str>| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_contextful"));
        cmd.args(["run", "history", "--project", "research"]).current_dir(dir.path()).env_remove("CONTEXTFUL_NODE_ID").env_remove("CONTEXTFUL_CATALOG_URL");
        if let Some(url) = url {
            cmd.env("CONTEXTFUL_CATALOG_URL", url);
        }
        cmd.output().unwrap()
    };
    let local = history(None);
    assert!(local.status.success(), "{}", String::from_utf8_lossy(&local.stderr));

    let out = history(Some("host=127.0.0.1 port=1 dbname=catalog"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "a build without `pg-catalog` opened the local catalog: {}", String::from_utf8_lossy(&out.stdout));
    assert!(stderr.contains("ProfileCapabilityAbsent") && stderr.contains("`run`") && stderr.contains("pg-catalog"), "{stderr}");
}
