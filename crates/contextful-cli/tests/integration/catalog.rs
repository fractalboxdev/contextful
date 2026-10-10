//! The Postgres catalog wired by a binary built with the `pg-catalog` feature.
#![cfg(feature = "pg-catalog")]

use std::process::Command;

/// A build linking `pg-catalog` given `CONTEXTFUL_CATALOG_URL` opens the Postgres catalog it names in place of the
/// local catalog file; a build without `pg-catalog` given it raises {{topology.package.capability-absent}}.
///
/// The named server is unreachable, so the command fails naming it, never the password, and no local catalog file
/// opens beside it; `catalog_absent` holds the build without the feature.
// spec: topology.coordinate.catalog-url@e265e6d7
#[test]
fn a_named_postgres_catalog_replaces_the_local_catalog_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".contextful/context/research")).unwrap();
    std::fs::write(dir.path().join("contextful.toml"), "[[pipeline.tables]]\nname = \"filings\"\n").unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let out = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["run", "history", "--project", "research"])
        .current_dir(dir.path())
        .env_remove("CONTEXTFUL_NODE_ID")
        .env("CONTEXTFUL_CATALOG_URL", format!("host=127.0.0.1 port={port} password=canary-secret dbname=catalog connect_timeout=5"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "an unreachable catalog listed history: {}", String::from_utf8_lossy(&out.stdout));
    assert!(stderr.contains(&format!("Postgres catalog 127.0.0.1:{port}/catalog")), "{stderr}");
    assert!(!stderr.contains("canary-secret"), "{stderr}");
    assert!(!dir.path().join(".contextful/context/research/machine.sqlite").exists(), "the local catalog file opened beside the named one");
}
