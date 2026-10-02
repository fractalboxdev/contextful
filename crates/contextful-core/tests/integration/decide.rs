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
