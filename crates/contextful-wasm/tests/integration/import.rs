//! `connector.import`: what crosses inward.

use crate::support::{host, loopback, open, open_with, text};
use contextful_core::run::FailureTag;
use contextful_wasm::Limits;
use serde_json::json;

/// A guest standard library pulling broader interfaces links against an empty context: no preopens, no environment,
/// no arguments.
// spec: connector.import.empty-context@76c99ea8
#[test]
fn a_guest_standard_library_sees_no_environment_arguments_or_files() {
    std::env::set_var("CONTEXTFUL_PROBE_AMBIENT", "visible-to-the-host");
    let mut s = open();
    s.open("ambient", None).unwrap();
    assert_eq!(text(&s.next().unwrap().unwrap()), "env=0 args=0 root=none");
}

/// The pipeline's guest configuration table is the one part of the source configuration crossing inward, as one JSON
/// object delivered once per session ahead of discovery.
// spec: connector.import.forwarded-config@a2678abd
#[test]
fn the_guest_table_arrives_once_ahead_of_discovery() {
    let mut s = open_with(loopback(), &Limits::default(), Some(&json!({ "region": "eu", "limit": 50 }))).unwrap();
    s.discover().unwrap();
    s.open("calls", None).unwrap();
    assert_eq!(text(&s.next().unwrap().unwrap()), "configure,discover,open");
    s.open("config", None).unwrap();
    assert_eq!(text(&s.next().unwrap().unwrap()), r#"{"limit":50,"region":"eu"}"#);

    let mut bare = open();
    bare.discover().unwrap();
    bare.open("calls", None).unwrap();
    assert_eq!(text(&bare.next().unwrap().unwrap()), "discover,open", "no table, no configure call");

    let refused = open_with(loopback(), &Limits::default(), Some(&json!({ "unknown": 1 }))).err().expect("an unusable key refuses");
    assert_eq!(refused.tag, FailureTag::Permanent);
    let rejected = open_with(loopback(), &Limits::default(), Some(&json!({ "token": "secret://vendor-token" }))).err().expect("a reference refuses");
    assert!(rejected.message.starts_with("ConnectorConfigRejected"), "{rejected}");
}

/// Where the artifact is local and probed, a guest table declared against a guest exporting no configuration
/// interface raises `ConnectorConfigUnclaimed`.
// spec: connector.import.config-unclaimed@54a0c902
#[test]
fn a_guest_table_for_a_guest_without_the_config_interface_is_refused() {
    let (h, _, base) = host();
    let f = h.open(base, loopback(), &Limits::default(), Some(&json!({ "region": "eu" }))).err().expect("refused");
    assert!(f.message.starts_with("ConnectorConfigUnclaimed"), "{f}");
    assert_eq!(f.tag, FailureTag::Config);
    assert!(h.open(base, loopback(), &Limits::default(), None).is_ok());
}
