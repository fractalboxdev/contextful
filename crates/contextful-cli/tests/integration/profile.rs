//! The profile a build was selected with, and a build without the data plane answering each
//! data-plane subcommand by name.

use std::process::{Command, Output};

fn cf(dir: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful")).args(args).current_dir(dir).env_remove("CONTEXTFUL_NODE_ID").output().unwrap()
}

/// The profile bundle this test build selected, as the version line names it.
fn selected() -> &'static str {
    if cfg!(feature = "contextful-full") {
        "contextful-full"
    } else if cfg!(feature = "contextful-edge") {
        "contextful-edge"
    } else if cfg!(feature = "contextful-control") {
        "contextful-control"
    } else {
        "development"
    }
}

/// `contextful --version` prints the workspace version and the profile bundle the build selected, `development` for a
/// build selecting none.
// spec: topology.package.version-profile@d7e96b60
#[test]
fn the_version_names_the_workspace_version_and_the_profile() {
    let dir = tempfile::tempdir().unwrap();
    let out = cf(dir.path(), &["--version"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(line, format!("contextful {} {}", env!("CARGO_PKG_VERSION"), selected()));
}

/// The subcommands this build's profile does not link: every data-plane name on a build
/// without the read path, and the run path's names on the read replica.
#[cfg(not(feature = "data-plane"))]
fn unlinked() -> &'static [&'static [&'static str]] {
    const RUN: [&[&str]; 5] = [
        &["init", "research"],
        &["build", "orders_daily"],
        &["pipeline", "validate"],
        &["export", "run", "spans-mirror"],
        &["eval", "run", "--goldens", "cases.jsonl"],
    ];
    const ALL: [&[&str]; 9] = [
        &["init", "research"],
        &["build", "orders_daily"],
        &["query", "SELECT 1"],
        &["pipeline", "validate"],
        &["sync", "probe", "file:///nowhere"],
        &["serve", "--http", "127.0.0.1:0"],
        &["mcp"],
        &["export", "run", "spans-mirror"],
        &["eval", "run", "--goldens", "cases.jsonl"],
    ];
    if cfg!(feature = "read-plane") {
        &RUN
    } else {
        &ALL
    }
}

/// A subcommand reaching a capability its build's profile does not link raises `ProfileCapabilityAbsent`, naming
/// the subcommand and the profile, and acts on nothing; a name no profile defines is a usage error.
// spec: topology.package.capability-absent@f3e98eb1
#[cfg(not(feature = "data-plane"))]
#[test]
fn a_data_plane_subcommand_on_a_build_without_it_is_refused_by_name() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("contextful.toml"), "[[pipeline]]\nid = \"feed\"\ntables = [\"orders\"]\n").unwrap();
    for args in unlinked() {
        let out = cf(dir.path(), args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {err}");
        assert!(err.starts_with("ProfileCapabilityAbsent:"), "{args:?}: {err}");
        assert!(err.contains(&format!("`{}`", args[0])) && err.contains(selected()), "{args:?}: {err}");
    }
    assert!(!dir.path().join(".contextful").exists(), "a refused subcommand created the store root");

    let out = cf(dir.path(), &["frobnicate"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{err}");
    assert!(!err.contains("ProfileCapabilityAbsent"), "{err}");

    // The identity role stays linked: a key pair is minted on the same build.
    let out = cf(dir.path(), &["token", "keygen", "--out", "issuer.key"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// The read replica answers a raw statement and reaches its bucket, and refuses every run-path subcommand by
/// name before touching the project.
#[cfg(all(feature = "contextful-edge", not(feature = "contextful-full")))]
#[test]
fn the_read_replica_reads_and_syncs_and_refuses_every_write_path() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("contextful.toml"), "[[pipeline]]\nid = \"feed\"\ntables = [\"orders\"]\n").unwrap();
    for args in [&["init", "research"][..], &["run", "list"], &["job", "validate"], &["pipeline", "fire", "feed"], &["context", "land"], &["memory", "write"], &["derive", "run"], &["export", "run", "spans-mirror"]] {
        let out = cf(dir.path(), args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.starts_with("ProfileCapabilityAbsent:") && err.contains("`contextful-edge`"), "{args:?}: {err}");
    }
    let out = cf(dir.path(), &["query", "--json", "SELECT 41 + 1 AS answer"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("42"), "{}", String::from_utf8_lossy(&out.stdout));
    for args in [&["sync", "probe", "file:///nowhere"][..], &["mcp", "--help"], &["serve", "--help"]] {
        let out = cf(dir.path(), args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!err.contains("ProfileCapabilityAbsent"), "{args:?}: {err}");
    }
    assert!(!dir.path().join(".contextful").join("context").join("research").exists(), "a refused subcommand created a store root");
}
