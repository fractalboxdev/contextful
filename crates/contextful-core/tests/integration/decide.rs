//! The decision module's case vocabulary: one case text in, one decision out.

use contextful_core::decide::{decide, Decision, CASE_MALFORMED};

fn decided(case: &[u8]) -> Decision {
    decide(case)
}

fn malformed() -> Decision {
    Decision::refused(CASE_MALFORMED, None)
}

#[test]
fn text_that_is_not_utf8_is_malformed() {
    for bad in [&b"\xff"[..], b"\xc0\xaf", b"\xed\xa0\x80", b"\xf4\x90\x80\x80", b"\x80"] {
        let case = [&br#"{"op":"covers_name","pattern":""#[..], bad, br#"","name":"a"}"#].concat();
        assert_eq!(decided(&case), malformed(), "{case:?}");
    }
    assert_eq!(decided(b"not json"), malformed());
}

#[test]
fn an_integer_past_the_unsigned_64_bit_range_is_malformed() {
    for literal in ["18446744073709551616", "340282366920938463463374607431768211456", "-1", "1e2"] {
        let case = format!(r#"{{"op":"narrow","parent":[{{"actions":["read"],"tables":["*"],"max_rows":{literal}}}],"child":[]}}"#);
        assert_eq!(decided(case.as_bytes()), malformed(), "{literal}");
    }
    let at_max = r#"{"op":"narrow","parent":[{"actions":["read"],"tables":["*"],"max_rows":18446744073709551615}],"child":[]}"#;
    assert_eq!(decided(at_max.as_bytes()).verdict, "admitted");
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
