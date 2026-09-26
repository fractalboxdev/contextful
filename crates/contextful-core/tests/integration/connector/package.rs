//! Artifact references, content pins and the forwarded guest table.

use contextful_core::connector::package::{content_hash, guest_config, Artifact, Digest, Form, PinRequirement, GUEST_CONFIG_BYTES};
use contextful_core::connector::ConnectorError;
use serde_json::json;

const BYTES: &[u8] = b"\0asm component bytes";

fn pin_of(bytes: &[u8]) -> String {
    Digest::of(bytes).to_string()
}

/// A reference parses into one of four forms: in-tree by name, a project-local path, an HTTPS URL, or an OCI
/// reference; any other scheme refuses.
#[test]
fn a_reference_parses_into_one_of_four_forms() {
    let pin = pin_of(BYTES);
    assert_eq!(Artifact::parse("http", None).unwrap().form, Form::InTree("http".into()));
    assert_eq!(Artifact::parse("connectors/vendor.wasm", None).unwrap().form, Form::Local("connectors/vendor.wasm".into()));
    assert_eq!(
        Artifact::parse("https://dl.vendor.example/vendor.wasm", Some(&pin)).unwrap().form,
        Form::Https("https://dl.vendor.example/vendor.wasm".into())
    );
    assert_eq!(
        Artifact::parse("oci://ghcr.io/vendor/metrics:1.4.0", Some(&pin)).unwrap().form,
        Form::Oci("oci://ghcr.io/vendor/metrics:1.4.0".into())
    );
    assert!(matches!(Artifact::parse("ftp://host/x.wasm", Some(&pin)), Err(ConnectorError::ConnectorInsecureArtifact(_))));
}

/// A remote artifact carrying no 64-hex content pin raises `ConnectorRemoteUnpinned` at parse.
// spec: connector.package.remote-unpinned@5c835d3f
#[test]
fn a_remote_artifact_without_a_64_hex_pin_is_refused_at_parse() {
    for reference in ["https://dl.vendor.example/vendor.wasm", "oci://ghcr.io/vendor/metrics:1.4.0"] {
        assert!(matches!(Artifact::parse(reference, None), Err(ConnectorError::ConnectorRemoteUnpinned(_))), "{reference}");
        let short = &pin_of(BYTES)[..63];
        assert!(matches!(Artifact::parse(reference, Some(short)), Err(ConnectorError::ConnectorRemoteUnpinned(_))), "{reference}");
        let not_hex = "g".repeat(64);
        assert!(matches!(Artifact::parse(reference, Some(&not_hex)), Err(ConnectorError::ConnectorRemoteUnpinned(_))), "{reference}");
        assert!(Artifact::parse(reference, Some(&pin_of(BYTES).to_uppercase())).is_ok(), "{reference}");
    }
}

/// A plain-HTTP artifact reference raises `ConnectorInsecureArtifact`.
// spec: connector.package.insecure-artifact@c683abe3
#[test]
fn a_plain_http_artifact_is_refused() {
    let pin = pin_of(BYTES);
    for reference in ["http://dl.vendor.example/vendor.wasm", "HTTP://dl.vendor.example/vendor.wasm"] {
        assert!(matches!(Artifact::parse(reference, Some(&pin)), Err(ConnectorError::ConnectorInsecureArtifact(_))), "{reference}");
        assert!(matches!(Artifact::parse(reference, None), Err(ConnectorError::ConnectorInsecureArtifact(_))), "{reference}");
    }
}

/// Bytes that do not hash to their pin refuse with both digests named, remote or local.
#[test]
fn bytes_that_do_not_hash_to_the_pin_are_refused() {
    let off = Digest::of(b"other bytes").to_string();
    for reference in ["https://dl.vendor.example/vendor.wasm", "connectors/vendor.wasm"] {
        let a = Artifact::parse(reference, Some(&off)).unwrap();
        match a.admit(BYTES, PinRequirement::default()) {
            Err(ConnectorError::ConnectorDigestMismatch(m)) => assert!(m.contains(&pin_of(BYTES)) && m.contains(&off), "{m}"),
            other => panic!("{reference}: {other:?}"),
        }
        let pinned = Artifact::parse(reference, Some(&pin_of(BYTES))).unwrap();
        assert_eq!(pinned.admit(BYTES, PinRequirement::default()).unwrap(), Digest::of(BYTES));
    }
}

/// Two switches require a pin on a local artifact, composed by disjunction: a store-wide policy key and a
/// per-connector manifest flag.
// spec: connector.package.pin-requirement@57825a53
#[test]
fn either_switch_requires_a_local_pin() {
    let local = Artifact::parse("connectors/vendor.wasm", None).unwrap();
    assert!(local.admit(BYTES, PinRequirement { store: false, manifest: false }).is_ok());
    for (store, manifest) in [(true, false), (false, true), (true, true)] {
        let r = PinRequirement { store, manifest };
        assert!(r.required());
        assert!(local.admit(BYTES, r).is_err(), "store={store} manifest={manifest}");
    }
}

/// `Artifact::admit` raises `ConnectorLocalUnpinned` under either switch, carrying the digest of the bytes found. No
/// build step calls it, so `connector.package.local-unpinned` stays unpinned here.
#[test]
fn an_unpinned_local_artifact_under_a_requirement_names_the_digest_found() {
    let local = Artifact::parse("connectors/vendor.wasm", None).unwrap();
    match local.admit(BYTES, PinRequirement { store: true, manifest: false }) {
        Err(ConnectorError::ConnectorLocalUnpinned(m)) => assert!(m.contains(&pin_of(BYTES)), "{m}"),
        other => panic!("{other:?}"),
    }
    let in_tree = Artifact::parse("http", None).unwrap();
    assert!(in_tree.admit(BYTES, PinRequirement { store: true, manifest: true }).is_ok(), "an in-tree name carries no artifact");
}

/// Ahead of any I/O, a guest configuration value that is not a table, a serialization over 64 KiB, or a credential or
/// environment reference inside it raises `ConnectorConfigRejected`.
// spec: connector.import.config-shape@53f6bfb8
#[test]
fn a_guest_table_off_its_shape_is_refused() {
    for value in [json!("region=eu"), json!(["eu"]), json!(7), json!(null)] {
        assert!(matches!(guest_config(&value), Err(ConnectorError::ConnectorConfigRejected(_))), "{value}");
    }
    let big = json!({ "blob": "x".repeat(GUEST_CONFIG_BYTES) });
    assert!(matches!(guest_config(&big), Err(ConnectorError::ConnectorConfigRejected(_))));
    let fits = json!({ "blob": "x".repeat(GUEST_CONFIG_BYTES - 20) });
    assert!(guest_config(&fits).is_ok());
    for value in [
        json!({ "token": "secret://vendor-token" }),
        json!({ "account": "env://VENDOR_ACCOUNT" }),
        json!({ "nested": { "list": ["plain", "Bearer ${secret://vendor-token}"] } }),
        json!({ "${secret://key}": "value" }),
    ] {
        match guest_config(&value) {
            Err(ConnectorError::ConnectorConfigRejected(m)) => assert!(!m.contains("vendor-token"), "{m}"),
            other => panic!("{value}: {other:?}"),
        }
    }
    assert_eq!(guest_config(&json!({ "region": "eu", "limit": 50 })).unwrap(), r#"{"limit":50,"region":"eu"}"#);
}

/// `content_hash` folds the forwarded table into the artifact digest and returns the digest verbatim with no table. No
/// pipeline hash reads it, so `connector.import.config-hashing` stays unpinned here.
#[test]
fn the_forwarded_table_folds_into_the_content_hash() {
    let digest = Digest::of(BYTES);
    assert_eq!(content_hash(&digest, None), digest.as_str());
    let a: serde_json::Value = serde_json::from_str(r#"{"region":"eu","limit":50}"#).unwrap();
    let b: serde_json::Value = serde_json::from_str(r#"{"limit":50,"region":"eu"}"#).unwrap();
    let with_a = content_hash(&digest, Some(&a));
    assert_ne!(with_a, digest.as_str());
    assert_eq!(with_a.len(), 64);
    assert_eq!(with_a, content_hash(&digest, Some(&b)), "key order moves no hash");
    assert_ne!(with_a, content_hash(&digest, Some(&json!({ "region": "us", "limit": 50 }))));
    assert_ne!(with_a, content_hash(&Digest::of(b"other"), Some(&a)));
}
