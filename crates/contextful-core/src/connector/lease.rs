//! `connector.lease`: the declared lease scopes, the mint response, the answer classes
//! and the cache window of a leased value.

use super::reference::{Hydrated, SecretName};
use super::ConnectorError;
use crate::run::{Failure, FailureTag};
use crate::time::Instant;
use serde_json::Value;

/// A cached value lives at most 300 s (`connector.resolve.cache-ttl`).
pub const CACHE_TTL_SECS: u64 = 300;
/// Share of a lease window a cached entry retires early: 10 percent (`connector.lease.retirement-margin`).
pub const RETIRE_SHARE_PERCENT: u64 = 10;
/// Ceiling on the early-retirement margin: 30 s.
pub const RETIRE_CAP_SECS: u64 = 30;
/// Mint calls per resolver per declared name per lease window: 1 call (`connector.lease.calls-per-resolver`).
pub const MINT_CALLS_PER_WINDOW: u32 = 1;

/// The names hydrating as leases, each with the scope it mints (`connector.lease.scope-declaration`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scopes(pub Vec<(SecretName, String)>);

impl Scopes {
    /// Parse `CONTEXTFUL_LEASE_SCOPES`: comma-separated `name` or `name=scope`.
    pub fn parse(s: &str) -> Result<Scopes, ConnectorError> {
        let mut out = Vec::new();
        for item in s.split(',').map(str::trim).filter(|i| !i.is_empty()) {
            let (name, scope) = item.split_once('=').map_or((item, item), |(n, sc)| (n.trim(), sc.trim()));
            out.push((SecretName::parse(name)?, scope.to_string()));
        }
        Ok(Scopes(out))
    }

    pub fn scope(&self, name: &SecretName) -> Option<&str> {
        self.0.iter().find(|(n, _)| n == name).map(|(_, s)| s.as_str())
    }

    /// Hold the declaration at startup: the backend selected with no scope refuses, and
    /// so does the bootstrap name among the leased ones.
    pub fn check(&self, bootstrap: &SecretName) -> Result<(), ConnectorError> {
        if self.0.is_empty() {
            return Err(ConnectorError::SecretLeaseScopesEmpty("the lease backend is selected and `CONTEXTFUL_LEASE_SCOPES` declares no name".into()));
        }
        if self.scope(bootstrap).is_some() {
            return Err(ConnectorError::SecretBootstrapLeased(format!(
                "`{bootstrap}` mints every lease and cannot itself be leased"
            )));
        }
        Ok(())
    }
}

/// A lease: material and one expiry, held together in memory for the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub value: Hydrated,
    pub expires_at: Instant,
}

fn expiry(v: &Value, now: Instant) -> Option<Instant> {
    if let Some(secs) = v.get("expires_in").and_then(Value::as_u64) {
        return Some(now.plus_secs(secs));
    }
    match v.get("expires_at")? {
        Value::Number(n) => n.as_i64().and_then(|s| Instant::from_unix_secs(s).ok()),
        Value::String(s) => Instant::parse(s).ok(),
        _ => None,
    }
}

/// Read a mint success: `value` and one expiry, at the top level or under `lease`
/// (`connector.lease.response`). No expiry, or one already past, refuses as a transient
/// `SecretMalformedLease`.
pub fn read_response(body: &[u8], now: Instant) -> Result<Lease, Failure> {
    let malformed = |why: &str| Failure::new(FailureTag::Transient, ConnectorError::SecretMalformedLease(why.to_string()).to_string());
    let v: Value = serde_json::from_slice(body).map_err(|_| malformed("the lease response is not JSON"))?;
    let obj = v.get("lease").filter(|l| l.is_object()).unwrap_or(&v);
    let value = obj.get("value").and_then(Value::as_str).ok_or_else(|| malformed("the lease carries no `value`"))?;
    let expires_at = expiry(obj, now).ok_or_else(|| malformed("the lease carries no expiry"))?;
    if expires_at <= now {
        return Err(malformed("the lease expired before it arrived"));
    }
    Ok(Lease { value: Hydrated::new(value), expires_at })
}

/// The failure a non-success mint answer is (`connector.lease.unknown-scope` through
/// `connector.lease.transient-class`).
pub fn classify(status: u16, retry_after_secs: Option<u64>, name: &SecretName) -> Failure {
    match status {
        300..=399 => Failure::deterministic(FailureTag::Config, ConnectorError::SecretLeaseRedirect(format!("the mint endpoint redirected the request for `{name}`; the mint credential is not replayed")).to_string()),
        404 => Failure::deterministic(FailureTag::Config, ConnectorError::SecretLeaseScopeUnknown(format!("the provider knows no scope for `{name}`")).to_string()),
        401 | 403 => Failure::new(FailureTag::AuthExpired, ConnectorError::SecretMintRejected(format!("the provider refused the mint credential ({status}) for `{name}`")).to_string()),
        429 => {
            let f = Failure::new(FailureTag::RateLimited, format!("the lease provider throttled the mint for `{name}`"));
            match retry_after_secs {
                Some(s) => f.with_retry_after(s),
                None => f,
            }
        }
        400..=499 => Failure::deterministic(FailureTag::Config, ConnectorError::SecretLeaseRequestRejected(format!("the provider rejected the mint for `{name}` ({status})")).to_string()),
        _ => Failure::new(FailureTag::Transient, format!("the lease provider answered {status} for `{name}`")),
    }
}

/// When a cached entry obtained at `obtained` retires: at the cache TTL, or for a lease at
/// the tighter of the TTL and its expiry, pulled forward by 10 percent of that window,
/// at most 30 s.
pub fn retires_at(obtained: Instant, expires_at: Option<Instant>) -> Instant {
    let ttl_end = obtained.plus_secs(CACHE_TTL_SECS);
    let Some(expiry) = expires_at else { return ttl_end };
    let end = expiry.min(ttl_end);
    let window = obtained.secs_until(end);
    let margin = (window * RETIRE_SHARE_PERCENT / 100).min(RETIRE_CAP_SECS);
    end.minus_secs(margin)
}
