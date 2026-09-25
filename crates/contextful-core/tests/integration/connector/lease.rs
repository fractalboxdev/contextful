//! The lease response and the retirement of a cached lease.

use contextful_core::connector::lease::{read_response, retires_at, CACHE_TTL_SECS, RETIRE_CAP_SECS, RETIRE_SHARE_PERCENT};
use contextful_core::run::FailureTag;
use contextful_core::time::Instant;

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

const NOW: &str = "2030-01-01T00:00:00Z";

/// An entry retires 10 percent of its window ahead of expiry, the margin capped at 30 s.
// spec: connector.lease.retirement-margin@9037b17c
#[test]
fn a_lease_retires_a_tenth_of_its_window_early_at_most_30_s() {
    assert_eq!((RETIRE_SHARE_PERCENT, RETIRE_CAP_SECS, CACHE_TTL_SECS), (10, 30, 300));
    let now = at(NOW);
    // A 100 s lease retires 10 s early.
    assert_eq!(retires_at(now, Some(now.plus_secs(100))), now.plus_secs(90));
    // A 600 s lease is bounded by the 300 s cache window, and its margin caps at 30 s.
    assert_eq!(retires_at(now, Some(now.plus_secs(600))), now.plus_secs(270));
    // A stored credential carries no lease window and lives the whole cache TTL.
    assert_eq!(retires_at(now, None), now.plus_secs(300));
}

/// A success carries `value` plus one expiry — `expires_in` seconds, `expires_at` epoch seconds, or an RFC 3339
/// instant — at the top level or under `lease`.
// spec: connector.lease.response@98329469
#[test]
fn a_lease_reads_three_expiry_spellings_at_two_levels() {
    let now = at(NOW);
    let expect = now.plus_secs(600);
    for body in [
        r#"{"value":"v","expires_in":600}"#.to_string(),
        format!(r#"{{"value":"v","expires_at":{}}}"#, expect.unix_secs()),
        r#"{"value":"v","expires_at":"2030-01-01T00:10:00Z"}"#.to_string(),
        r#"{"lease":{"value":"v","expires_in":600}}"#.to_string(),
    ] {
        let lease = read_response(body.as_bytes(), now).unwrap();
        assert_eq!((lease.value.reveal(), lease.expires_at), ("v", expect), "{body}");
    }
}

/// A lease carrying no expiry, or one already past on arrival, raises `SecretMalformedLease` as a transient
/// failure.
// spec: connector.lease.malformed-lease@2aa55848
#[test]
fn a_lease_without_a_future_expiry_is_malformed_and_transient() {
    let now = at(NOW);
    for body in [r#"{"value":"v"}"#, r#"{"value":"v","expires_at":"2029-12-31T23:59:59Z"}"#, r#"{"value":"v","expires_in":0}"#] {
        let f = read_response(body.as_bytes(), now).unwrap_err();
        assert_eq!(f.tag, FailureTag::Transient, "{body}");
        assert!(f.message.starts_with("SecretMalformedLease"), "{}", f.message);
        assert!(!f.message.contains("\"v\""));
    }
}
