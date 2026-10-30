//! `connector.rotate`: the write half a credential store contributes to turnover, a
//! blind versioned put conditioned on the version a run hydrated, and the OAuth refresh
//! policy a connector manifest's `[auth]` table declares.

use super::attach::{is_loopback_host, scrub};
use super::lease::{RETIRE_CAP_SECS, RETIRE_SHARE_PERCENT};
use super::reference::{Hydrated, SecretName, SCHEME};
use super::ConnectorError;
use crate::run::{Failure, FailureTag};
use crate::time::Instant;
use serde_json::Value;
use url::Url;

/// When a credential obtained at `obtained` and expiring at `expires_at` refreshes: ahead
/// of expiry by the margin `connector.lease.retirement-margin` computes, 10 percent of
/// the window at most 30 s (`connector.rotate.oauth-credential`).
pub fn refresh_at(obtained: Instant, expires_at: Instant) -> Instant {
    let window = obtained.secs_until(expires_at);
    expires_at.minus_secs((window * RETIRE_SHARE_PERCENT / 100).min(RETIRE_CAP_SECS))
}

/// An opaque version a store assigns to a stored value.
pub type Version = String;

/// Why a store refused or failed a put.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PutError {
    /// The store refused the write (`connector.rotate.write-back-denied`).
    Denied(String),
    /// Another writer stored this version first (`connector.rotate.conditional-put`).
    Conflict(Version),
    /// The write did not complete.
    Failed(String),
}

/// A store's write half (`connector.rotate.division-of-labor`). Of the linked adapters
/// only `file` carries one; the environment, the lease provider and AWS Secrets Manager
/// never receive a write-back (`connector.rotate.write-halves`).
pub trait VersionedStore: Send + Sync {
    /// The adapter's name, as attribution records it.
    fn adapter(&self) -> &str;
    /// Store `value` for `name` when the current version is `if_version`, with an opaque
    /// expiry hint beside it (`connector.rotate.division-of-labor`).
    fn put(&self, name: &SecretName, value: &Hydrated, if_version: Option<&str>, expires_hint: Option<Instant>) -> Result<Version, PutError>;
}

/// A connector's OAuth refresh policy, read from its manifest's `[auth]` table
/// (`connector.rotate.auth-policy`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthPolicy {
    /// The token endpoint every refresh posts to, TLS or loopback.
    pub token_endpoint: Url,
    /// The OAuth client identifier the refresh form carries.
    pub client_id: String,
    /// The name of the client secret the refresh form carries.
    pub client_secret: SecretName,
    /// The name of the refresh token; its answering adapter stores each replacement.
    pub refresh_token: SecretName,
    /// The name attach templates reference for the access token the host holds.
    pub access_token: SecretName,
    /// The scope each refresh requests.
    pub scope: String,
}

fn auth_rejected(key: &str, why: impl std::fmt::Display) -> ConnectorError {
    ConnectorError::ConnectorManifestRejected(format!("`auth.{key}` {why}"))
}

/// The one `${secret://<name>}` reference `value` holds, refusing anything else under `key`
/// without repeating the value, which may be material.
fn one_reference(key: &str, value: &str) -> Result<SecretName, ConnectorError> {
    let inner = value.strip_prefix("${").and_then(|v| v.strip_suffix('}')).and_then(|v| v.strip_prefix(SCHEME));
    let refused = || auth_rejected(key, "holds other than one `${secret://<name>}` reference");
    SecretName::parse(inner.ok_or_else(refused)?).map_err(|_| refused())
}

impl AuthPolicy {
    /// Hold an `[auth]` table to its shape: a TLS or loopback endpoint carrying no
    /// userinfo, a non-empty client id and scope, and three distinct references, each
    /// refusal raising `ConnectorManifestRejected` naming the key
    /// (`connector.rotate.auth-refused`).
    pub fn new(token_endpoint: &str, client_id: &str, client_secret: &str, refresh_token: &str, access_token: &str, scope: &str) -> Result<AuthPolicy, ConnectorError> {
        let endpoint = Url::parse(token_endpoint).map_err(|e| auth_rejected("token_endpoint", format_args!("is not a URL: {e}")))?;
        let host = endpoint.host_str().unwrap_or_default().to_string();
        let transport_ok = endpoint.scheme() == "https" || (endpoint.scheme() == "http" && is_loopback_host(&host));
        if !transport_ok || host.is_empty() {
            return Err(auth_rejected("token_endpoint", format_args!("`{}` is neither TLS nor loopback", scrub(&endpoint))));
        }
        if !endpoint.username().is_empty() || endpoint.password().is_some() {
            return Err(auth_rejected("token_endpoint", format_args!("`{}` carries userinfo", scrub(&endpoint))));
        }
        if client_id.trim().is_empty() {
            return Err(auth_rejected("client_id", "is empty"));
        }
        if scope.trim().is_empty() {
            return Err(auth_rejected("scope", "is empty"));
        }
        let client_secret = one_reference("client_secret", client_secret)?;
        let refresh_token = one_reference("refresh_token", refresh_token)?;
        let access_token = one_reference("access_token", access_token)?;
        if access_token == refresh_token || access_token == client_secret {
            return Err(auth_rejected("access_token", format_args!("names `{access_token}`, which another `auth` reference names")));
        }
        if refresh_token == client_secret {
            return Err(auth_rejected("refresh_token", format_args!("names `{refresh_token}`, which `auth.client_secret` names")));
        }
        Ok(AuthPolicy { token_endpoint: endpoint, client_id: client_id.to_string(), client_secret, refresh_token, access_token, scope: scope.to_string() })
    }

    /// The refresh form: `grant_type=refresh_token`, the refresh token, the scope, the
    /// client id and the client secret (`connector.rotate.refresh-grant`).
    pub fn form(&self, refresh: &Hydrated, secret: &Hydrated) -> Hydrated {
        Hydrated::new(
            url::form_urlencoded::Serializer::new(String::new())
                .append_pair("grant_type", "refresh_token")
                .append_pair("refresh_token", refresh.reveal())
                .append_pair("scope", &self.scope)
                .append_pair("client_id", &self.client_id)
                .append_pair("client_secret", secret.reveal())
                .finish(),
        )
    }
}

/// A token endpoint's answer to a refresh: the access token, its expiry when the answer
/// carries a positive `expires_in`, and a replacement refresh token when it rotates one.
#[derive(Debug, PartialEq, Eq)]
pub struct TokenAnswer {
    pub access_token: Hydrated,
    pub expires_at: Option<Instant>,
    pub refresh_token: Option<Hydrated>,
}

fn refused(why: String) -> Failure {
    Failure::new(FailureTag::AuthExpired, ConnectorError::SecretRefreshRejected(why).to_string())
}

/// Read a successful refresh answer from `endpoint`, received at `now`. A body that is no
/// JSON object or carries no `access_token` raises `SecretRefreshRejected`, naming no value
/// (`connector.rotate.refresh-refused`).
pub fn read_token_answer(body: &[u8], now: Instant, endpoint: &str) -> Result<TokenAnswer, Failure> {
    let v: Value = serde_json::from_slice(body).map_err(|_| refused(format!("`{endpoint}` answered no JSON object")))?;
    let text = |k: &str| v.get(k).and_then(Value::as_str).filter(|t| !t.is_empty()).map(Hydrated::new);
    let access_token = text("access_token").ok_or_else(|| refused(format!("`{endpoint}` answered no `access_token`")))?;
    let expires_at = v.get("expires_in").and_then(Value::as_u64).filter(|s| *s > 0).map(|s| now.plus_secs(s));
    Ok(TokenAnswer { access_token, expires_at, refresh_token: text("refresh_token") })
}

/// The failure a non-success refresh answer from `endpoint` is: a `429` carries its
/// `Retry-After`, a redirect or another `4xx` raises `SecretRefreshRejected`, and anything
/// else is transient. The token client retries nothing itself, so the run's retry schedule
/// is the one retry layer (`connector.rotate.one-retry-layer`).
pub fn classify_refresh(status: u16, retry_after_secs: Option<u64>, endpoint: &str) -> Failure {
    match status {
        429 => {
            let f = Failure::new(FailureTag::RateLimited, format!("`{endpoint}` throttled the refresh"));
            retry_after_secs.map_or(f.clone(), |s| f.with_retry_after(s))
        }
        300..=499 => refused(format!("`{endpoint}` answered the refresh with {status}")),
        _ => Failure::new(FailureTag::Transient, format!("`{endpoint}` answered the refresh with {status}")),
    }
}
