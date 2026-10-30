//! `connector.rotate`: the `[auth]` refresh policy a connector manifest declares, its
//! refusals, the refresh form and the token endpoint's answer classes.

use contextful_core::connector::manifest::Manifest;
use contextful_core::connector::reference::Hydrated;
use contextful_core::connector::rotate::{classify_refresh, read_token_answer};
use contextful_core::connector::ConnectorError;
use contextful_core::run::FailureTag;
use contextful_core::time::Instant;

const AUTH: &str = r#"
name = "vendor"
[capabilities]
allow_hosts = ["api.vendor.example"]

[auth]
token_endpoint = "https://auth.vendor.example/oauth/token"
client_id      = "vendor-reader"
client_secret  = "${secret://vendor-client-secret}"
refresh_token  = "${secret://vendor-refresh}"
access_token   = "${secret://vendor-token}"
scope          = "records.read"
"#;

fn rejected(text: &str) -> String {
    match Manifest::parse(text) {
        Err(e @ ConnectorError::ConnectorManifestRejected(_)) => e.to_string(),
        other => panic!("expected ConnectorManifestRejected, got {other:?}"),
    }
}

/// A connector manifest's `[auth]` table declares the token endpoint, the client, the three
/// references and the scope.
// spec: connector.rotate.auth-policy@a81bc2d3
#[test]
fn an_auth_table_declares_the_refresh_policy() {
    let auth = Manifest::parse(AUTH).unwrap().auth.expect("the `[auth]` table reads");
    assert_eq!(auth.token_endpoint.as_str(), "https://auth.vendor.example/oauth/token");
    assert_eq!(auth.client_id, "vendor-reader");
    assert_eq!(auth.client_secret.as_str(), "vendor-client-secret");
    assert_eq!(auth.refresh_token.as_str(), "vendor-refresh");
    assert_eq!(auth.access_token.as_str(), "vendor-token");
    assert_eq!(auth.scope, "records.read");
    let loopback = AUTH.replace("https://auth.vendor.example", "http://127.0.0.1:8443");
    assert!(Manifest::parse(&loopback).unwrap().auth.is_some(), "a loopback endpoint may be cleartext");
    assert!(Manifest::parse("name = \"vendor\"\n").unwrap().auth.is_none());
}

/// A missing key, another key, a cleartext or userinfo endpoint, a value holding other than one
/// reference, and one name bound twice each refuse, naming the key and never the value.
// spec: connector.rotate.auth-refused@60ebe7d3
#[test]
fn an_auth_table_off_its_shape_refuses_naming_the_key() {
    let cases = [
        (AUTH.replace("scope          = \"records.read\"\n", ""), "auth.scope"),
        (AUTH.replace("scope ", "grant = \"x\"\nscope "), "auth.grant"),
        (AUTH.replace("https://auth.vendor.example", "http://auth.vendor.example"), "auth.token_endpoint"),
        (AUTH.replace("https://auth.vendor.example", "https://user:pw@auth.vendor.example"), "auth.token_endpoint"),
        (AUTH.replace("\"${secret://vendor-client-secret}\"", "\"plain-client-secret-value\""), "auth.client_secret"),
        (AUTH.replace("\"${secret://vendor-refresh}\"", "\"Bearer ${secret://vendor-refresh}\""), "auth.refresh_token"),
        (AUTH.replace("${secret://vendor-token}", "${secret://vendor-refresh}"), "auth.access_token"),
        (AUTH.replace("${secret://vendor-refresh}", "${secret://vendor-client-secret}"), "auth.refresh_token"),
        (AUTH.replace("\"vendor-reader\"", "\" \""), "auth.client_id"),
    ];
    assert!(!cases.is_empty());
    for (text, key) in &cases {
        let message = rejected(text);
        assert!(message.contains(key), "{key}: {message}");
        assert!(!message.contains("plain-client-secret-value") && !message.contains(":pw@"), "{message}");
    }
}

/// A refresh posts `grant_type=refresh_token`, the refresh token, the scope, the client id and
/// the client secret as one form.
// spec: connector.rotate.refresh-grant@ab9f8372
#[test]
fn the_refresh_form_carries_the_grant_the_scope_and_the_client() {
    let auth = Manifest::parse(AUTH).unwrap().auth.unwrap();
    let form = auth.form(&Hydrated::new("refresh-0"), &Hydrated::new("client-secret-0"));
    let pairs: Vec<(String, String)> = url::form_urlencoded::parse(form.reveal().as_bytes()).into_owned().collect();
    assert_eq!(
        pairs,
        [("grant_type", "refresh_token"), ("refresh_token", "refresh-0"), ("scope", "records.read"), ("client_id", "vendor-reader"), ("client_secret", "client-secret-0")]
            .map(|(k, v)| (k.to_string(), v.to_string()))
    );
    assert!(!format!("{form:?}").contains("refresh-0"), "the form prints as its wrapper");
}

/// An answer carries the access token, an expiry from a positive `expires_in`, and an optional
/// replacement refresh token.
// spec: connector.rotate.refresh-answer@7a21460f
#[test]
fn a_refresh_answer_reads_the_access_token_its_expiry_and_a_replacement() {
    let now = Instant::parse("2030-01-01T00:00:00Z").unwrap();
    let a = read_token_answer(br#"{"access_token":"a1","expires_in":3600,"refresh_token":"r1","token_type":"Bearer"}"#, now, "https://auth.vendor.example/oauth/token").unwrap();
    assert_eq!(a.access_token.reveal(), "a1");
    assert_eq!(a.expires_at, Some(now.plus_secs(3600)));
    assert_eq!(a.refresh_token.as_ref().map(Hydrated::reveal), Some("r1"));
    let bare = read_token_answer(br#"{"access_token":"a2"}"#, now, "https://auth.vendor.example/oauth/token").unwrap();
    assert_eq!((bare.expires_at, bare.refresh_token), (None, None));
}

/// A redirect, a `4xx` other than `429`, or a success carrying no `access_token` raises
/// `SecretRefreshRejected`, classed `auth_expired`, naming no token.
// spec: connector.rotate.refresh-refused@30fee3d1
#[test]
fn a_refused_refresh_raises_refresh_rejected() {
    let endpoint = "https://auth.vendor.example/oauth/token";
    for status in [302, 400, 401, 403] {
        let f = classify_refresh(status, None, endpoint);
        assert!(f.message.starts_with("SecretRefreshRejected"), "{status}: {f}");
        assert_eq!(f.tag, FailureTag::AuthExpired);
    }
    let now = Instant::parse("2030-01-01T00:00:00Z").unwrap();
    for body in [&br#"{"token_type":"Bearer","refresh_token":"r-secret"}"#[..], b"not json"] {
        let f = read_token_answer(body, now, endpoint).unwrap_err();
        assert!(f.message.starts_with("SecretRefreshRejected") && !f.message.contains("r-secret"), "{f}");
        assert_eq!(f.tag, FailureTag::AuthExpired);
    }
}

/// The token client retries nothing: a `429` passes its `Retry-After`, a `5xx` is transient, so
/// the run's retry schedule is the one retry layer.
// spec: connector.rotate.one-retry-layer@7def2c94
#[test]
fn a_throttled_or_failing_refresh_returns_to_the_runs_retry() {
    let endpoint = "https://auth.vendor.example/oauth/token";
    let throttled = classify_refresh(429, Some(7), endpoint);
    assert_eq!((throttled.tag, throttled.retry_after_secs), (FailureTag::RateLimited, Some(7)));
    for status in [500, 502, 503] {
        assert_eq!(classify_refresh(status, None, endpoint).tag, FailureTag::Transient);
    }
}
