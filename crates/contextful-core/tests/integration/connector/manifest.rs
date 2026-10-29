//! The connector manifest: its closed key set, the shape of each value, and its one build input.

use contextful_core::connector::manifest::Manifest;
use contextful_core::connector::ConnectorError;
use std::path::Path;

/// The adopted-posture manifest of the specification's Shapes section.
const ADOPTED: &str = r#"
name    = "vendor-metrics"
version = "1.4.0"
world   = "source-connector@1.2.0"

wasm             = "vendor-metrics.wasm"
wasm_sha256      = "9f2c1b7ad0e49f2c1b7ad0e49f2c1b7ad0e49f2c1b7ad0e49f2c1b7ad0e4abcd"
require_wasm_pin = true

[build]
source = "guest"

[capabilities]
allow_hosts = ["api.vendor.example"]
clock       = true

[capabilities.scope_probe]
endpoint      = "https://api.vendor.example/v1/identity"
scopes_header = "X-Granted-Scopes"
expect        = ["reports.read", "accounts.read"]
"#;

/// The refusal for `text`, which must be `ConnectorManifestRejected`.
fn rejected(text: &str) -> String {
    match Manifest::parse(text) {
        Err(e @ ConnectorError::ConnectorManifestRejected(_)) => e.to_string(),
        other => panic!("expected ConnectorManifestRejected, got {other:?}"),
    }
}

#[test]
fn the_adopted_manifest_reads_every_declared_capability() {
    let m = Manifest::parse(ADOPTED).unwrap();
    assert_eq!(m.name.as_deref(), Some("vendor-metrics"));
    assert_eq!(m.wasm.as_deref(), Some("vendor-metrics.wasm"));
    assert!(m.require_wasm_pin);
    assert_eq!(m.build.as_ref().unwrap().source, Path::new("guest"));
    let d = &m.declared;
    assert!(d.hosts.permits("api.vendor.example") && !d.hosts.permits("other.example"));
    assert!(d.clock);
    let probe = d.scope_probe.as_ref().unwrap();
    assert_eq!(probe.scopes_header, "X-Granted-Scopes");
    assert_eq!(probe.expect, ["reports.read", "accounts.read"]);
}

/// An adopted manifest carries a digest from the fixed builder platform. A template manifest carries a pin requirement
/// and placeholder. The parser accepts a template; the loader refuses it.
// spec: connector.package.manifest-posture@c17c9a82
#[test]
fn a_template_manifest_parses_and_its_placeholder_pin_refuses_at_load() {
    let adopted = Manifest::parse(ADOPTED).unwrap();
    assert!(adopted.pin().unwrap().is_some(), "an adopted manifest carries its digest");
    let template = ADOPTED.replace("9f2c1b7ad0e49f2c1b7ad0e49f2c1b7ad0e49f2c1b7ad0e49f2c1b7ad0e4abcd", "<written by connector pin>");
    let m = Manifest::parse(&template).unwrap();
    assert!(m.require_wasm_pin, "a template carries the pin requirement");
    match m.pin() {
        Err(e @ ConnectorError::ConnectorManifestRejected(_)) => assert!(e.to_string().contains("wasm_sha256"), "{e}"),
        other => panic!("expected ConnectorManifestRejected, got {other:?}"),
    }
}

#[test]
fn a_manifest_with_no_capabilities_declares_nothing() {
    let m = Manifest::parse("name = \"probe\"\n").unwrap();
    assert!(m.declared.hosts.0.is_empty() && !m.declared.clock);
    assert!(m.declared.scope_probe.is_none() && m.build.is_none());
}

/// A manifest's `dedup_window_secs` declares the seconds its destination deduplicates a repeated
/// idempotency key; a manifest declaring none carries no window.
// spec: connector.declare-capability.dedup-window@b04e69cb
#[test]
fn a_manifest_declares_its_deduplication_window_and_defaults_to_none() {
    assert_eq!(Manifest::parse(&format!("dedup_window_secs = 600\n{ADOPTED}")).unwrap().dedup_window_secs, Some(600));
    assert_eq!(Manifest::parse(ADOPTED).unwrap().dedup_window_secs, None);
    assert!(rejected(&format!("dedup_window_secs = -1\n{ADOPTED}")).contains("dedup_window_secs"));
}

#[test]
fn an_unread_key_at_any_depth_refuses_naming_the_key() {
    let nested = rejected("[capabilities.scope_probe]\nendpoint = \"https://a.example/id\"\nscopes_header = \"X-S\"\nburst = 4\n");
    assert!(nested.contains("`capabilities.scope_probe.burst`"), "{nested}");
    let attach = rejected("[attach]\nAuthorization = \"Bearer ${secret://vendor-token}\"\n");
    assert!(attach.contains("`attach`"), "{attach}");
    let bounds = rejected("[bounds]\nmemory_bytes = 268435456\n");
    assert!(bounds.contains("`bounds`"), "{bounds}");
    let build = rejected("[build]\nsource = \"guest\"\nimage = \"rust:1\"\n");
    assert!(build.contains("`build.image`"), "{build}");
}

/// A declaration the load binds nothing to refuses rather than reading as granted: an
/// environment name and a shared quota.
#[test]
fn an_environment_name_or_a_shared_quota_refuses_naming_the_key() {
    let env = rejected("[capabilities]\nenv = [\"VENDOR_ACCOUNT_ID\"]\n");
    assert!(env.contains("`capabilities.env`"), "{env}");
    let limiter = rejected("[capabilities.limiter]\nquota = \"q\"\nclass = \"c\"\n");
    assert!(limiter.contains("`capabilities.limiter`"), "{limiter}");
}

#[test]
fn a_value_off_its_shape_refuses_naming_the_key() {
    let clock = rejected("[capabilities]\nclock = \"yes\"\n");
    assert!(clock.contains("`capabilities.clock`"), "{clock}");
    let hosts = rejected("[capabilities]\nallow_hosts = \"api.vendor.example\"\n");
    assert!(hosts.contains("`capabilities.allow_hosts`"), "{hosts}");
    let pin = rejected("require_wasm_pin = \"true\"\n");
    assert!(pin.contains("`require_wasm_pin`"), "{pin}");
}

#[test]
fn a_build_source_leaving_the_manifest_directory_refuses() {
    for source in ["../guest", "/src/guest", "guest/../../x", ""] {
        let out = rejected(&format!("[build]\nsource = \"{source}\"\n"));
        assert!(out.contains("`build.source`"), "{source}: {out}");
    }
}

#[test]
fn the_build_source_resolves_against_the_manifest_directory() {
    let m = Manifest::parse("[build]\nsource = \"guests/vendor\"\n").unwrap();
    let dir = m.build_dir(Path::new("connectors/vendor/connector.toml")).unwrap();
    assert_eq!(dir, Path::new("connectors/vendor/guests/vendor"));
    assert!(Manifest::parse("name = \"x\"\n").unwrap().build_dir(Path::new("connector.toml")).is_none());
}

#[test]
fn a_manifest_file_loads_from_disk_and_an_unreadable_one_refuses() {
    let dir = std::env::temp_dir().join(format!("contextful-manifest-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("connector.toml");
    std::fs::write(&path, ADOPTED).unwrap();
    assert_eq!(Manifest::from_read(&path, std::fs::read_to_string(&path)).unwrap().name.as_deref(), Some("vendor-metrics"));
    std::fs::write(&path, "[capabilities\n").unwrap();
    assert!(matches!(Manifest::from_read(&path, std::fs::read_to_string(&path)), Err(ConnectorError::ConnectorManifestRejected(_))));
    std::fs::remove_dir_all(&dir).unwrap();
}
