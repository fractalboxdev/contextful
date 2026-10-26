//! The consumer suite accepts an artifact without a checkout; D1 uses explicit bindings.

use contextful_acceptance::conformance::{self, Options, Outcome};
use std::path::Path;

#[cfg(unix)]
fn executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// The suite runs inside the caller's process group, as a release build runs it under a
/// supervisor; stopping each finished artifact process signals only that process's own group.
/// procps `kill` reads `-KILL -<pid>` without `--` as a kill of the caller's group.
#[test]
#[cfg(unix)]
fn stopping_a_finished_artifact_leaves_the_callers_process_group_running() {
    use std::os::unix::process::CommandExt;
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("contextful");
    executable(&binary, "#!/bin/sh\ncase \"$1\" in\n--version) printf 'contextful 0.5.0 contextful-edge\\n';;\n*) printf 'ProfileCapabilityAbsent: run requires data-plane\\n' >&2; exit 1;;\nesac\n");
    let output = dir.path().join("report.json");
    let script = format!(
        "{suite} --binary {binary} --output {output} --storage-profile local; echo caller-survived",
        suite = env!("CARGO_BIN_EXE_contextful-conformance"),
        binary = binary.display(),
        output = output.display(),
    );
    let caller = std::process::Command::new("/bin/sh").args(["-c", &script]).process_group(0).output().unwrap();
    assert!(output.exists(), "the suite wrote no report: {}", String::from_utf8_lossy(&caller.stderr));
    assert!(
        String::from_utf8_lossy(&caller.stdout).contains("caller-survived"),
        "the caller's group was signalled: {:?}",
        caller.status
    );
}

#[test]
#[cfg(unix)]
fn missing_capability_reports_unsupported_without_admitting_work() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("contextful");
    executable(&binary, "#!/bin/sh\ncase \"$1\" in\n--version) printf 'contextful 0.5.0 contextful-edge\\n';;\n*) printf 'ProfileCapabilityAbsent: run requires data-plane\\n' >&2; exit 1;;\nesac\n");
    let report = conformance::run(&Options {
        binary: binary.clone(),
        storage_profile: "local".into(),
        required: vec!["hard-crash-replay".into()],
        timeout_seconds: 30,
    })
    .unwrap();
    assert!(!report.passed());
    assert_eq!(report.suite_revision, conformance::suite_revision());
    assert_eq!(report.artifact_digest.len(), 64);
    assert_eq!(
        report.reported_version.as_deref(),
        Some("contextful 0.5.0 contextful-edge")
    );
    assert_eq!(
        report.source_commit, None,
        "a version is no evidence of source correspondence"
    );
    let result = report
        .scenarios
        .iter()
        .find(|r| r.id == "hard-crash-replay")
        .unwrap();
    assert_eq!(result.outcome, Outcome::Unsupported);
    assert!(result.requirement.as_deref().unwrap().contains("run"));
    assert!(!dir.path().join(".contextful").exists());
}

#[test]
#[cfg(unix)]
fn a_failed_command_names_its_error_identifier_and_withholds_its_message() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("contextful");
    executable(&binary, "#!/bin/sh\ncase \"$1 $2\" in\n'--version ') printf 'contextful fixture contextful-full\\n';;\n*' --help') ;;\n*) printf 'recovery binding: ExecutionRecoveryMismatch: key secret-material\\n' >&2; exit 1;;\nesac\n");
    let report = conformance::run(&Options {
        binary,
        storage_profile: "local".into(),
        required: vec!["empty-replacement".into()],
        timeout_seconds: 30,
    })
    .unwrap();
    let result = report.scenarios.iter().find(|s| s.id == "empty-replacement").unwrap();
    assert_eq!(result.outcome, Outcome::Failed);
    assert_eq!(
        result.diagnostic.as_deref(),
        Some("ConformanceCommandFailed:pipeline:ExecutionRecoveryMismatch")
    );
    assert!(!serde_json::to_string(&report).unwrap().contains("secret-material"));
}

#[test]
#[cfg(unix)]
fn artifact_runs_with_an_empty_provider_environment() {
    let _guard = super::CONFORMANCE_ENV.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("contextful");
    executable(&binary, "#!/bin/sh\nif [ -n \"${CONFORMANCE_INHERITED_SECRET:-}\" ]; then exit 99; fi\ncase \"$1\" in\n--version) printf 'contextful fixture development\\n';;\n*) printf 'ProfileCapabilityAbsent: run\\n' >&2; exit 1;;\nesac\n");
    std::env::set_var("CONFORMANCE_INHERITED_SECRET", "credential-material");
    let report = conformance::run(&Options {
        binary,
        storage_profile: "local".into(),
        required: vec![],
        timeout_seconds: 30,
    })
    .unwrap();
    std::env::remove_var("CONFORMANCE_INHERITED_SECRET");
    assert_eq!(
        report.reported_version.as_deref(),
        Some("contextful fixture development")
    );
    assert!(!serde_json::to_string(&report)
        .unwrap()
        .contains("credential-material"));
}

// spec: assurance.consumer.verdict@58ba38bf
#[test]
#[cfg(unix)]
fn unknown_storage_or_scenario_never_counts_as_a_pass() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("contextful");
    executable(
        &binary,
        "#!/bin/sh\nprintf 'contextful fixture development\\n'\n",
    );
    for (profile, required) in [("imaginary", vec![]), ("local", vec!["imaginary".into()])] {
        let report = conformance::run(&Options {
            binary: binary.clone(),
            storage_profile: profile.into(),
            required,
            timeout_seconds: 30,
        })
        .unwrap();
        assert!(!report.passed());
        assert!(report
            .scenarios
            .iter()
            .any(|r| matches!(r.outcome, Outcome::Unsupported | Outcome::Failed)));
    }
}

#[test]
#[cfg(unix)]
fn a_d1_consumer_run_requires_its_real_bindings_before_a_verdict() {
    let _guard = super::CONFORMANCE_ENV.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("contextful");
    executable(&binary, "#!/bin/sh\nprintf 'contextful fixture contextful-full\\n'\n");
    let report = conformance::run(&Options {
        binary,
        storage_profile: "cloudflare-d1".into(),
        required: vec!["hard-crash-replay".into()],
        timeout_seconds: 30,
    }).unwrap();
    assert!(!report.passed());
    let result = report.scenarios.iter().find(|s| s.id == "hard-crash-replay").unwrap();
    assert_eq!(result.outcome, Outcome::Unsupported);
    assert_eq!(result.diagnostic.as_deref(), Some("ConformanceCapabilityMissing:cloudflare-d1"));
}

#[test]
#[cfg(unix)]
fn a_live_d1_admission_lease_refuses_a_second_suite_before_any_reset() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let _guard = super::CONFORMANCE_ENV.lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut bridge = Command::new("node")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../contextful-d1/tests/fixture.mjs"))
        .arg(root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn().unwrap();
    let mut port = String::new();
    BufReader::new(bridge.stdout.take().unwrap()).read_line(&mut port).unwrap();
    let endpoint = format!("http://127.0.0.1:{}", port.trim());
    let settings = [
        ("CONTEXTFUL_CONFORMANCE_D1_ENDPOINT", endpoint.clone()),
        ("CONTEXTFUL_CONFORMANCE_R2_ENDPOINT", "http://127.0.0.1:9".into()),
        ("CONTEXTFUL_CONFORMANCE_R2_BUCKET", "conformance".into()),
        ("CONTEXTFUL_CONFORMANCE_R2_ACCESS_KEY_ID", "synthetic".into()),
        ("CONTEXTFUL_CONFORMANCE_R2_SECRET_ACCESS_KEY", "synthetic".into()),
        ("CONTEXTFUL_CONFORMANCE_RECOVERY_ID", "synthetic-admission".into()),
        ("CONTEXTFUL_CONFORMANCE_D1_DISPOSABLE", "1".into()),
    ];
    for (key, value) in &settings { std::env::set_var(key, value); }
    let sql = |statement: &str| {
        let body = serde_json::json!({"sql":statement,"params":[]}).to_string();
        let mut response = ureq::post(&format!("{endpoint}/sql"))
            .header("content-type", "application/json")
            .send(body.as_bytes()).unwrap();
        assert_eq!(response.status(), 200);
        serde_json::from_slice::<serde_json::Value>(&response.body_mut().read_to_vec().unwrap()).unwrap()
    };
    // A marked, bound database whose reset would empty every execution table.
    for table in ["contextful_guard", "contextful_reconciliation", "contextful_export", "contextful_awakeable", "contextful_sweep", "contextful_blob", "contextful_journal", "contextful_run", "contextful_scope", "contextful_lease"] {
        sql(&format!("CREATE TABLE {table} (x)"));
    }
    sql("CREATE TABLE contextful_recovery (id INTEGER PRIMARY KEY, project TEXT, generation TEXT)");
    sql("INSERT INTO contextful_recovery VALUES (1, 'research', 'synthetic-admission')");
    sql("CREATE TABLE contextful_conformance (id INTEGER PRIMARY KEY, generation TEXT)");
    sql("INSERT INTO contextful_conformance VALUES (1, 'synthetic-admission')");
    sql("INSERT INTO contextful_scope VALUES ('protected-sentinel')");
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("contextful");
    executable(&binary, "#!/bin/sh\ncase \"$1 $2\" in\n'--version ') printf 'contextful fixture contextful-full\\n';;\n*' --help') ;;\n*) printf 'StorageBindingUnavailable\\n' >&2; exit 1;;\nesac\n");
    let suite = |required: &str| conformance::run(&Options {
        binary: binary.clone(),
        storage_profile: "cloudflare-d1".into(),
        required: vec![required.into()],
        timeout_seconds: 30,
    }).unwrap();
    // Another container's suite holds the lease and renews it.
    sql("CREATE TABLE conformance_admission (id INTEGER PRIMARY KEY CHECK (id = 1), holder TEXT NOT NULL, expires_at INTEGER NOT NULL)");
    sql("INSERT INTO conformance_admission VALUES (1, 'another-suite', unixepoch() + 120)");
    let refused = suite("empty-replacement");
    let scenario = refused.scenarios.iter().find(|s| s.required).unwrap();
    assert_eq!(scenario.outcome, Outcome::Failed);
    assert_eq!(scenario.diagnostic.as_deref(), Some("ConformanceAlreadyRunning"));
    assert_eq!(sql("SELECT COUNT(*) AS n FROM contextful_scope")["rows"][0]["n"], 1, "a refused suite reset the database");
    assert_eq!(sql("SELECT holder FROM conformance_admission")["rows"][0]["holder"], "another-suite");
    // A crashed holder's lease lapses; the next suite admits, resets and releases.
    sql("UPDATE conformance_admission SET expires_at = unixepoch() - 1");
    let admitted = suite("empty-replacement");
    let scenario = admitted.scenarios.iter().find(|s| s.required).unwrap();
    assert_ne!(scenario.diagnostic.as_deref(), Some("ConformanceAlreadyRunning"), "{admitted:?}");
    assert_eq!(sql("SELECT COUNT(*) AS n FROM contextful_scope")["rows"][0]["n"], 0);
    assert_eq!(sql("SELECT COUNT(*) AS n FROM conformance_admission")["rows"][0]["n"], 0, "the suite kept its lease");
    for (key, _) in &settings { std::env::remove_var(key); }
    let _ = bridge.kill();
    let _ = bridge.wait();
}

#[test]
#[cfg(unix)]
fn a_d1_consumer_replays_after_empty_disk_replacement() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let _guard = super::CONFORMANCE_ENV.lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut bridge = Command::new("node")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../contextful-d1/tests/fixture.mjs"))
        .arg(root.path())
        .stdout(Stdio::piped())
        .spawn().unwrap();
    let mut port = String::new();
    BufReader::new(bridge.stdout.take().unwrap()).read_line(&mut port).unwrap();
    let corpus = contextful_acceptance::s3::S3Server::start("conformance");
    let settings = [
        ("CONTEXTFUL_CONFORMANCE_D1_ENDPOINT", format!("http://127.0.0.1:{}", port.trim())),
        ("CONTEXTFUL_CONFORMANCE_R2_ENDPOINT", corpus.endpoint.clone()),
        ("CONTEXTFUL_CONFORMANCE_R2_BUCKET", "conformance".into()),
        ("CONTEXTFUL_CONFORMANCE_R2_ACCESS_KEY_ID", contextful_acceptance::s3::ACCESS_KEY.into()),
        ("CONTEXTFUL_CONFORMANCE_R2_SECRET_ACCESS_KEY", contextful_acceptance::s3::SECRET_KEY.into()),
        ("CONTEXTFUL_CONFORMANCE_RECOVERY_ID", "synthetic-d1-replacement".into()),
        ("CONTEXTFUL_CONFORMANCE_D1_DISPOSABLE", "1".into()),
    ];
    for (key, value) in &settings { std::env::set_var(key, value); }
    let dir = tempfile::tempdir().unwrap();
    let workspace = contextful_acceptance::workspace_root();
    let host = Command::new("rustc").arg("-vV").output().unwrap();
    assert!(host.status.success());
    let host = String::from_utf8(host.stdout).unwrap();
    let target_triple = host.lines().find_map(|line| line.strip_prefix("host: ")).unwrap();
    let build = Command::new(env!("CARGO"))
        .args(["build", "--locked", "--offline", "-p", "contextful-cli", "--bin", "contextful", "--no-default-features", "--features", "contextful-full", "--target", target_triple])
        .current_dir(&workspace).status().unwrap();
    assert!(build.success());
    let target = std::env::var_os("CARGO_TARGET_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| workspace.join("target"));
    let binary = dir.path().join("supplied-contextful");
    std::fs::copy(target.join(target_triple).join("debug/contextful"), &binary).unwrap();
    let report = conformance::run(&Options {
        binary: binary.clone(),
        storage_profile: "cloudflare-d1".into(),
        required: vec![],
        timeout_seconds: 60,
    }).unwrap();
    assert!(report.passed(), "{}", serde_json::to_string_pretty(&report).unwrap());
    assert!(report.scenarios.iter().any(|scenario| {
        scenario.id == "typed-export-acknowledgement" && scenario.outcome == Outcome::Passed
    }));
    let endpoint = &settings[0].1;
    let sql = |statement: &str| {
        let body = serde_json::json!({"sql":statement,"params":[]}).to_string();
        let mut response = ureq::post(&format!("{endpoint}/sql"))
            .header("content-type", "application/json")
            .send(body.as_bytes()).unwrap();
        assert_eq!(response.status(), 200);
        let bytes = response.body_mut().read_to_vec().unwrap();
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
    };
    sql("INSERT INTO contextful_scope (scope, version, cursor) VALUES ('protected-sentinel', 0, '{}')");
    sql("DROP TABLE IF EXISTS contextful_conformance");
    let refused = conformance::run(&Options {
        binary,
        storage_profile: "cloudflare-d1".into(),
        required: vec!["empty-replacement".into()],
        timeout_seconds: 60,
    }).unwrap();
    assert!(!refused.passed(), "an unmarked D1 database was reset");
    assert_eq!(sql("SELECT COUNT(*) AS n FROM contextful_scope WHERE scope = 'protected-sentinel'")["rows"][0]["n"], 1);
    for (key, _) in &settings { std::env::remove_var(key); }
    let _ = bridge.kill();
    let _ = bridge.wait();
}

#[test]
#[cfg(unix)]
fn a_hung_artifact_keeps_machine_readable_failed_outcomes() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("contextful");
    executable(&binary, "#!/bin/sh\nwhile :; do :; done\n");
    let report = conformance::run(&Options {
        binary,
        storage_profile: "local".into(),
        required: vec!["pagination-checkpoint".into()],
        timeout_seconds: 1,
    })
    .unwrap();
    assert!(!report.passed());
    let result = report
        .scenarios
        .iter()
        .find(|scenario| scenario.required)
        .unwrap();
    assert_eq!(result.outcome, Outcome::Failed);
    assert_eq!(
        result.diagnostic.as_deref(),
        Some("ConformanceProcessTimeout:version")
    );
}

#[test]
#[cfg(unix)]
fn a_failed_or_hung_scenario_step_names_its_verb_and_error_identifier() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("contextful");
    for (step, timeout_seconds, diagnostic) in [
        ("printf '%s\\n' StorageBindingUnavailable 'endpoint secret-material' >&2; exit 1", 30, "ConformanceCaptureFailed:run:StorageBindingUnavailable"),
        ("exit 1", 30, "ConformanceCaptureFailed:run"),
        ("while :; do :; done", 1, "ConformanceProcessTimeout:run"),
    ] {
        executable(&binary, &format!("#!/bin/sh\ncase \"$1 $2\" in\n'--version ') printf 'contextful fixture contextful-full\\n';;\n*' --help') ;;\n'run start') {step};;\n*) exit 99;;\nesac\n"));
        let report = conformance::run(&Options {
            binary: binary.clone(),
            storage_profile: "local".into(),
            required: vec!["pagination-checkpoint".into()],
            timeout_seconds,
        })
        .unwrap();
        let result = report.scenarios.iter().find(|s| s.required).unwrap();
        assert_eq!(result.outcome, Outcome::Failed);
        assert_eq!(result.diagnostic.as_deref(), Some(diagnostic));
        assert!(!serde_json::to_string(&report).unwrap().contains("secret-material"));
    }
}

#[test]
#[cfg(unix)]
fn a_daemon_poll_that_never_reads_names_the_query_error_not_a_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("contextful");
    executable(&binary, "#!/bin/sh\ncase \"$1 $2\" in\n'--version ') printf 'contextful fixture contextful-full\\n';;\n*' --help') ;;\n'token keygen') printf 'public-key\\n';;\n'token mint') printf 'token\\n';;\n'pipeline import') ;;\n'pipeline serve')\n  url=$(sed -n 's/.*endpoint = \"\\([^\"]*\\)\".*/\\1/p' contextful.toml)\n  curl -fsS \"$url\" >/dev/null || exit 1\n  trap 'exit 0' TERM\n  while :; do sleep 0.1; done;;\n'query --json') printf 'ReadDurationExceeded: statement reached 30 s\\n' >&2; exit 1;;\n*) exit 99;;\nesac\n");
    let report = conformance::run(&Options {
        binary,
        storage_profile: "local".into(),
        required: vec!["graceful-restart".into()],
        timeout_seconds: 30,
    })
    .unwrap();
    let result = report.scenarios.iter().find(|scenario| scenario.required).unwrap();
    assert_eq!(result.outcome, Outcome::Failed);
    assert_eq!(result.diagnostic.as_deref(), Some("ConformanceCommandFailed:query:ReadDurationExceeded"), "{report:?}");
}

#[test]
#[cfg(unix)]
fn a_running_output_flood_is_stopped_before_the_process_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("contextful");
    executable(
        &binary,
        "#!/bin/sh\nhead -c 2097152 /dev/zero\nwhile :; do :; done\n",
    );
    let report = conformance::run(&Options {
        binary,
        storage_profile: "local".into(),
        required: vec!["pagination-checkpoint".into()],
        timeout_seconds: 10,
    })
    .unwrap();
    assert!(!report.passed());
    let result = report
        .scenarios
        .iter()
        .find(|scenario| scenario.required)
        .unwrap();
    assert_eq!(result.diagnostic.as_deref(), Some("ConformanceOutputLimit"));
}

fn artifact(dir: &Path) -> std::path::PathBuf {
    let selected = std::env::var_os("CONTEXTFUL_CONFORMANCE_TEST_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| contextful_acceptance::bin("contextful"));
    let snapshot = dir.join("supplied-contextful");
    std::fs::copy(selected, &snapshot).unwrap();
    snapshot
}

#[test]
#[cfg(unix)]
fn the_distributed_suite_runs_outside_the_checkout_and_reports_actual_execution() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("compatibility.json");
    let binary = artifact(dir.path());
    let run = std::process::Command::new(env!("CARGO_BIN_EXE_contextful-conformance"))
        .arg("--binary")
        .arg(&binary)
        .arg("--output")
        .arg(&output)
        .current_dir(dir.path())
        .env_clear()
        .env("CONFORMANCE_INHERITED_SECRET", "never-in-the-artifact")
        .output()
        .unwrap();
    let text = std::fs::read_to_string(output).unwrap();
    let report: conformance::Report = serde_json::from_str(&text).unwrap();
    assert!(
        run.status.success(),
        "{text}\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(report.passed());
    assert_eq!(
        report.artifact_digest,
        conformance::digest(&binary).unwrap()
    );
    assert_eq!(report.scenarios.len(), 8);
    assert!(report
        .scenarios
        .iter()
        .all(|scenario| scenario.outcome == Outcome::Passed));
    assert!(!text.contains("never-in-the-artifact"));
    assert!(report.source_commit.is_none());
}

#[test]
#[cfg(unix)]
fn consumer_scenarios_reject_cursor_skips_duplicate_identity_and_premature_acknowledgement() {
    let dir = tempfile::tempdir().unwrap();
    let binary = artifact(dir.path());
    let actual = binary.to_string_lossy().replace('\'', "'\\''");
    for (scenario, mutation, diagnostic) in [
        ("pagination-checkpoint", r#"if [ "$1 $2" = 'run start' ]; then
    sed 's/"cursor":"p2"/"cursor":"done"/g' vendor.sh > vendor.sh.tmp
    mv vendor.sh.tmp vendor.sh
fi"#, "ConformanceCursorSkipOrDuplicateIdentity"),
        ("public-identity-masking", r#"if [ "$1" = query ]; then
    tmp=$(mktemp)
    trap 'rm -f "$tmp"' EXIT
    "$actual" "$@" > "$tmp" || exit $?
    sed 's/public-0002/public-0001/g' "$tmp"
    exit $?
fi"#, "ConformancePublicIdentityChanged"),
        ("typed-export-acknowledgement", "if [ \"$1 $2\" = 'export run' ]; then exit 0; fi", "ConformanceAcknowledgementBeforeCommit"),
        ("applied-scheduling", r#"if [ "$1 $2" = 'pipeline serve' ]; then
    n=$(cat cycle-counter 2>/dev/null || printf 0)
    printf '%s' "$((n + 1))" > cycle-counter
    stamp=$(printf '2030-01-01T%02d:00:00Z' "$n")
    exec "$actual" "$1" "$2" "$3" "$4" "$5" "$6" "$stamp" "$8" "$9"
fi"#, "ConformanceScheduleRepeatedWithinWindow"),
    ] {
        let wrapper = dir.path().join(scenario);
        executable(&wrapper, &format!("#!/bin/sh\nactual='{actual}'\n{mutation}\nexec \"$actual\" \"$@\"\n"));
        let report = conformance::run(&Options { binary: wrapper, storage_profile: "local".into(), required: vec![scenario.into()], timeout_seconds: 40 }).unwrap();
        assert!(!report.passed(), "{scenario} accepted its planted defect");
        let result = report.scenarios.iter().find(|result| result.id == scenario).unwrap();
        assert_eq!(result.outcome, Outcome::Failed);
        assert_eq!(result.diagnostic.as_deref(), Some(diagnostic), "{report:?}");
    }
}

#[test]
#[cfg(unix)]
fn a_failed_scenario_stops_its_daemon_so_the_daemon_ends_its_own_children() {
    let _guard = super::CONFORMANCE_ENV.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("child.pid");
    let binary = dir.path().join("contextful");
    // The daemon starts a child in its own process group, as a serve dispatches one, ends
    // it on SIGTERM, and never serves, so the scenario fails on its deadline.
    executable(&binary, &format!(r#"#!/bin/sh
case "$1 $2" in
"--version ") printf 'contextful 0.5.0 contextful-full\n';;
"token keygen") printf 'ed25519/fixture\n';;
"pipeline serve")
  perl -e 'setpgrp(0, 0); exec "sleep", "300"' &
  child=$!
  printf '%s' "$child" > '{marker}'
  trap 'kill -TERM -"$child"; exit 0' TERM
  while :; do sleep 0.1; done;;
*) printf 'ok\n';;
esac
"#, marker = marker.display()));
    let report = conformance::run(&Options {
        binary,
        storage_profile: "local".into(),
        required: vec!["graceful-restart".into()],
        timeout_seconds: 10,
    })
    .unwrap();
    let result = report.scenarios.iter().find(|r| r.id == "graceful-restart").unwrap();
    assert_eq!(result.diagnostic.as_deref(), Some("ConformanceDaemonTimeout"));
    let child = std::fs::read_to_string(&marker).unwrap();
    let alive = || {
        let present = std::process::Command::new("/bin/kill").args(["-0", child.trim()]).status().unwrap().success();
        if !present { return false; }
        #[cfg(target_os = "linux")]
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", child.trim())) {
            // PID 1 may not reap the exited child promptly; kill -0 still finds a zombie.
            if stat.rsplit_once(") ").is_some_and(|(_, state)| state.starts_with("Z ")) { return false; }
        }
        true
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while alive() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let orphaned = alive();
    if orphaned {
        let _ = std::process::Command::new("/bin/kill").args(["-KILL", child.trim()]).status();
    }
    assert!(!orphaned, "the daemon's dispatched child outlived the scenario");
}
