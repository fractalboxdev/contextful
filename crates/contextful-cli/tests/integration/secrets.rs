//! `contextful secrets list` through the built binary: configured bindings, operator
//! records, observations and the latest run attribution.
#![cfg(feature = "data-plane")]

use super::pipeline::{project, Vendor};
use std::path::Path;
use std::process::{Command, Output};

const SENTINEL: &str = "sentinel-credential-7f3a";

fn cf(dir: &Path, envs: &[(&str, &str)], args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_contextful"));
    c.args(args).current_dir(dir).env_remove("CONTEXTFUL_NODE_ID").env_remove("CONTEXTFUL_SECRETS_BACKEND").env_remove("VENDOR_TOKEN");
    for (k, v) in envs {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn lines(out: &Output) -> Vec<serde_json::Value> {
    ok(out).lines().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}"))).collect()
}

/// An http pipeline binding `vendor-token` in a header template, then `extra`.
fn manifest(endpoint: &str, extra: &str) -> String {
    format!(
        "[[pipeline]]\nid = \"orders\"\ntables = [\"items\"]\n[pipeline.source]\nname = \"http\"\n[pipeline.source.config]\nendpoint = \"{endpoint}\"\n\
         [pipeline.source.config.headers]\nAuthorization = \"Bearer ${{secret://vendor-token}}\"\n\n{extra}"
    )
}

const RECORD: &str = "[secret.vendor-token]\ndescription = \"Vendor API key\"\naccount = \"acct-4471\"\ngrants = [\"orders.read\"]\n\
    surfaces = [{ surface = \"orders pipeline\", binding = \"Authorization header\" }]\nrotate = \"console.vendor.example\"\nexpires = 2030-01-10\n";

/// The listing enumerates configured bindings with their adapters, operator metadata and
/// observations, and reads no credential value.
// spec: connector.record.inventory@39593444
#[test]
fn the_inventory_lists_configured_bindings_and_never_a_value() {
    let dir = project(&manifest("https://api.vendor.example/v1/orders", RECORD));
    let env = [("VENDOR_TOKEN", SENTINEL)];
    let out = cf(dir.path(), &env, &["secrets", "list", "--project", "research", "--now", "2030-01-01T00:00:00Z"]);
    let text = ok(&out);
    assert!(text.starts_with("vendor-token\n"), "{text}");
    for line in ["configured  env", "answered    none recorded", "operator    description: Vendor API key", "operator    grants: orders.read", "operator    surfaces: orders pipeline (Authorization header)", "observed    env at 2030-01-01T00:00:00Z: present", "[expires within 30 d]"] {
        assert!(text.contains(line), "missing `{line}`: {text}");
    }
    let json = cf(dir.path(), &env, &["secrets", "list", "--project", "research", "--json"]);
    for out in [&out, &json] {
        assert!(!String::from_utf8_lossy(&out.stdout).contains(SENTINEL) && !String::from_utf8_lossy(&out.stderr).contains(SENTINEL));
    }
}

/// `secrets list` exits 0 whatever its marks; `--json` prints one object per binding per line.
// spec: connector.record.inventory-output@bde7d450
#[test]
fn the_json_listing_prints_one_object_per_binding_and_exits_zero_on_marks() {
    let extra = format!("{RECORD}\n[[pipeline]]\nid = \"speech\"\ntables = [\"items\"]\n[pipeline.source]\nname = \"http\"\n[pipeline.source.config]\nendpoint = \"https://api.speech.example/v1\"\n[pipeline.source.config.headers]\nX-Key = \"${{secret://speech-vendor}}\"\n");
    let dir = project(&manifest("https://api.vendor.example/v1/orders", &extra));
    let rows = lines(&cf(dir.path(), &[], &["secrets", "list", "--project", "research", "--json", "--now", "2030-02-01T00:00:00Z"]));
    assert_eq!(rows.len(), 2);
    let names: Vec<&str> = rows.iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["speech-vendor", "vendor-token"]);
    let vendor = &rows[1];
    assert_eq!(vendor["configured"], serde_json::json!(["env"]));
    assert!(vendor["answered"].is_null());
    assert_eq!(vendor["operator"]["account"], "acct-4471");
    assert_eq!(vendor["observations"][0]["adapter"], "env");
    assert_eq!(vendor["observations"][0]["at"], "2030-02-01T00:00:00Z");
    assert_eq!(vendor["expiries"][0]["mark"], "elapsed");
    assert!(rows[0]["operator"].is_null());
}

/// A binding without a record lists with unknown fields and an unknown expiry.
#[test]
fn a_binding_without_a_record_reads_unknown() {
    let dir = project(&manifest("https://api.vendor.example/v1/orders", ""));
    let text = ok(&cf(dir.path(), &[], &["secrets", "list", "--project", "research"]));
    assert!(text.contains("operator    description: unknown") && text.contains("operator    grants: unknown") && text.contains("expires     operator: unknown\n"), "{text}");
}

/// A record naming no configured binding refuses the listing and the run's declaration read.
#[test]
fn a_record_describing_no_binding_fails_the_listing_and_the_run() {
    let dir = project(&manifest("https://api.vendor.example/v1/orders", &format!("[secret.vendor-tokne]\ndescription = \"{SENTINEL}\"\n")));
    for args in [&["secrets", "list", "--project", "research"][..], &["pipeline", "validate"][..], &["pipeline", "run", "orders", "--project", "research", "--site-id", "site"][..]] {
        let out = cf(dir.path(), &[], args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success() && err.contains("SecretRecordRejected") && err.contains("secret.vendor-tokne"), "{args:?}: {err}");
        assert!(!err.contains(SENTINEL), "{err}");
    }
}

/// Editing an operator record changes no pipeline's content hash.
// spec: connector.record.record-is-inert@cb1e6f4b
#[test]
fn an_edited_record_leaves_the_content_hash_unchanged() {
    let hash = |extra: &str| {
        let dir = project(&manifest("https://api.vendor.example/v1/orders", extra));
        let out = ok(&cf(dir.path(), &[], &["pipeline", "validate"]));
        out.lines().find(|l| l.starts_with("orders: valid")).map(str::to_string).unwrap_or_else(|| panic!("{out}"))
    };
    let bare = hash("");
    assert_eq!(hash(RECORD), bare);
    assert_eq!(hash(&RECORD.replace("Vendor API key", "Rotated vendor key")), bare);
}

/// A run's attribution shows as the answering adapter; configuration alone asserts none.
// spec: connector.record.configured-is-not-observed@a7773b63
#[test]
fn the_latest_run_attribution_shows_apart_from_configuration() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"a\"}]".into()));
    let dir = project(&manifest(&vendor.url("/v1/orders"), ""));
    let env = [("VENDOR_TOKEN", SENTINEL), ("CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES", "1")];
    let before = lines(&cf(dir.path(), &env, &["secrets", "list", "--project", "research", "--json"]));
    assert!(before[0]["answered"].is_null(), "{}", before[0]);
    ok(&cf(dir.path(), &env, &["pipeline", "run", "orders", "--project", "research", "--run-id", "orders-1", "--site-id", "site"]));
    let after = lines(&cf(dir.path(), &env, &["secrets", "list", "--project", "research", "--json"]));
    assert_eq!(after[0]["answered"]["adapter"], "env");
    assert_eq!(after[0]["answered"]["run_id"], "orders-1");
    let history = ok(&cf(dir.path(), &env, &["run", "history", "--project", "research", "--export"]));
    assert!(history.contains("\"credential_providers\":{\"vendor-token\":\"env\"}"), "{history}");
    assert!(!history.contains(SENTINEL));
}

/// A binding a `pipelines/*.json` pipeline declares lists, and its record validates.
#[test]
fn a_json_pipelines_binding_lists_and_takes_a_record() {
    let dir = project(RECORD);
    std::fs::create_dir_all(dir.path().join("pipelines")).unwrap();
    std::fs::write(
        dir.path().join("pipelines/orders.json"),
        r#"{"id": "orders", "tables": ["items"], "source": {"name": "http", "config": {"endpoint": "https://api.vendor.example/v1/orders", "headers": {"Authorization": "Bearer ${secret://vendor-token}"}}}}"#,
    )
    .unwrap();
    let rows = lines(&cf(dir.path(), &[], &["secrets", "list", "--project", "research", "--json"]));
    let names: Vec<&str> = rows.iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["vendor-token"]);
    assert_eq!(rows[0]["operator"]["account"], "acct-4471");
    let validated = ok(&cf(dir.path(), &[], &["pipeline", "validate"]));
    assert!(validated.contains("orders: valid"), "{validated}");
}

/// A `[secret.<name>]` table outside the discovered `contextful.toml` refuses, so a
/// misspelled record in `pipelines/` never passes unseen.
#[test]
fn a_record_outside_the_declaration_refuses() {
    for record in [format!("[secret.vendor-tokne]\ndescription = \"{SENTINEL}\"\n"), RECORD.to_string()] {
        let dir = project(&manifest("https://api.vendor.example/v1/orders", ""));
        std::fs::create_dir_all(dir.path().join("pipelines")).unwrap();
        std::fs::write(dir.path().join("pipelines/orders.toml"), &record).unwrap();
        for args in [&["secrets", "list", "--project", "research"][..], &["pipeline", "validate"][..]] {
            let out = cf(dir.path(), &[], args);
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(!out.status.success() && err.contains("SecretRecordRejected") && err.contains("pipelines/orders.toml"), "{args:?}: {err}");
            assert!(!err.contains(SENTINEL), "{err}");
        }
    }
}

/// A record's validity does not depend on the invoking environment: a record for the lease
/// mint name, which no declaration binds, refuses with and without the lease backend.
#[test]
fn record_validity_ignores_the_selected_backend() {
    let dir = project(&manifest("https://api.vendor.example/v1/orders", "[secret.lease-mint]\ndescription = \"mint\"\n"));
    let lease = [("CONTEXTFUL_SECRETS_BACKEND", "lease,env"), ("CONTEXTFUL_LEASE_SCOPES", "vendor-token"), ("CONTEXTFUL_LEASE_PROVIDER_URL", "https://mint.invalid/"), ("LEASE_MINT", "mint")];
    for envs in [&[][..], &lease[..]] {
        let out = cf(dir.path(), envs, &["pipeline", "validate"]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success() && err.contains("SecretRecordRejected") && err.contains("secret.lease-mint"), "{envs:?}: {err}");
    }
}
