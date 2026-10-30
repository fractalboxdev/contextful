//! The `[auth]` refresh loop: the held access token, the refresh grant, the write-forward of
//! each replacement refresh token, and the one retry layer.

use crate::support::{proxy_env, Response, Server, SetClock};
use contextful_core::connector::rotate::AuthPolicy;
use contextful_core::run::FailureTag;
use contextful_outbound::oauth::Refresher;
use contextful_outbound::secrets::assemble_all;
use std::collections::BTreeMap;
use std::sync::Arc;

fn policy(server: &Server) -> AuthPolicy {
    AuthPolicy::new(&server.url("/oauth/token"), "vendor-reader", "${secret://vendor-client-secret}", "${secret://vendor-refresh}", "${secret://vendor-token}", "records.read").unwrap()
}

/// A token endpoint rotating the refresh token on every grant: `refresh-<n>` answers access
/// token `access-<n+1>` and replacement `refresh-<n+1>`, valid for `expires_in` seconds.
fn rotating(expires_in: u64) -> Server {
    Server::start(move |r| {
        let form: BTreeMap<String, String> = url::form_urlencoded::parse(&r.body).into_owned().collect();
        let presented = form.get("refresh_token").and_then(|t| t.strip_prefix("refresh-")).and_then(|n| n.parse::<u32>().ok());
        match presented {
            Some(n) if form.get("client_secret").map(String::as_str) == Some("client-secret") => {
                Response::json(200, &format!(r#"{{"access_token":"access-{}","refresh_token":"refresh-{}","expires_in":{expires_in}}}"#, n + 1, n + 1))
            }
            _ => Response::json(400, r#"{"error":"invalid_grant"}"#),
        }
    })
}

/// A secrets directory holding the client secret and `refresh-0`, and its file backend's assembly.
fn store() -> (tempfile::TempDir, BTreeMap<String, String>) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("vendor-client-secret"), "client-secret").unwrap();
    std::fs::write(dir.path().join("vendor-refresh"), "refresh-0").unwrap();
    let vars = [("CONTEXTFUL_SECRETS_BACKEND", "file"), ("CONTEXTFUL_SECRETS_DIR", dir.path().to_str().unwrap())].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    (dir, vars)
}

fn refresher(server: &Server, vars: &BTreeMap<String, String>, clock: &SetClock) -> Refresher {
    let a = assemble_all(vars, Arc::new(clock.clone())).unwrap();
    Refresher::new(policy(server), Arc::new(a.resolver), a.writers, Arc::new(clock.clone())).unwrap()
}

/// The host holds the access token from the answer, refreshes it at the retirement margin of
/// its declared expiry, and puts each replacement to the refresh token's adapter.
// spec: connector.rotate.oauth-credential@391c7c68
// spec: connector.rotate.access-token@69382394
// spec: connector.rotate.refresh-answer@7a21460f
#[test]
fn the_access_token_is_held_until_its_margin_and_each_replacement_is_written_forward() {
    let _env = proxy_env();
    let server = rotating(100);
    let (dir, vars) = store();
    let clock = SetClock::new();
    let r = refresher(&server, &vars, &clock);
    assert_eq!(r.access().unwrap().reveal(), "access-1");
    assert_eq!(std::fs::read_to_string(dir.path().join("vendor-refresh")).unwrap(), "refresh-1");
    clock.advance(89);
    assert_eq!(r.access().unwrap().reveal(), "access-1", "held inside 90 percent of its window");
    clock.advance(1);
    assert_eq!(r.access().unwrap().reveal(), "access-2", "refreshed at the margin");
    assert_eq!(std::fs::read_to_string(dir.path().join("vendor-refresh")).unwrap(), "refresh-2");
    let grants = server.received("/oauth/token");
    assert_eq!(grants.len(), 2);
    assert_eq!(r.grants(), 2);
    assert_eq!(grants[0].method, "POST");
    assert_eq!(grants[0].header("content-type"), Some("application/x-www-form-urlencoded"));
}

/// A rejected token refreshes once; a rejection of a token already replaced reuses the
/// replacement without another grant.
// spec: connector.rotate.rejected-once@161fc964
#[test]
fn a_rejection_refreshes_once_and_a_stale_rejection_reuses_the_replacement() {
    let _env = proxy_env();
    let server = rotating(3600);
    let (_dir, vars) = store();
    let r = refresher(&server, &vars, &SetClock::new());
    let first = r.access().unwrap();
    r.rejected(|held| held.reveal() == first.reveal()).unwrap();
    assert_eq!(r.access().unwrap().reveal(), "access-2");
    r.rejected(|held| held.reveal() == first.reveal()).unwrap();
    assert_eq!(r.access().unwrap().reveal(), "access-2");
    assert_eq!(server.received("/oauth/token").len(), 2);
}

/// A refused put fails the refresh with `SecretRotationWriteDenied`, and no access token is held
/// or served; an adapter with no write half sends the token endpoint nothing.
// spec: connector.rotate.write-back-denied@13fed6fb
#[test]
fn a_refused_write_back_holds_no_access_token() {
    let _env = proxy_env();
    let server = rotating(3600);
    let (dir, vars) = store();
    let path = dir.path().join("vendor-refresh");
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&path, perms).unwrap();
    let r = refresher(&server, &vars, &SetClock::new());
    for _ in 0..2 {
        let f = r.access().unwrap_err();
        assert!(f.message.starts_with("SecretRotationWriteDenied"), "{f}");
        assert!(!f.message.contains("access-") && !f.message.contains("refresh-"), "{f}");
    }
    assert_eq!(server.received("/oauth/token").len(), 2, "each attempt refreshes anew; none serves the refused grant");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "refresh-0");

    let env: BTreeMap<String, String> = [("VENDOR_REFRESH", "refresh-0"), ("VENDOR_CLIENT_SECRET", "client-secret"), ("CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES", "1")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let quiet = rotating(3600);
    let r = refresher(&quiet, &env, &SetClock::new());
    let f = r.access().unwrap_err();
    assert!(f.message.starts_with("SecretRotationWriteDenied") && f.message.contains("`env`"), "{f}");
    assert!(quiet.received("/oauth/token").is_empty(), "no write half, no grant");
}

/// The token request is sent once and retried by nothing below the run: a `503` is one request
/// and a transient failure, a `429` carries its `Retry-After`, a `400` raises
/// `SecretRefreshRejected`.
// spec: connector.rotate.one-retry-layer@7def2c94
// spec: connector.rotate.refresh-refused@30fee3d1
// spec: connector.rotate.refresh-grant@ab9f8372
#[test]
fn each_refresh_is_one_request_under_the_runs_retry() {
    let _env = proxy_env();
    let (_dir, vars) = store();
    for (status, tag) in [(503, FailureTag::Transient), (429, FailureTag::RateLimited), (400, FailureTag::AuthExpired)] {
        let server = Server::start(move |_| {
            let mut resp = Response::json(status, r#"{"error":"x"}"#);
            resp.headers.push(("Retry-After".into(), "7".into()));
            resp
        });
        let f = refresher(&server, &vars, &SetClock::new()).access().unwrap_err();
        assert_eq!(f.tag, tag, "{status}: {f}");
        assert_eq!(server.received("/oauth/token").len(), 1, "{status}: the token client retried");
        if status == 429 {
            assert_eq!(f.retry_after_secs, Some(7));
        }
        if status == 400 {
            assert!(f.message.starts_with("SecretRefreshRejected"), "{f}");
        }
    }
}
