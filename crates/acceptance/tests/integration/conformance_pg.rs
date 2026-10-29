//! The remote profile runs against a supplied PG-enabled release artifact and synthetic services.

use contextful_acceptance::conformance::{self, Options};
use std::path::PathBuf;

#[test]
#[ignore = "requires a supplied pg-catalog artifact and installed PostgreSQL fixture tools"]
fn the_postgres_profile_executes_every_required_scenario_on_remote_state() {
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
        storage_profile: "postgres".into(),
        required: vec![],
        timeout_seconds: 60,
    })
    .unwrap();
    assert!(
        report.passed(),
        "{}",
        serde_json::to_string_pretty(&report).unwrap()
    );
    assert_eq!(report.storage_profile, "postgres");
    assert_eq!(report.scenarios.len(), 9);
    assert!(report.scenarios.iter().any(|scenario| scenario.id == "delegated-item-fence" && scenario.outcome == conformance::Outcome::Passed));
}

// spec: assurance.consumer.storage-profile@dc4e2706
#[test]
#[cfg(unix)]
fn unexpected_storage_initialization_errors_fail_without_exporting_credentials() {
    use contextful_acceptance::conformance::Outcome;
    use std::os::unix::fs::PermissionsExt;

    let _guard = super::CONFORMANCE_ENV.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("contextful");
    let _restore = fixture_tools(dir.path());
    for (error, expected, diagnostic) in [
        (
            "database connection refused",
            Outcome::Failed,
            "ConformanceStorageInitializeFailed",
        ),
        (
            "StorageBindingUnavailable",
            Outcome::Failed,
            "ConformanceStorageInitializeFailed:StorageBindingUnavailable",
        ),
        (
            "StorageCorpusUnavailable: R2 corpus bucket: unavailable: LIST `consumer/`",
            Outcome::Failed,
            "ConformanceStorageInitializeFailed:StorageCorpusUnavailable",
        ),
        (
            "ProfileCapabilityAbsent: pg-catalog",
            Outcome::Unsupported,
            "ConformanceCapabilityMissing:postgres",
        ),
        (
            "StorageCapabilityMissing: pg-catalog",
            Outcome::Unsupported,
            "ConformanceCapabilityMissing:postgres",
        ),
        (
            "StorageProfileUnsupported: postgres",
            Outcome::Unsupported,
            "ConformanceCapabilityMissing:postgres",
        ),
    ] {
        std::fs::write(&binary,format!("#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'contextful fixture development\\n'; exit 0; fi\nif [ \"$2\" = --help ]; then exit 0; fi\nif [ \"$1 $2\" = 'storage initialize' ]; then printf '%s\\n' '{error}: credential-material' >&2; exit 1; fi\nexit 99\n")).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let report = conformance::run(&Options {
            binary: binary.clone(),
            storage_profile: "postgres".into(),
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

// spec: assurance.consumer.artifact-stability@68a33dfc
#[test]
#[cfg(unix)]
fn an_artifact_whose_bytes_change_during_the_run_earns_no_compatibility_verdict() {
    use contextful_acceptance::conformance::Outcome;
    use std::os::unix::fs::PermissionsExt;

    let _guard = super::CONFORMANCE_ENV.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("contextful");
    let _restore = fixture_tools(dir.path());
    std::fs::write(&binary, "#!/bin/sh\nif [ \"$1\" = --version ]; then printf '#\\n' >> \"$0\"; printf 'contextful fixture development\\n'; exit 0; fi\nif [ \"$2\" = --help ]; then exit 0; fi\nif [ \"$1 $2\" = 'storage initialize' ]; then printf 'StorageProfileUnsupported: postgres\\n' >&2; exit 1; fi\nexit 99\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let report = conformance::run(&Options {
        binary: binary.clone(),
        storage_profile: "postgres".into(),
        required: vec!["pagination-checkpoint".into()],
        timeout_seconds: 60,
    })
    .unwrap();
    assert!(!report.passed());
    let changed = report.scenarios.iter().find(|s| s.id == "artifact-stability").unwrap_or_else(|| panic!("{report:?}"));
    assert!(changed.required);
    assert_eq!(changed.outcome, Outcome::Failed);
    assert_eq!(changed.diagnostic.as_deref(), Some("ConformanceArtifactChanged"));
}

#[test]
#[cfg(unix)]
fn a_failed_storage_check_names_the_identifier_the_cli_prints_alone() {
    use contextful_acceptance::conformance::Outcome;
    use std::os::unix::fs::PermissionsExt;

    let _guard = super::CONFORMANCE_ENV.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("contextful");
    let _restore = fixture_tools(dir.path());
    // The CLI prints its error chain as `{:#}`: a D1 failure is a bare identifier, alone
    // or as the last segment under a context.
    for (error, diagnostic) in [
        ("StorageBindingUnavailable", "ConformanceCommandFailed:storage:StorageBindingUnavailable"),
        ("private D1 binding: StorageBindingFailed", "ConformanceCommandFailed:storage:StorageBindingFailed"),
        ("StorageCapabilityMissing: R2 corpus bucket: list: HTTP 500 InternalError", "ConformanceCommandFailed:storage:StorageCapabilityMissing"),
        ("No such file or directory (os error 2)", "ConformanceCommandFailed:storage"),
        ("R2 secret: CredentialMaterial0123", "ConformanceCommandFailed:storage"),
    ] {
        std::fs::write(&binary, format!("#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'contextful fixture development\\n'; exit 0; fi\nif [ \"$2\" = --help ]; then exit 0; fi\nif [ \"$1 $2\" = 'storage initialize' ]; then exit 0; fi\nif [ \"$1 $2\" = 'storage check' ]; then printf '%s\\n' '{error}' 'endpoint credential-material' >&2; exit 1; fi\nexit 99\n")).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let report = conformance::run(&Options {
            binary: binary.clone(),
            storage_profile: "postgres".into(),
            required: vec!["empty-replacement".into()],
            timeout_seconds: 60,
        })
        .unwrap();
        let scenario = report.scenarios.iter().find(|scenario| scenario.required).unwrap();
        assert_eq!(scenario.outcome, Outcome::Failed, "{report:?}");
        assert_eq!(scenario.diagnostic.as_deref(), Some(diagnostic));
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains("credential-material") && !serialized.contains("CredentialMaterial"));
    }
}

/// Synthetic `initdb` and `postgres` resolve first on `PATH` until the guard drops; the
/// server accepts and closes connections, so classification stops at the artifact's stderr.
#[cfg(unix)]
fn fixture_tools(dir: &std::path::Path) -> RestorePath {
    use std::os::unix::fs::PermissionsExt;
    let helpers = dir.join("helpers");
    std::fs::create_dir(&helpers).unwrap();
    std::fs::write(helpers.join("initdb"), "#!/bin/sh\nexit 0\n").unwrap();
    let listener_source = helpers.join("postgres.rs");
    std::fs::write(&listener_source, r#"
use std::net::TcpListener;

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let port = args.windows(2).find(|pair| pair[0] == "-p").unwrap()[1].parse::<u16>().unwrap();
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    for connection in listener.incoming() {
        drop(connection.unwrap());
    }
}
"#).unwrap();
    let compiled = std::process::Command::new("rustc")
        .arg(&listener_source)
        .arg("-o")
        .arg(helpers.join("postgres"))
        .status()
        .unwrap();
    assert!(compiled.success(), "failed to compile synthetic postgres listener");
    for helper in ["initdb", "postgres"] {
        std::fs::set_permissions(helpers.join(helper), std::fs::Permissions::from_mode(0o700))
            .unwrap();
    }
    let prior_path = std::env::var_os("PATH");
    // Fixture tools resolve first while concurrent acceptance builds retain their toolchain.
    let mut paths = vec![helpers];
    paths.extend(prior_path.iter().flat_map(|path| std::env::split_paths(path)));
    std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
    RestorePath(prior_path)
}

#[cfg(unix)]
struct RestorePath(Option<std::ffi::OsString>);

#[cfg(unix)]
impl Drop for RestorePath {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            std::env::set_var("PATH", path);
        } else {
            std::env::remove_var("PATH");
        }
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
        storage_profile: "postgres".into(),
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
