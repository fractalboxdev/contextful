//! The consumer suite accepts an artifact without a checkout; D1 uses explicit bindings.

use contextful_acceptance::conformance::{self, Options, Outcome};
use std::path::Path;

#[cfg(unix)]
fn executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
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
    let actual = serde_json::to_string(binary.to_str().unwrap()).unwrap();
    for (scenario, mutation, diagnostic) in [
        ("pagination-checkpoint", "if args[:2] == ['run', 'start']:\n    p = pathlib.Path('vendor.sh')\n    p.write_text(p.read_text().replace('\"cursor\":\"p2\"', '\"cursor\":\"done\"'))\n", "ConformanceCursorSkipOrDuplicateIdentity"),
        ("public-identity-masking", "if args[:1] == ['query'] and '--json' in args:\n    result = subprocess.run([actual] + args, capture_output=True)\n    value = json.loads(result.stdout)\n    value['rows'][1][0] = value['rows'][0][0]\n    print(json.dumps(value))\n    sys.exit(result.returncode)\n", "ConformancePublicIdentityChanged"),
        ("typed-export-acknowledgement", "if args[:2] == ['export', 'run']:\n    sys.exit(0)\n", "ConformanceAcknowledgementBeforeCommit"),
        ("applied-scheduling", "if args[:2] == ['pipeline', 'serve'] and '--now' in args:\n    counter = pathlib.Path('cycle-counter')\n    n = int(counter.read_text()) if counter.exists() else 0\n    counter.write_text(str(n + 1))\n    args[args.index('--now') + 1] = '2030-01-01T%02d:00:00Z' % n\n", "ConformanceScheduleRepeatedWithinWindow"),
    ] {
        let wrapper = dir.path().join(scenario);
        executable(&wrapper, &format!("#!/usr/bin/python3\nimport json, os, pathlib, subprocess, sys\nactual = {actual}\nargs = sys.argv[1:]\n{mutation}os.execv(actual, [actual] + args)\n"));
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
    let alive = || std::process::Command::new("/bin/kill").args(["-0", child.trim()]).status().unwrap().success();
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
