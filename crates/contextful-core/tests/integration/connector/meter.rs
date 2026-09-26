//! The limiter binding, the acquire answer and the report body.

use contextful_core::connector::meter::{acquire_body, require_binding, Decision, LimiterBinding, LimiterDeclaration, Pool, Usage, DEFAULT_PERMITS};
use contextful_core::connector::ConnectorError;
use contextful_core::run::retry::RETRY_AFTER_CEILING_SECS;
use contextful_core::time::Instant;
use std::collections::BTreeMap;

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

const NOW: &str = "2030-01-01T00:00:00Z";

fn declaration() -> LimiterDeclaration {
    LimiterDeclaration::new("graph-app", "reads", &["X-App-Usage"]).unwrap()
}

/// The operator binds a quota name to an endpoint, a token held by reference and a batch size; an unnamed size is
/// the default, and a zero size asks for one permit.
// spec: connector.meter.limiter-binding@e8417ea3
#[test]
fn a_binding_names_an_endpoint_a_token_reference_and_a_batch_size() {
    let b = LimiterBinding::parse("graph-app", "https://limiter.example/v1/", "secret://limiter-token", Some(4)).unwrap();
    assert_eq!(b.quota, "graph-app");
    assert_eq!(b.token.as_str(), "limiter-token");
    assert_eq!(b.permits, 4);
    assert_eq!(b.call_url("acquire").as_str(), "https://limiter.example/v1/acquire");
    let d = LimiterBinding::parse("graph-app", "https://limiter.example", "secret://limiter-token", None).unwrap();
    assert_eq!(d.permits, DEFAULT_PERMITS);
    assert_eq!(LimiterBinding::parse("graph-app", "https://limiter.example", "secret://limiter-token", Some(0)).unwrap().permits, 1);
}

/// A cleartext endpoint off loopback, an endpoint carrying a query, fragment or userinfo, and a token that is not a
/// `secret://` reference raise `ConnectorLimiterBindingRejected`; the refusal never repeats the literal.
#[test]
fn a_cleartext_endpoint_or_a_literal_token_rejects_the_binding() {
    let rejected = |endpoint: &str, token: &str| match LimiterBinding::parse("graph-app", endpoint, token, None) {
        Err(ConnectorError::ConnectorLimiterBindingRejected(m)) => m,
        other => panic!("expected ConnectorLimiterBindingRejected, got {other:?}"),
    };
    rejected("http://limiter.example", "secret://limiter-token");
    rejected("https://limiter.example?key=1", "secret://limiter-token");
    rejected("https://user:pw@limiter.example", "secret://limiter-token");
    rejected("https://limiter.example#frag", "secret://limiter-token");
    let m = rejected("https://limiter.example", "sk-live-limiter-literal");
    assert!(!m.contains("sk-live-limiter-literal"), "{m}");
    rejected("https://limiter.example", "env://LIMITER_TOKEN");
    // Loopback is exempt from TLS.
    for host in ["http://127.0.0.1:8080", "http://localhost:8080", "http://[::1]:8080"] {
        assert!(LimiterBinding::parse("graph-app", host, "secret://limiter-token", None).is_ok(), "{host}");
    }
}

/// A declared quota with no binding raises `ConnectorQuotaUnbound`, naming the quota.
#[test]
fn a_declared_quota_with_no_binding_refuses() {
    let mut bindings = BTreeMap::new();
    bindings.insert("other".to_string(), LimiterBinding::parse("other", "https://limiter.example", "secret://limiter-token", None).unwrap());
    match require_binding(&declaration(), &bindings) {
        Err(ConnectorError::ConnectorQuotaUnbound(m)) => assert!(m.contains("graph-app"), "{m}"),
        other => panic!("expected ConnectorQuotaUnbound, got {other:?}"),
    }
    bindings.insert("graph-app".to_string(), LimiterBinding::parse("graph-app", "https://limiter.example", "secret://limiter-token", None).unwrap());
    assert_eq!(require_binding(&declaration(), &bindings).unwrap().quota, "graph-app");
}

/// An answer that is not JSON, names no decision, or grants without a count raises
/// {{connector.meter.unreadable-answer}}.
#[test]
fn an_unreadable_acquire_answer_grants_nothing() {
    for body in ["<html>ok</html>", "{}", "{\"decision\":\"granted\"}", "{\"decision\":\"maybe\"}", "{\"permits\":-1}", "{\"decision\":7}"] {
        match Decision::read(body.as_bytes()) {
            Err(ConnectorError::ConnectorLimiterUnreadable(_)) => {}
            other => panic!("`{body}`: expected ConnectorLimiterUnreadable, got {other:?}"),
        }
    }
}

/// The acquire and report bodies carry counts and verbatim headers, and no price, cost or currency field.
// spec: connector.meter.counting-not-pricing@1fca5afa
#[test]
fn the_wire_counts_requests_and_prices_none() {
    let acquire = acquire_body("graph-app", "reads", 8);
    let keys: Vec<&String> = acquire.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["class", "permits", "quota"]);
    let mut pool = Pool::default();
    pool.grant(2, 10, at(NOW));
    assert!(pool.take(at(NOW)));
    pool.observe(Usage::observe(&declaration(), 200, &[("X-App-Usage".into(), "{\"call_count\":3}".into())], at(NOW)));
    let report = pool.drain("graph-app", "reads", "run-1", at(NOW)).unwrap().to_json();
    let keys: Vec<&String> = report.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["class", "granted", "observed_at", "quota", "run_id", "spent", "usage"]);
    let text = report.to_string();
    for priced in ["price", "cost", "currency", "amount"] {
        assert!(!text.contains(priced), "{text}");
    }
}

/// A declaration forwarding a credential-bearing header raises `ConnectorForwardRejected`, whatever its case.
#[test]
fn a_declaration_forwarding_a_credential_header_refuses() {
    for header in ["Authorization", "proxy-authorization", "Cookie", "SET-COOKIE"] {
        match LimiterDeclaration::new("graph-app", "reads", &["X-App-Usage", header]) {
            Err(ConnectorError::ConnectorForwardRejected(m)) => assert!(m.contains("graph-app") && m.contains(&header.to_ascii_lowercase()), "{m}"),
            other => panic!("`{header}`: expected ConnectorForwardRejected, got {other:?}"),
        }
    }
    assert_eq!(declaration().forward, ["x-app-usage"]);
}

/// A limiter wait above the run's retry-after ceiling reads as the ceiling, from a denial, a bare `429` and a
/// zero-permit grant alike.
// spec: connector.meter.denial-ceiling@1ef4e6cb
#[test]
fn a_limiter_wait_caps_at_the_retry_after_ceiling() {
    let cap = Decision::Denied { retry_after_secs: RETRY_AFTER_CEILING_SECS };
    let huge = u64::MAX;
    assert_eq!(Decision::read(format!("{{\"decision\":\"denied\",\"retry_after_secs\":{huge}}}").as_bytes()).unwrap(), cap);
    assert_eq!(Decision::read(format!("{{\"retry_after_secs\":{huge}}}").as_bytes()).unwrap(), cap);
    assert_eq!(Decision::read(format!("{{\"decision\":\"granted\",\"permits\":0,\"ttl_secs\":{huge}}}").as_bytes()).unwrap(), cap);
    assert_eq!(Decision::throttled(Some("99999999999"), at(NOW)), cap);
    assert_eq!(Decision::throttled(Some("7"), at(NOW)), Decision::Denied { retry_after_secs: 7 });
}
