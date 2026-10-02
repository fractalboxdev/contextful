//! A pipeline source naming a component artifact: its pin, grant, guest table and bounds.

use contextful_core::connector::component::{ComponentSource, SourceError, KEYS};
use contextful_core::connector::package::Form;
use contextful_core::connector::ConnectorError;
use contextful_core::run::RunError;
use serde_json::json;

const PIN: &str = "5f0c5a4a3e4ad7e0e2d5a4bd6c0c6f5ad7fd1e0c7d3c5c0b0b8c1d2e3f405162";

fn parse(name: &str, config: serde_json::Value) -> Result<Option<ComponentSource>, SourceError> {
    ComponentSource::parse(name, &config)
}

#[test]
fn an_in_tree_name_is_no_component() {
    assert!(parse("http", json!({})).unwrap().is_none());
    assert!(parse("vendor-metrics", json!({ "sha256": PIN })).unwrap().is_none());
}

/// A pipeline source named by an artifact path, HTTPS URL or OCI reference runs as a component, its config reading
/// `sha256`, `allow`, `attach`, `guest`, `memory_bytes` and `require_pin`, the manifest flag of
/// {{connector.package.pin-requirement}}.
// spec: connector.package.component-source@1ce79a60
#[test]
fn a_component_source_reads_its_pin_grant_guest_table_and_bounds() {
    let c = parse(
        "connectors/vendor.wasm",
        json!({
            "sha256": PIN,
            "allow": ["api.vendor.example"],
            "attach": { "Authorization": "Bearer ${secret://vendor-token}" },
            "guest": { "region": "eu" },
            "memory_bytes": 536870912u64,
            "require_pin": true,
        }),
    )
    .unwrap()
    .expect("a path names a component");
    assert_eq!(c.artifact.form, Form::Local("connectors/vendor.wasm".into()));
    assert_eq!(c.artifact.pin.as_ref().map(|d| d.as_str()), Some(PIN));
    assert!(c.allow.permits("api.vendor.example") && !c.allow.permits("other.example"));
    assert_eq!(c.attach.len(), 1);
    assert_eq!(c.attach[0].0, "Authorization");
    assert!(c.attach[0].1.has_reference());
    assert_eq!(c.guest, Some(json!({ "region": "eu" })));
    assert_eq!(c.memory_bytes, Some(536_870_912));
    assert!(c.requirement(false).manifest);

    let https = parse("https://dl.vendor.example/vendor.wasm", json!({ "sha256": PIN })).unwrap().unwrap();
    assert!(matches!(https.artifact.form, Form::Https(_)));
    let bare = parse("vendor.wasm", json!({})).unwrap().unwrap();
    assert!(bare.allow.0.is_empty(), "no `allow` reaches no host");
    assert!(!bare.allow.permits("api.vendor.example"));
    assert!(bare.attach.is_empty() && bare.guest.is_none() && bare.memory_bytes.is_none());
    assert!(!bare.requirement(false).required());
    assert!(bare.requirement(true).required(), "the store-wide key requires a pin without the manifest flag");
}

#[test]
fn a_key_outside_the_component_set_is_refused_naming_the_accepted_keys() {
    match parse("vendor.wasm", json!({ "endpoint": "https://api.vendor.example" })) {
        Err(SourceError::Run(RunError::PipelineUnknownConfigKey(m))) => {
            assert!(m.contains("`endpoint`"), "{m}");
            assert!(KEYS.iter().all(|k| m.contains(k)), "{m}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_pin_and_the_guest_table_refuse_at_parse() {
    assert!(matches!(parse("https://dl.vendor.example/v.wasm", json!({})), Err(SourceError::Connector(ConnectorError::ConnectorRemoteUnpinned(_)))));
    assert!(matches!(parse("vendor.wasm", json!({ "sha256": "abc" })), Err(SourceError::Connector(ConnectorError::ConnectorDigestMismatch(_)))));
    assert!(matches!(
        parse("vendor.wasm", json!({ "guest": { "token": "${secret://vendor-token}" } })),
        Err(SourceError::Connector(ConnectorError::ConnectorConfigRejected(_)))
    ));
    assert!(matches!(parse("vendor.wasm", json!({ "allow": [] })), Err(SourceError::Connector(ConnectorError::ConnectorAllowlistRejected(_)))));
    for bad in [json!({ "memory_bytes": "big" }), json!({ "require_pin": "yes" }), json!({ "allow": "api.vendor.example" }), json!({ "attach": ["x"] })] {
        assert!(matches!(parse("vendor.wasm", bad.clone()), Err(SourceError::Run(RunError::Invalid(_)))), "{bad}");
    }
}

/// An attach value embedding no reference raises `SecretLiteralAttachValue`.
// spec: connector.attach.literal-attach-value@5f025ea8
#[test]
fn an_attach_value_embedding_no_reference_is_refused() {
    let r = parse("vendor.wasm", json!({ "allow": ["api.vendor.example"], "attach": { "X-Client": "reports" } }));
    assert!(matches!(r, Err(SourceError::Connector(ConnectorError::SecretLiteralAttachValue(_)))), "{r:?}");
    let material = parse("vendor.wasm", json!({ "allow": ["api.vendor.example"], "attach": { "Authorization": "Bearer ghp_0123456789abcdefghijABCDEFGHIJ012345" } }));
    assert!(matches!(material, Err(SourceError::Connector(ConnectorError::SecretMaterialInDeclaration(_)))), "{material:?}");
}

#[test]
fn a_credential_beside_a_wildcard_or_second_host_refuses_at_parse() {
    for hosts in [json!(["*.vendor.example"]), json!(["api.vendor.example", "cdn.vendor.example"])] {
        let r = parse("vendor.wasm", json!({ "allow": hosts, "attach": { "Authorization": "Bearer ${secret://vendor-token}" } }));
        assert!(matches!(r, Err(SourceError::Connector(ConnectorError::SecretWildcardHost(_)))), "{r:?}");
    }
}
