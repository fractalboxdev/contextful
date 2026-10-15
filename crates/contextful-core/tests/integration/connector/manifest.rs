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
env         = ["VENDOR_ACCOUNT_ID"]
clock       = true

[capabilities.scope_probe]
endpoint      = "https://api.vendor.example/v1/identity"
scopes_header = "X-Granted-Scopes"
expect        = ["reports.read", "accounts.read"]

[capabilities.limiter]
quota         = "vendor-app-shared"
class         = "batch-read"
usage_headers = ["X-RateLimit-Remaining", "X-RateLimit-Reset"]
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
    assert_eq!(d.env, ["VENDOR_ACCOUNT_ID"]);
    assert!(d.clock);
    let probe = d.scope_probe.as_ref().unwrap();
    assert_eq!(probe.scopes_header, "X-Granted-Scopes");
    assert_eq!(probe.expect, ["reports.read", "accounts.read"]);
    let limiter = d.limiter.as_ref().unwrap();
    assert_eq!(limiter.quota, "vendor-app-shared");
    assert!(limiter.forwards("x-ratelimit-reset"));
}

#[test]
fn a_manifest_with_no_capabilities_declares_nothing() {
    let m = Manifest::parse("name = \"probe\"\n").unwrap();
    assert!(m.declared.hosts.0.is_empty() && m.declared.env.is_empty() && !m.declared.clock);
    assert!(m.declared.scope_probe.is_none() && m.declared.limiter.is_none() && m.build.is_none());
}

#[test]
fn an_unread_key_at_any_depth_refuses_naming_the_key() {
    let nested = rejected("[capabilities.limiter]\nquota = \"q\"\nclass = \"c\"\nburst = 4\n");
    assert!(nested.contains("`capabilities.limiter.burst`"), "{nested}");
    let attach = rejected("[attach]\nAuthorization = \"Bearer ${secret://vendor-token}\"\n");
    assert!(attach.contains("`attach`"), "{attach}");
    let bounds = rejected("[bounds]\nmemory_bytes = 268435456\n");
    assert!(bounds.contains("`bounds`"), "{bounds}");
    let build = rejected("[build]\nsource = \"guest\"\nimage = \"rust:1\"\n");
    assert!(build.contains("`build.image`"), "{build}");
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
    assert_eq!(Manifest::load(&path).unwrap().name.as_deref(), Some("vendor-metrics"));
    std::fs::write(&path, "[capabilities\n").unwrap();
    assert!(matches!(Manifest::load(&path), Err(ConnectorError::ConnectorManifestRejected(_))));
    std::fs::remove_dir_all(&dir).unwrap();
}
