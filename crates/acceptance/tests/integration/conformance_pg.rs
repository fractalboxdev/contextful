#![cfg(feature = "pg-conformance")]
//! The remote profile runs against a supplied PG-enabled release artifact and synthetic services.

use contextful_acceptance::conformance::{self, Options};
use std::path::PathBuf;

#[test]
fn the_cloudflare_profile_executes_every_required_scenario_on_remote_state() {
    let _guard = super::CONFORMANCE_ENV.lock().unwrap();
    let selected = PathBuf::from(
        std::env::var_os("CONTEXTFUL_CONFORMANCE_TEST_BINARY")
            .expect("pg-conformance requires a supplied pg-catalog artifact"),
    );
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("contextful");
    std::fs::copy(selected, &binary).unwrap();
    let report = conformance::run(&Options {
        binary,
        storage_profile: "cloudflare-postgres".into(),
        required: vec![],
        timeout_seconds: 60,
    })
    .unwrap();
    assert!(
        report.passed(),
        "{}",
        serde_json::to_string_pretty(&report).unwrap()
    );
    assert_eq!(report.storage_profile, "cloudflare-postgres");
    assert_eq!(report.scenarios.len(), 7);
}

#[test]
#[cfg(target_os = "macos")]
fn a_remote_fixture_releases_postgres_shared_memory_on_drop() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let _guard = super::CONFORMANCE_ENV.lock().unwrap();
    let selected = PathBuf::from(
        std::env::var_os("CONTEXTFUL_CONFORMANCE_TEST_BINARY")
            .expect("pg-conformance requires a supplied pg-catalog artifact"),
    );
    let prior_path = std::env::var_os("PATH").unwrap();
    let postgres = std::env::split_paths(&prior_path)
        .map(|dir| dir.join("postgres"))
        .find(|path| path.is_file())
        .unwrap()
        .canonicalize()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let wrapper = dir.path().join("postgres");
    let log = dir.path().join("postgres-pids");
    let quote =
        |value: &std::path::Path| format!("'{}'", value.to_str().unwrap().replace('\'', "'\\''"));
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" >> {}\nexec {} \"$@\"\n",
            quote(&log),
            quote(&postgres)
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut paths = vec![dir.path().to_path_buf()];
    paths.extend(std::env::split_paths(&prior_path));
    std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
    let result = conformance::run(&Options {
        binary: selected,
        storage_profile: "cloudflare-postgres".into(),
        required: vec!["pagination-checkpoint".into()],
        timeout_seconds: 60,
    });
    std::env::set_var("PATH", prior_path);
    let report = result.unwrap();
    assert!(
        report.passed(),
        "{}",
        serde_json::to_string_pretty(&report).unwrap()
    );
    let pids: Vec<u32> = std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|pid| pid.parse().unwrap())
        .collect();
    assert_eq!(pids.len(), 1, "one scenario owns one PostgreSQL master");
    let ipc = Command::new("/usr/bin/ipcs")
        .args(["-m", "-p"])
        .output()
        .unwrap();
    assert!(
        ipc.status.success(),
        "{}",
        String::from_utf8_lossy(&ipc.stderr)
    );
    let listing = String::from_utf8(ipc.stdout).unwrap();
    let creator = listing
        .lines()
        .find(|line| line.split_whitespace().any(|word| word == "CPID"))
        .unwrap()
        .split_whitespace()
        .position(|word| word == "CPID")
        .unwrap();
    let leaked: Vec<_> = listing
        .lines()
        .filter(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            fields.first() == Some(&"m")
                && fields
                    .get(creator)
                    .and_then(|pid| pid.parse::<u32>().ok())
                    .is_some_and(|pid| pids.contains(&pid))
        })
        .collect();
    assert!(
        leaked.is_empty(),
        "PostgreSQL fixture retains SysV shared memory after drop: {leaked:?}"
    );
}
