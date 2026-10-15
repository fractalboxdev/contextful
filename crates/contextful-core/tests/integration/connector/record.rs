//! Operator records, the inventory row and its expiry marks.

use contextful_core::connector::record::{
    bound_names, inventory_row, read_records, Answered, Expiry, Mark, Observation, OperatorRecord, Surface, Unavailable, EXPIRY_WARNING_DAYS,
};
use contextful_core::connector::ConnectorError;
use contextful_core::time::Instant;
use std::collections::BTreeSet;

const FILE: &str = "contextful.toml";
const NOW: &str = "2030-01-01T00:00:00Z";

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

fn manifest(text: &str) -> toml::Value {
    toml::from_str(text).unwrap()
}

fn bound(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| n.to_string()).collect()
}

fn rejected(text: &str) -> String {
    match read_records(FILE, &manifest(text), &bound(&["vendor-token"])).unwrap_err() {
        ConnectorError::SecretRecordRejected(m) => m,
        other => panic!("expected SecretRecordRejected, got {other:?}"),
    }
}

/// Every record key parses; `expires = "never"` is the explicit no-expiry assertion.
// spec: connector.record.record-table@9f120f6d
#[test]
fn a_secret_table_carries_the_seven_optional_record_keys() {
    let text = r#"
[secret.vendor-token]
description = "Vendor API key"
account     = "acct-4471 (orders tenancy)"
grants      = ["orders.read", "orders.export"]
surfaces    = [{ surface = "orders pipeline", binding = "Authorization header" }]
rotate      = "console.vendor.example/settings/api-keys"
created     = 2029-06-01
expires     = "never"
"#;
    let records = read_records(FILE, &manifest(text), &bound(&["vendor-token"])).unwrap();
    let r = &records["vendor-token"];
    assert_eq!(r.description.as_deref(), Some("Vendor API key"));
    assert_eq!(r.account.as_deref(), Some("acct-4471 (orders tenancy)"));
    assert_eq!(r.grants, Some(vec!["orders.read".to_string(), "orders.export".to_string()]));
    assert_eq!(r.rotate.as_deref(), Some("console.vendor.example/settings/api-keys"));
    assert_eq!(r.created, Some(at("2029-06-01T00:00:00Z")));
    assert_eq!(r.expires, Expiry::Never);
}

/// An operator record describes a name's grants, account, consuming surfaces and rotation
/// location; creation and expiry stay operator metadata.
// spec: connector.record.operator-record@0e4b32da
#[test]
fn an_operator_record_describes_grants_account_surfaces_and_rotation() {
    let text = "[secret.vendor-token]\naccount = \"acct-1\"\ngrants = [\"read\"]\nrotate = \"console\"\nexpires = 2030-01-10\n";
    let r = read_records(FILE, &manifest(text), &bound(&["vendor-token"])).unwrap().remove("vendor-token").unwrap();
    assert_eq!((r.account.as_deref(), r.rotate.as_deref()), (Some("acct-1"), Some("console")));
    assert_eq!(r.expires, Expiry::At(at("2030-01-10T00:00:00Z")));
    let row = inventory_row("vendor-token", vec!["env".into()], Some(r), Vec::new(), None, at(NOW));
    assert_eq!(row.expiries[0].provenance, "operator");
}

/// A record identifies each consuming surface and its binding name.
// spec: connector.record.surface-of-consumption@5d61c3b5
#[test]
fn a_record_names_each_consuming_surface_and_its_binding() {
    let text = "[secret.vendor-token]\nsurfaces = [{ surface = \"orders\", binding = \"Authorization\" }, { surface = \"export\", binding = \"VENDOR_TOKEN\" }]\n";
    let r = read_records(FILE, &manifest(text), &bound(&["vendor-token"])).unwrap().remove("vendor-token").unwrap();
    assert_eq!(
        r.surfaces,
        vec![
            Surface { surface: "orders".into(), binding: "Authorization".into() },
            Surface { surface: "export".into(), binding: "VENDOR_TOKEN".into() }
        ]
    );
}

/// Grants an operator has not established are unknown.
// spec: connector.record.unverified-scope-is-marked@1652f6ce
#[test]
fn an_absent_grants_key_reads_as_unknown() {
    let r = read_records(FILE, &manifest("[secret.vendor-token]\ndescription = \"key\"\n"), &bound(&["vendor-token"])).unwrap().remove("vendor-token").unwrap();
    assert_eq!(r.grants, None);
    assert_eq!(r.expires, Expiry::Unknown);
}

/// A misspelled name, an unknown key, a mistyped value and a credential-shaped string each
/// refuse, naming the file, the name and the key path, never the value.
// spec: connector.record.record-rejected@d06f2826
#[test]
fn a_record_describing_nothing_or_holding_a_bad_key_is_refused_without_its_value() {
    let sentinel = "sentinel-value-0042";
    let unbound = rejected(&format!("[secret.vendor-tokne]\ndescription = \"{sentinel}\"\n"));
    assert!(unbound.contains(FILE) && unbound.contains("secret.vendor-tokne"), "{unbound}");
    assert!(!unbound.contains(sentinel), "{unbound}");

    let unknown = rejected(&format!("[secret.vendor-token]\nvalue = \"{sentinel}\"\n"));
    assert!(unknown.contains("secret.vendor-token.value"), "{unknown}");
    assert!(!unknown.contains(sentinel), "{unknown}");

    let mistyped = rejected(&format!("[secret.vendor-token]\nexpires = \"{sentinel}\"\n"));
    assert!(mistyped.contains("secret.vendor-token.expires"), "{mistyped}");
    assert!(!mistyped.contains(sentinel), "{mistyped}");

    let nested = rejected("[secret.vendor-token]\ngrants = [\"read\", 7]\n");
    assert!(nested.contains("secret.vendor-token.grants[1]"), "{nested}");

    let token = "ghp_0123456789abcdefghijklmnopqrstuvwxyzAB";
    let shaped = rejected(&format!("[secret.vendor-token]\naccount = \"{token}\"\n"));
    assert!(shaped.contains("secret.vendor-token.account"), "{shaped}");
    assert!(!shaped.contains(token), "{shaped}");
}

/// A committed record holds descriptive metadata only, and no value is needed to record a binding.
// spec: connector.record.metadata-storage@f43dea8c
#[test]
fn a_record_holds_metadata_and_never_a_value() {
    for key in ["value", "ciphertext", "secret"] {
        let m = rejected(&format!("[secret.vendor-token]\n{key} = \"x\"\n"));
        assert!(m.starts_with("SecretRecordRejected") || m.contains(key), "{m}");
    }
    let r = read_records(FILE, &manifest("[secret.vendor-token]\n"), &bound(&["vendor-token"])).unwrap();
    assert_eq!(r["vendor-token"], OperatorRecord::default());
}

/// Every `secret://` name outside the record block is a configured binding.
#[test]
fn configured_bindings_are_every_secret_reference_outside_the_record_block() {
    let text = r#"
[pipeline.source.config.headers]
Authorization = "Bearer ${secret://vendor-token}"
[derive.speech.env]
SPEECH_API_KEY = "${secret://speech-vendor}"
[sync]
key = "secret://sync-key"
[secret.vendor-token]
description = "describes secret://not-a-binding"
"#;
    assert_eq!(bound_names(&manifest(text)), bound(&["speech-vendor", "sync-key", "vendor-token"]));
}

/// Days remaining is computed for a dated expiry alone; an absent expiry is unknown.
// spec: connector.record.expiry-unknown@75b358a1
#[test]
fn an_absent_expiry_is_unknown_and_carries_no_days_remaining() {
    let row = inventory_row("vendor-token", vec!["env".into()], None, Vec::new(), None, at(NOW));
    assert_eq!(row.expiries.len(), 1);
    assert_eq!((row.expiries[0].expires, row.expiries[0].days_remaining, row.expiries[0].mark), (Expiry::Unknown, None, None));
    let never = OperatorRecord { expires: Expiry::Never, ..OperatorRecord::default() };
    let row = inventory_row("vendor-token", vec!["env".into()], Some(never), Vec::new(), None, at(NOW));
    assert_eq!((row.expiries[0].expires, row.expiries[0].days_remaining), (Expiry::Never, None));
}

/// A dated expiry within 30 d and an elapsed one are each marked, keeping provenance.
// spec: connector.record.expiry-warning@ce4b5753
#[test]
fn an_expiry_within_30_d_and_an_elapsed_one_are_marked() {
    assert_eq!(EXPIRY_WARNING_DAYS, 30);
    let dated = |s: &str| OperatorRecord { expires: Expiry::At(at(s)), ..OperatorRecord::default() };
    let row = |r| inventory_row("vendor-token", Vec::new(), Some(r), Vec::new(), None, at(NOW)).expiries.remove(0);
    let soon = row(dated("2030-01-11T00:00:00Z"));
    assert_eq!((soon.days_remaining, soon.mark, soon.provenance.as_str()), (Some(10), Some(Mark::Within), "operator"));
    let edge = row(dated("2030-01-31T00:00:00Z"));
    assert_eq!((edge.days_remaining, edge.mark), (Some(30), None));
    let gone = row(dated("2029-12-30T00:00:00Z"));
    assert_eq!(gone.mark, Some(Mark::Elapsed));
    assert_eq!(gone.days_remaining, Some(-2));
}

/// An operator expiry and an observed one both appear, each labelled apart.
// spec: connector.record.observation-provenance@e9e98b77
#[test]
fn operator_and_observed_expiries_are_listed_with_their_provenance() {
    let operator = OperatorRecord { expires: Expiry::At(at("2030-06-01T00:00:00Z")), ..OperatorRecord::default() };
    let observed = Observation::Observed {
        adapter: "manager".into(),
        at: at(NOW),
        present: true,
        version: Some("7".into()),
        created: None,
        expires: Expiry::At(at("2030-01-05T00:00:00Z")),
    };
    let row = inventory_row("vendor-token", vec!["manager".into()], Some(operator), vec![observed], None, at(NOW));
    let by: Vec<(&str, Option<Mark>)> = row.expiries.iter().map(|e| (e.provenance.as_str(), e.mark)).collect();
    assert_eq!(by, vec![("operator", None), ("manager", Some(Mark::Within))]);
    assert_eq!(row.observations[0].adapter(), "manager");
}

/// A binding with no record stays in inventory with unknown descriptive fields.
// spec: connector.record.optional-record@6a574618
#[test]
fn a_binding_without_a_record_lists_with_unknown_fields() {
    let row = inventory_row("vendor-token", vec!["env".into()], None, Vec::new(), None, at(NOW));
    assert_eq!((row.name.as_str(), row.operator.as_ref()), ("vendor-token", None));
}

/// Configured adapters identify configuration; only a run's attribution names an answering one.
#[test]
fn configured_adapters_never_read_as_the_answering_one() {
    let row = inventory_row("vendor-token", vec!["lease".into(), "env".into()], None, Vec::new(), None, at(NOW));
    assert_eq!((row.configured.len(), row.answered.as_ref()), (2, None));
    let answered = Answered { adapter: "env".into(), run_id: "run-1".into(), at: at(NOW) };
    let row = inventory_row("vendor-token", vec!["lease".into(), "env".into()], None, Vec::new(), Some(answered.clone()), at(NOW));
    assert_eq!(row.answered, Some(answered));
}

/// An unavailable read carries its reason and no expiry stands in for it.
#[test]
fn an_unavailable_observation_contributes_no_expiry() {
    let o = Observation::Unavailable { adapter: "manager".into(), at: at(NOW), reason: Unavailable::Denied("403".into()) };
    let row = inventory_row("vendor-token", vec!["manager".into()], None, vec![o], None, at(NOW));
    assert_eq!(row.expiries.len(), 1);
    let json = serde_json::to_value(&row.observations[0]).unwrap();
    assert_eq!((json["state"].as_str(), json["reason"].as_str()), (Some("unavailable"), Some("denied")));
}
