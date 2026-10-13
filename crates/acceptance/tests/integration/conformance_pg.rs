//! The remote profile runs against a supplied PG-enabled release artifact and synthetic services.

use contextful_acceptance::conformance::{self, Options};
use std::path::PathBuf;

#[test]
#[ignore = "requires a supplied pg-catalog artifact and installed PostgreSQL fixture tools"]
fn the_cloudflare_profile_executes_every_required_scenario_on_remote_state() {
    let _guard = super::CONFORMANCE_ENV.lock().unwrap();
    let selected = PathBuf::from(
        std::env::var_os("CONTEXTFUL_CONFORMANCE_TEST_BINARY")
            .expect("remote conformance requires a supplied pg-catalog artifact"),
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
#[cfg(unix)]
fn unexpected_storage_initialization_errors_fail_without_exporting_credentials() {
    use contextful_acceptance::conformance::Outcome;
    use std::os::unix::fs::PermissionsExt;

    let _guard = super::CONFORMANCE_ENV.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("contextful");
    let helpers = dir.path().join("helpers");
    std::fs::create_dir(&helpers).unwrap();
    std::fs::write(helpers.join("initdb"), "#!/bin/sh\nexit 0\n").unwrap();
    // Classification stops at the artifact's initialize error, before any SQL request.
    std::fs::write(helpers.join("postgres"), "#!/usr/bin/python3\nimport signal, socket, sys\nport = int(sys.argv[sys.argv.index('-p') + 1])\nsignal.signal(signal.SIGINT, lambda *_: sys.exit(0))\nwith socket.socket() as server:\n    server.bind(('127.0.0.1', port))\n    server.listen()\n    while True:\n        connection, _ = server.accept()\n        connection.close()\n").unwrap();
    for helper in ["initdb", "postgres"] {
        std::fs::set_permissions(helpers.join(helper), std::fs::Permissions::from_mode(0o700))
            .unwrap();
    }
    let prior_path = std::env::var_os("PATH");
    // Fixture tools resolve first while concurrent acceptance builds retain their toolchain.
    let mut paths = vec![helpers];
    paths.extend(prior_path.iter().flat_map(|path| std::env::split_paths(path)));
    std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
    struct RestorePath(Option<std::ffi::OsString>);
    impl Drop for RestorePath {
        fn drop(&mut self) {
            if let Some(path) = &self.0 {
                std::env::set_var("PATH", path);
            } else {
                std::env::remove_var("PATH");
            }
        }
    }
    let _restore = RestorePath(prior_path);
    for (error, expected, diagnostic) in [
        (
            "database connection refused",
            Outcome::Failed,
            "ConformanceStorageInitializeFailed",
        ),
        (
            "ProfileCapabilityAbsent: pg-catalog",
            Outcome::Unsupported,
            "ConformanceCapabilityMissing:cloudflare-postgres",
        ),
        (
            "StorageCapabilityMissing: pg-catalog",
            Outcome::Unsupported,
            "ConformanceCapabilityMissing:cloudflare-postgres",
        ),
        (
            "StorageProfileUnsupported: cloudflare-postgres",
            Outcome::Unsupported,
            "ConformanceCapabilityMissing:cloudflare-postgres",
        ),
    ] {
        std::fs::write(&binary,format!("#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'contextful fixture development\\n'; exit 0; fi\nif [ \"$2\" = --help ]; then exit 0; fi\nif [ \"$1 $2\" = 'storage initialize' ]; then printf '%s\\n' '{error}: credential-material' >&2; exit 1; fi\nexit 99\n")).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let report = conformance::run(&Options {
            binary: binary.clone(),
            storage_profile: "cloudflare-postgres".into(),
            required: vec!["pagination-checkpoint".into()],
            timeout_seconds: 60,
        })
        .unwrap();
        assert!(!report.passed());
        let scenario = report
            .scenarios
            .iter()
            .find(|scenario| scenario.required)
            .unwrap();
        assert_eq!(scenario.outcome, expected, "{report:?}");
        assert_eq!(scenario.diagnostic.as_deref(), Some(diagnostic));
        assert!(!serde_json::to_string(&report)
            .unwrap()
            .contains("credential-material"));
    }
}

#[test]
#[cfg(target_os = "macos")]
#[ignore = "requires a supplied pg-catalog artifact, installed PostgreSQL fixture tools and macOS IPC inspection"]
fn a_remote_fixture_releases_postgres_shared_memory_on_drop() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let _guard = super::CONFORMANCE_ENV.lock().unwrap();
    let selected = PathBuf::from(
        std::env::var_os("CONTEXTFUL_CONFORMANCE_TEST_BINARY")
            .expect("remote conformance requires a supplied pg-catalog artifact"),
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
