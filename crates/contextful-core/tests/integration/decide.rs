//! The decision module's domain cases: one decoded case in, one decision out.

use contextful_core::decide::{decide_value, Decision, CASE_MALFORMED};

fn decided(case: &[u8]) -> Decision {
    decide_value(&serde_json::from_slice(case).unwrap())
}

fn malformed() -> Decision {
    Decision::refused(CASE_MALFORMED, None)
}

#[test]
fn a_placement_case_carries_the_zone_it_resolved() {
    let admits = decided(br#"{"op":"zone_admits","zone":"\u00a0on-prem:hq","allow":["on-prem:*"]}"#);
    assert_eq!((admits.verdict.as_str(), admits.zone.as_deref()), ("admitted", Some("on-prem:hq")));
    let excluded = decided(br#"{"op":"zone_admits","zone":"cloud","allow":["on-prem:*"]}"#);
    assert_eq!((excluded.verdict.as_str(), excluded.zone.as_deref()), ("excluded", Some("undeclared")));
    let unparsed = decided(br#"{"op":"zone_admits","zone":"cloud","allow":["on-prem:**"]}"#);
    assert_eq!(unparsed.error.as_deref(), Some("EnforceZonePatternUnparsed"));
    let pinned = decided(br#"{"op":"session_zone","incognito":true}"#);
    assert_eq!((pinned.verdict.as_str(), pinned.zone.as_deref()), ("placed", Some("local:device")));
    let widened = decided(br#"{"op":"session_zone","asserted":"public-cloud:x","signed":"on-prem:hq","incognito":false}"#);
    assert_eq!(widened.error.as_deref(), Some("EnforceZoneAssertionWidens"));
    assert_eq!(decided(br#"{"op":"session_zone","signed":"on-prem:hq"}"#), malformed());
}

#[test]
fn a_narrowing_case_decides_horizon_and_child_expiry() {
    let refused = |error: &str, dimension: Option<&str>| Decision::refused(error, dimension);
    let later = decided(
        br#"{"op":"narrow","parent":[{"actions":["read"],"tables":["*"],"horizon":1000}],
            "child":[{"actions":["read"],"tables":["*"],"horizon":1001}]}"#,
    );
    assert_eq!(later, refused("AttenuationWidens", Some("horizon")));
    let earlier = decided(
        br#"{"op":"narrow","parent":[{"actions":["read"],"tables":["*"],"horizon":1000}],
            "child":[{"actions":["read"],"tables":["*"],"horizon":999}]}"#,
    );
    assert_eq!(earlier, Decision::verdict("admitted"));
    // A child naming no horizon inherits its parent's.
    let inherits = decided(
        br#"{"op":"narrow","parent":[{"actions":["read"],"tables":["*"],"horizon":1000}],
            "child":[{"actions":["read"],"tables":["*"]}]}"#,
    );
    assert_eq!(inherits, Decision::verdict("admitted"));
    let extended = decided(
        br#"{"op":"narrow","parent":[{"actions":["read"],"tables":["*"]}],"parent_exp":1000,
            "child":[{"actions":["read"],"tables":["*"]}],"child_exp":1001}"#,
    );
    assert_eq!(extended, refused("AttenuationExpiryExtended", None));
    let within = decided(
        br#"{"op":"narrow","parent":[{"actions":["read"],"tables":["*"]}],"parent_exp":1000,
            "child":[{"actions":["read"],"tables":["*"]}],"child_exp":1000}"#,
    );
    assert_eq!(within, Decision::verdict("admitted"));
    // Widening refuses before expiry, and expiry before tenant scope.
    let widens_first = decided(
        br#"{"op":"narrow","parent":[{"actions":["read"],"tables":["*"],"tenant":{"table":"t","value":"a"}}],
            "parent_exp":1000,"child":[{"actions":["write"],"tables":["*"]}],"child_exp":1001}"#,
    );
    assert_eq!(widens_first, refused("AttenuationWidens", Some("actions")));
    let expiry_first = decided(
        br#"{"op":"narrow","parent":[{"actions":["read"],"tables":["*"],"tenant":{"table":"t","value":"a"}}],
            "parent_exp":1000,"child":[{"actions":["read"],"tables":["*"]}],"child_exp":1001}"#,
    );
    assert_eq!(expiry_first, refused("AttenuationExpiryExtended", None));
    // An instant no timestamp represents refuses; an expiry past the signed 64-bit range is malformed.
    let unrepresentable = decided(
        br#"{"op":"narrow","parent":[{"actions":["read"],"tables":["*"],"horizon":18446744073709551615}],"child":[]}"#,
    );
    assert_eq!(unrepresentable.error.as_deref(), Some("TimestampMalformed"));
    // A later grant's malformed field outranks an earlier grant's unrepresentable horizon.
    let later_malformed = decided(
        br#"{"op":"narrow","parent":[{"actions":["read"],"tables":["*"],"horizon":18446744073709551615},
            {"actions":["read"],"tables":["*"],"max_rows":-1}],"child":[]}"#,
    );
    assert_eq!(later_malformed, malformed());
    let overflow = decided(
        br#"{"op":"narrow","parent":[],"parent_exp":9223372036854775808,"child":[]}"#,
    );
    assert_eq!(overflow, malformed());
}
