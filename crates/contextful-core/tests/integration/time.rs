//! `authority.verify`: the one timestamp grammar, decoded into UTC instants.

use contextful_core::time::Instant;
use contextful_core::AuthorityError;

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

/// Every checkpoint decodes timestamps into UTC instants under one grammar and compares instants.
// spec: authority.verify.timestamps@181d5994
#[test]
fn timestamps_decode_into_utc_instants_and_compare_as_instants() {
    // One instant written at two offsets is one instant.
    assert_eq!(at("2030-01-01T01:00:00+01:00"), at("2030-01-01T00:00:00Z"));
    assert_eq!(at("2030-01-01T01:00:00+01:00").to_rfc3339(), "2030-01-01T00:00:00Z");

    // Ordering follows the instant, not the text.
    assert!(at("2030-01-01T00:30:00+01:00") < at("2030-01-01T00:00:00Z"));
    assert!(at("2030-01-01T00:00:01Z") > at("2030-01-01T00:00:00Z"));

    // A numeric claim and its textual form decode to the same instant.
    assert_eq!(Instant::from_unix_secs(1_770_000_000).unwrap(), at("2026-02-02T02:40:00Z"));
    assert_eq!(at("2026-02-02T02:40:00Z").unix_secs(), 1_770_000_000);
    assert_eq!(at("2030-01-01T00:00:00Z").plus_secs(900), at("2030-01-01T00:15:00Z"));

    // The serde form is the same grammar.
    let json = serde_json::to_string(&at("2030-01-01T00:00:00Z")).unwrap();
    assert_eq!(json, "\"2030-01-01T00:00:00Z\"");
    assert_eq!(serde_json::from_str::<Instant>(&json).unwrap(), at("2030-01-01T00:00:00Z"));
    assert!(serde_json::from_str::<Instant>("\"2030-01-01\"").is_err());
}

/// A timestamp that does not decode under that grammar raises `TimestampMalformed` at every checkpoint alike.
// spec: authority.verify.malformed-timestamp@b308318f
#[test]
fn a_timestamp_outside_the_grammar_is_malformed() {
    for bad in [
        "",
        "2030-01-01",
        "2030-01-01 00:00:00Z",
        "2030-01-01T00:00:00",
        " 2030-01-01T00:00:00Z",
        "2030-01-01T00:00:00Z ",
        "2030-13-01T00:00:00Z",
        "1770000000",
        "tomorrow",
    ] {
        match Instant::parse(bad) {
            Err(AuthorityError::TimestampMalformed(msg)) => assert!(msg.contains(bad.trim()), "{bad:?}: {msg}"),
            other => panic!("{bad:?}: expected TimestampMalformed, got {other:?}"),
        }
    }
    let err = Instant::parse("2030-01-01").unwrap_err();
    assert!(err.to_string().starts_with("TimestampMalformed"), "{err}");

    // A numeric claim outside the representable range refuses the same way.
    assert!(matches!(Instant::from_unix_secs(i64::MAX), Err(AuthorityError::TimestampMalformed(_))));
}

#[test]
fn instants_carry_nanoseconds() {
    let t = at("2030-01-01T00:00:00.000000123Z");
    assert_eq!(t.unix_nanos(), 1_893_456_000_000_000_123);
    assert_eq!(Instant::from_unix_nanos(t.unix_nanos()).unwrap(), t);
    assert_eq!(t.to_rfc3339_nanos(), "2030-01-01T00:00:00.000000123Z");
    assert_eq!(at("2030-01-01T01:00:00+01:00").to_rfc3339_nanos(), "2030-01-01T00:00:00.000000000Z");
    assert_eq!(at("2030-01-01T00:15:00Z").minus_secs(900), at("2030-01-01T00:00:00Z"));
    assert!(Instant::from_unix_nanos(i128::MAX).is_err());
}

/// One `<n>[smhd]` duration grammar serves `retain_runs` and `max_lag`: digits then one
/// unit character, zero admitted; a caller needing a positive span refuses zero itself.
#[test]
fn one_duration_grammar_reads_n_then_a_unit() {
    use contextful_core::time::duration_secs;
    assert_eq!(duration_secs("90s"), Some(90));
    assert_eq!(duration_secs("15m"), Some(900));
    assert_eq!(duration_secs("2h"), Some(7_200));
    assert_eq!(duration_secs("7d"), Some(604_800));
    assert_eq!(duration_secs("0d"), Some(0));
    for bad in ["", "d", "+5d", "-1h", "1.5h", "1 h", "1w", "7天", "٧d", "99999999999999999999d"] {
        assert_eq!(duration_secs(bad), None, "`{bad}`");
    }
}
