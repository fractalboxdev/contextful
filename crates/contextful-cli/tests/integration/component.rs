//! `contextful pipeline` over a component source: the probe guest, pinned by digest, run
//! through the component host and the mediated client.
#![cfg(feature = "component-host")]

use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

/// The probe guest exporting `config` and `attribution` beside the base world.
const PROBE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../contextful-wasm/tests/fixtures/probe.wasm");
/// The probe guest exporting the base world alone.
const PROBE_BASE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../contextful-wasm/tests/fixtures/probe-base.wasm");

fn digest(path: &str) -> String {
    Sha256::digest(std::fs::read(path).unwrap()).iter().map(|b| format!("{b:02x}")).collect()
}

/// A project holding the probe at `connectors/probe.wasm` and `manifest` as its `contextful.toml`.
fn project(manifest: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::create_dir_all(dir.path().join("connectors")).unwrap();
    std::fs::copy(PROBE, dir.path().join("connectors/probe.wasm")).unwrap();
    std::fs::copy(PROBE_BASE, dir.path().join("connectors/probe-base.wasm")).unwrap();
    std::fs::write(dir.path().join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{}", manifest)).unwrap();
    dir
}

/// A pipeline `probe` over `name`, landing `tables`, its source config written as TOML lines.
fn manifest(name: &str, tables: &[&str], config: &str) -> String {
    let tables = tables.iter().map(|t| format!("{t:?}")).collect::<Vec<_>>().join(", ");
    format!("[[pipeline]]\nid = \"probe\"\ntables = [{tables}]\n\n[pipeline.source]\nname = \"{name}\"\n\n[pipeline.source.config]\n{config}\n")
}

fn cf(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(args)
        .current_dir(dir)
        .env_remove("CONTEXTFUL_NODE_ID")
        .env_remove("CONTEXTFUL_SECRETS_BACKEND")
        .env_remove("CONTEXTFUL_COMPONENT_TARGET")
        .envs(env.iter().copied())
        .output()
        .unwrap()
}

fn fire(dir: &Path, run: &str, extra: &[&str], env: &[(&str, &str)]) -> Output {
    let mut args = vec!["pipeline", "run", "probe", "--project", "research", "--run-id", run, "--site-id", "site-a", "--now", "2030-01-01T00:00:00Z"];
    args.extend_from_slice(extra);
    cf(dir, &args, env)
}

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn refused(out: &Output, error: &str) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "expected {error}, got success: {}", String::from_utf8_lossy(&out.stdout));
    assert!(stderr.contains(error), "expected {error}, got: {stderr}");
    stderr
}

fn run_row(dir: &Path, run: &str) -> serde_json::Value {
    serde_json::from_str(&ok(&cf(dir, &["run", "show", run, "--project", "research"], &[]))).unwrap()
}

/// A loopback vendor answering every request with `200 ok`, recording each request's headers.
struct Vendor {
    port: u16,
    seen: Arc<Mutex<Vec<Vec<String>>>>,
}

impl Vendor {
    fn start() -> Vendor {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
        let log = seen.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut headers = Vec::new();
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                    headers.push(h.trim().to_ascii_lowercase());
                }
                log.lock().unwrap().push(headers);
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
            }
        });
        Vendor { port, seen }
    }

    fn requests(&self) -> Vec<Vec<String>> {
        self.seen.lock().unwrap().clone()
    }
}

/// `pipeline run` resolves a component source, admits it against its pin and loads it once per fire, before any
/// run row, and records its {{connector.import.config-hashing}} content hash as each run's connector hash.
// spec: connector.package.component-load@43f09e4a
#[test]
fn pipeline_run_lands_a_pinned_component_and_records_its_digest() {
    let pin = digest(PROBE);
    let dir = project(&manifest("connectors/probe.wasm", &["items"], &format!("sha256 = \"{pin}\"")));
    let out = ok(&fire(dir.path(), "run-1", &[], &[]));
    assert!(out.contains("probe_items: run-1 success · 3 rows in 2 batches"), "{out}");
    let row = run_row(dir.path(), "run-1");
    assert_eq!(row["connector_hash"], serde_json::json!(pin), "with no guest table the digest is the hash verbatim");
    assert_eq!(row["connector_id"], serde_json::json!("connectors/probe.wasm"));

    // The guest's position carries to the next fire, which reads on from row 3.
    let out = ok(&fire(dir.path(), "run-2", &[], &[]));
    assert!(out.contains("probe_items: run-2 success · 0 rows"), "{out}");

    // A forwarded guest table folds into the hash.
    let dir = project(&manifest("connectors/probe.wasm", &["config"], &format!("sha256 = \"{pin}\"\nguest = {{ region = \"eu\" }}")));
    ok(&fire(dir.path(), "run-1", &[], &[]));
    let hash = run_row(dir.path(), "run-1")["connector_hash"].as_str().unwrap().to_string();
    assert_eq!(hash.len(), 64);
    assert_ne!(hash, pin);
}

#[test]
fn a_pipeline_run_writes_a_reusable_component_cache_entry() {
    let pin = digest(PROBE);
    let dir = project(&manifest("connectors/probe.wasm", &["items"], &format!("sha256 = \"{pin}\"")));
    ok(&fire(dir.path(), "cache-run-1", &[], &[]));
    let cache = dir.path().join(".contextful/cache/components");
    let entries = std::fs::read_dir(&cache).unwrap().map(|entry| entry.unwrap().path()).collect::<Vec<_>>();
    assert!(entries.iter().any(|entry| entry.extension().is_some_and(|extension| extension == "cwasm")));
    ok(&fire(dir.path(), "cache-run-2", &[], &[]));
}

#[test]
fn bytes_off_their_pin_refuse_before_any_run_row() {
    let other = digest(PROBE_BASE);
    let dir = project(&manifest("connectors/probe.wasm", &["items"], &format!("sha256 = \"{other}\"")));
    refused(&fire(dir.path(), "run-1", &[], &[]), "ConnectorDigestMismatch");
    let show = cf(dir.path(), &["run", "show", "run-1", "--project", "research"], &[]);
    assert!(!show.status.success(), "no run row is written for bytes that never loaded");
}

/// With either switch set, an unpinned local artifact raises `ConnectorLocalUnpinned` at build, carrying the digest of
/// the bytes found.
// spec: connector.package.local-unpinned@c59e312a
#[test]
fn an_unpinned_local_artifact_under_either_switch_is_refused_with_its_digest() {
    // The per-connector manifest flag.
    let dir = project(&manifest("connectors/probe.wasm", &["items"], "require_pin = true"));
    let stderr = refused(&fire(dir.path(), "run-1", &[], &[]), "ConnectorLocalUnpinned");
    assert!(stderr.contains(&digest(PROBE)), "{stderr}");

    // The store-wide policy key, with the manifest flag unset.
    let dir = project(&manifest("connectors/probe.wasm", &["items"], ""));
    let config = dir.path().join(".contextful/context/research/config.toml");
    std::fs::write(&config, "[node]\nid = \"ingest-a\"\n\n[connector]\nrequire_pin = true\n").unwrap();
    let stderr = refused(&fire(dir.path(), "run-1", &[], &[]), "ConnectorLocalUnpinned");
    assert!(stderr.contains(&digest(PROBE)), "{stderr}");
    let show = cf(dir.path(), &["run", "show", "run-1", "--project", "research"], &[]);
    assert!(!show.status.success(), "no run row is written for an artifact refused its pin");
    refused(&cf(dir.path(), &["pipeline", "validate", "--project", "research"], &[]), "ConnectorLocalUnpinned");

    // The store key admits a pinned artifact.
    let pin = digest(PROBE);
    std::fs::write(dir.path().join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{}", manifest("connectors/probe.wasm", &["items"], &format!("sha256 = \"{pin}\"")))).unwrap();
    ok(&fire(dir.path(), "run-1", &[], &[]));

    // Neither switch set admits the unpinned artifact.
    let dir = project(&manifest("connectors/probe.wasm", &["items"], ""));
    ok(&fire(dir.path(), "run-1", &[], &[]));
}

/// A component session's grant is its declared `allow` hosts and `attach` headers alone, each header hydrated per
/// request under {{connector.resolve.hydration-is-just-in-time}}; a source declaring no `allow` reaches no host.
// spec: connector.package.component-grant@fd049d0f
#[test]
fn a_guest_reaches_only_its_allowlist_and_the_host_attaches_its_credential() {
    let vendor = Vendor::start();
    let config = "allow = [\"127.0.0.1\"]\nattach = { Authorization = \"Bearer ${secret://vendor-token}\" }";
    let env = [("VENDOR_TOKEN", "vendor-token-value"), ("CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES", "1")];

    let allowed = format!("fetch http://127.0.0.1:{}/v1", vendor.port);
    let dir = project(&manifest("connectors/probe.wasm", &[&allowed], config));
    let out = ok(&fire(dir.path(), "run-1", &[], &env));
    assert!(out.contains("success · 1 rows"), "{out}");
    let got = vendor.requests();
    assert_eq!(got.len(), 1);
    assert!(got[0].contains(&"authorization: bearer vendor-token-value".to_string()), "{got:?}");

    // `localhost` is not on the allowlist, so the request fails the read before any socket opens.
    let off = format!("fetch http://localhost:{}/v1", vendor.port);
    let dir = project(&manifest("connectors/probe.wasm", &[&off], config));
    let stderr = refused(&fire(dir.path(), "run-1", &[], &env), "SecretUnpermittedRequest");
    assert!(!stderr.contains("vendor-token-value"), "{stderr}");
    assert_eq!(vendor.requests().len(), 1, "the refused request reached the vendor");

    // No `allow` reaches no host.
    let dir = project(&manifest("connectors/probe.wasm", &[&allowed], ""));
    refused(&fire(dir.path(), "run-1", &[], &[]), "SecretUnpermittedRequest");
    assert_eq!(vendor.requests().len(), 1);
}

/// `pipeline validate` loads a local component artifact from the directory `pipeline run` resolves it against and runs
/// discovery, so a missing export or a world mismatch fails before any run; a remote artifact is checked without I/O.
// spec: connector.package.component-validate@3132dafd
#[test]
fn validate_loads_a_local_component_and_runs_discovery() {
    let pin = digest(PROBE);
    let dir = project(&manifest("connectors/probe.wasm", &["items"], &format!("sha256 = \"{pin}\"")));
    let out = ok(&cf(dir.path(), &["pipeline", "validate"], &[]));
    assert!(out.contains("probe: valid") && out.contains("discovers items"), "{out}");

    // Core-module bytes are no component.
    std::fs::write(dir.path().join("connectors/core.wasm"), b"\0asm\x01\0\0\0").unwrap();
    std::fs::write(dir.path().join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{}", manifest("connectors/core.wasm", &["items"], ""))).unwrap();
    refused(&cf(dir.path(), &["pipeline", "validate"], &[]), "the component does not load");

    // A guest table against a guest exporting no configuration interface.
    std::fs::write(dir.path().join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{}", manifest("connectors/probe-base.wasm", &["items"], "guest = { region = \"eu\" }"))).unwrap();
    refused(&cf(dir.path(), &["pipeline", "validate"], &[]), "ConnectorConfigUnclaimed");

    // A remote artifact is parsed and pinned, never fetched.
    let remote = manifest("https://dl.vendor.invalid/probe.wasm", &["items"], &format!("sha256 = \"{pin}\""));
    std::fs::write(dir.path().join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{}", remote)).unwrap();
    let out = ok(&cf(dir.path(), &["pipeline", "validate"], &[]));
    assert!(out.contains("probe: valid"), "{out}");

    // A manifest below the project names its artifact against the project directory, as `pipeline run` reads it.
    let dir = project("");
    std::fs::create_dir_all(dir.path().join("conf")).unwrap();
    std::fs::write(dir.path().join("conf/contextful.toml"), format!("authoring_posture = \"per_request\"\n{}", manifest("connectors/probe.wasm", &["items"], &format!("sha256 = \"{pin}\"")))).unwrap();
    let validate = ["pipeline", "validate", "--declaration", "conf/contextful.toml"];
    assert!(ok(&cf(dir.path(), &validate, &[])).contains("discovers items"));
    assert!(ok(&cf(dir.path(), &[&validate[..], &["--project", "research"]].concat(), &[])).contains("discovers items"));
    let out = ok(&fire(dir.path(), "run-1", &["--declaration", "conf/contextful.toml"], &[]));
    assert!(out.contains("3 rows in 2 batches"), "{out}");

    // Bytes beside the manifest alone are found by neither command.
    std::fs::rename(dir.path().join("connectors"), dir.path().join("conf/connectors")).unwrap();
    refused(&cf(dir.path(), &validate, &[]), "reading component artifact `connectors/probe.wasm`");
    refused(&fire(dir.path(), "run-2", &["--declaration", "conf/contextful.toml"], &[]), "reading component artifact `connectors/probe.wasm`");
}

/// `incremental` beside a component source raises `ConnectorPositionOwned` at validation; the guest's opaque cursor is
/// the run's position.
// spec: connector.package.component-position@8a1f439a
#[test]
fn an_incremental_field_beside_a_component_source_is_refused() {
    let text = format!("{}\n", manifest("connectors/probe.wasm", &["items"], "")).replace("tables = [\"items\"]", "tables = [\"items\"]\nincremental = \"id\"");
    let dir = project(&text);
    refused(&cf(dir.path(), &["pipeline", "validate"], &[]), "ConnectorPositionOwned");
    refused(&fire(dir.path(), "run-1", &[], &[]), "ConnectorPositionOwned");
}

#[test]
fn a_component_key_outside_its_set_is_refused_before_any_io() {
    let dir = project(&manifest("connectors/probe.wasm", &["items"], "endpoint = \"https://api.vendor.example\""));
    refused(&cf(dir.path(), &["pipeline", "validate"], &[]), "PipelineUnknownConfigKey");
}

#[test]
fn a_remote_artifact_is_refused_at_run_naming_its_form() {
    let pin = digest(PROBE);
    for name in ["https://dl.vendor.invalid/probe.wasm", "oci://registry.vendor.invalid/probe:1"] {
        let dir = project(&manifest(name, &["items"], &format!("sha256 = \"{pin}\"")));
        let stderr = refused(&fire(dir.path(), "run-1", &[], &[]), "resolves local artifacts");
        assert!(stderr.contains(name), "{stderr}");
    }
}

/// `pipeline run` and `pipeline validate` compile components for the interpreted target when `--component-target
/// pulley` or `CONTEXTFUL_COMPONENT_TARGET=pulley` selects it; native stays the default.
// spec: connector.package.component-target@c0b51a53
#[test]
fn the_interpreted_target_is_selected_by_flag_or_environment() {
    let pin = digest(PROBE);
    let dir = project(&manifest("connectors/probe.wasm", &["items"], &format!("sha256 = \"{pin}\"")));
    let by_flag = fire(dir.path(), "run-1", &["--component-target", "pulley"], &[]);
    let by_env = fire(dir.path(), "run-2", &[], &[("CONTEXTFUL_COMPONENT_TARGET", "pulley")]);
    let validated = cf(dir.path(), &["pipeline", "validate", "--component-target", "pulley"], &[]);
    #[cfg(feature = "pulley")]
    {
        assert!(ok(&by_flag).contains("run-1 success · 3 rows"));
        assert!(ok(&by_env).contains("run-2 success · 0 rows"));
        ok(&validated);
    }
    #[cfg(not(feature = "pulley"))]
    {
        refused(&by_flag, "`pulley` feature");
        refused(&by_env, "`pulley` feature");
        refused(&validated, "`pulley` feature");
    }
    assert!(ok(&fire(dir.path(), "run-3", &["--component-target", "native"], &[])).contains("run-3 success"));
}
