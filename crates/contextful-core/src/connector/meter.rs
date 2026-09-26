//! `connector.meter`: the limiter declaration and its operator binding, the acquire and
//! report wire, and the permit pool a mediated client spends one request at a time.

use super::attach::{is_loopback_host, scrub};
use super::reference::{SecretName, SCHEME};
use super::ConnectorError;
use crate::time::Instant;
use std::collections::BTreeMap;
use url::Url;

/// Permits one acquire asks for when the binding names no batch size.
pub const DEFAULT_PERMITS: u32 = 8;
/// Seconds a granted batch stays spendable when the limiter names no TTL.
pub const DEFAULT_TTL_SECS: u64 = 10;
/// Seconds a denial waits when the limiter names no retry-after.
pub const DEFAULT_RETRY_AFTER_SECS: u64 = 1;

/// What a connector declares about the shared vendor quota its requests draw on
/// (`connector.meter.limiter-declaration`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimiterDeclaration {
    /// The shared quota's name, the key an operator binding answers.
    pub quota: String,
    /// The traffic class every request reserves under.
    pub class: String,
    /// Vendor quota-state response headers forwarded verbatim, lower-cased.
    pub forward: Vec<String>,
}

impl LimiterDeclaration {
    pub fn new<S: AsRef<str>>(quota: &str, class: &str, forward: &[S]) -> LimiterDeclaration {
        LimiterDeclaration {
            quota: quota.to_string(),
            class: class.to_string(),
            forward: forward.iter().map(|h| h.as_ref().to_ascii_lowercase()).collect(),
        }
    }

    /// Whether a response header is one this declaration forwards.
    pub fn forwards(&self, header: &str) -> bool {
        self.forward.iter().any(|h| h.eq_ignore_ascii_case(header))
    }
}

/// The operator's binding of one quota name (`connector.meter.limiter-binding`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimiterBinding {
    pub quota: String,
    /// The limiter's base URL; `acquire` and `report` hang off it.
    pub endpoint: Url,
    /// The bearer token, held by reference and hydrated per call.
    pub token: SecretName,
    /// Permits one acquire asks for, at least 1.
    pub permits: u32,
}

fn rejected(quota: &str, why: String) -> ConnectorError {
    ConnectorError::ConnectorLimiterBindingRejected(format!("the limiter binding for quota `{quota}` {why}"))
}

impl LimiterBinding {
    /// Read a binding. An endpoint that is neither HTTPS nor loopback, one carrying a
    /// query, fragment or userinfo, and a token that is not a `secret://` reference
    /// raise `ConnectorLimiterBindingRejected` (`connector.meter.binding-transport`). The
    /// refusal never repeats the token, which may be material.
    pub fn parse(quota: &str, endpoint: &str, token: &str, permits: Option<u32>) -> Result<LimiterBinding, ConnectorError> {
        let url = Url::parse(endpoint).map_err(|e| rejected(quota, format!("names an endpoint that is not a URL: {e}")))?;
        if url.scheme() != "https" && !url.host_str().is_some_and(is_loopback_host) {
            return Err(rejected(quota, format!("names `{}`, which is neither HTTPS nor loopback", scrub(&url))));
        }
        if url.query().is_some() || url.fragment().is_some() || !url.username().is_empty() || url.password().is_some() {
            return Err(rejected(quota, format!("names `{}`, which is not a plain base URL", scrub(&url))));
        }
        let Some(name) = token.strip_prefix(SCHEME) else {
            return Err(rejected(quota, format!("writes its token as a literal of {} bytes; it binds as `secret://<name>`", token.len())));
        };
        let token = SecretName::parse(name).map_err(|_| rejected(quota, "names a token reference that is not a logical name".to_string()))?;
        Ok(LimiterBinding { quota: quota.to_string(), endpoint: url, token, permits: permits.unwrap_or(DEFAULT_PERMITS).max(1) })
    }

    /// `<endpoint>/<call>`.
    pub fn call_url(&self, call: &str) -> Url {
        let base = self.endpoint.as_str().trim_end_matches('/');
        Url::parse(&format!("{base}/{call}")).expect("a plain base URL joins a path segment")
    }
}

/// The binding answering `declaration`'s quota, or `ConnectorQuotaUnbound`
/// (`connector.meter.quota-unbound`).
pub fn require_binding<'a>(declaration: &LimiterDeclaration, bindings: &'a BTreeMap<String, LimiterBinding>) -> Result<&'a LimiterBinding, ConnectorError> {
    bindings.get(&declaration.quota).ok_or_else(|| {
        ConnectorError::ConnectorQuotaUnbound(format!(
            "quota `{}` (class `{}`) is declared and no limiter binding names it; bound quotas: {:?}",
            declaration.quota,
            declaration.class,
            bindings.keys().collect::<Vec<_>>()
        ))
    })
}

/// The acquire body: quota, class and permit count, and nothing priced
/// (`connector.meter.acquire`, `connector.meter.counting-not-pricing`).
pub fn acquire_body(quota: &str, class: &str, permits: u32) -> serde_json::Value {
    serde_json::json!({ "quota": quota, "class": class, "permits": permits })
}

/// A limiter's answer to one acquire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Granted { permits: u32, ttl_secs: u64 },
    Denied { retry_after_secs: u64 },
}

impl Decision {
    /// Read a `2xx` acquire body. `decision` discriminates; a body carrying `permits`
    /// alone reads as a grant and one carrying `retry_after_secs` alone as a denial. A
    /// zero-permit grant reads as a denial waiting out its TTL. Anything else raises
    /// `ConnectorLimiterUnreadable` (`connector.meter.unreadable-answer`).
    pub fn read(body: &[u8]) -> Result<Decision, ConnectorError> {
        let unreadable = |why: &str| ConnectorError::ConnectorLimiterUnreadable(format!("the acquire answer {why}"));
        let value: serde_json::Value = serde_json::from_slice(body).map_err(|_| unreadable("is not JSON"))?;
        let number = |key: &str| -> Result<Option<u64>, ConnectorError> {
            match value.get(key) {
                None | Some(serde_json::Value::Null) => Ok(None),
                Some(v) => v.as_u64().map(Some).ok_or_else(|| unreadable(&format!("carries a `{key}` that is not a non-negative integer"))),
            }
        };
        let (permits, ttl, retry) = (number("permits")?, number("ttl_secs")?, number("retry_after_secs")?);
        let decision = match value.get("decision") {
            None => None,
            Some(d) => Some(d.as_str().ok_or_else(|| unreadable("carries a `decision` that is not a string"))?),
        };
        let granted = |permits: u64| {
            let ttl_secs = ttl.unwrap_or(DEFAULT_TTL_SECS);
            match u32::try_from(permits).unwrap_or(u32::MAX) {
                0 => Decision::Denied { retry_after_secs: ttl_secs.max(1) },
                permits => Decision::Granted { permits, ttl_secs },
            }
        };
        match (decision, permits, retry) {
            (Some("denied"), _, retry) => Ok(Decision::Denied { retry_after_secs: retry.unwrap_or(DEFAULT_RETRY_AFTER_SECS) }),
            (Some("granted"), Some(p), _) | (None, Some(p), _) => Ok(granted(p)),
            (Some("granted"), None, _) => Err(unreadable("grants without a `permits` count")),
            (None, None, Some(retry)) => Ok(Decision::Denied { retry_after_secs: retry }),
            (Some(other), _, _) => Err(unreadable(&format!("names decision `{other}`"))),
            (None, None, None) => Err(unreadable("carries no decision")),
        }
    }

    /// A bare `429` reads as a denial carrying its `Retry-After`.
    pub fn throttled(retry_after: Option<&str>, now: Instant) -> Decision {
        Decision::Denied { retry_after_secs: retry_after.and_then(|r| parse_retry_after(r, now)).unwrap_or(DEFAULT_RETRY_AFTER_SECS) }
    }
}

/// `Retry-After` in seconds: a delta-seconds count, or an HTTP date resolved against `now`.
pub fn parse_retry_after(raw: &str, now: Instant) -> Option<u64> {
    let raw = raw.trim();
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(secs);
    }
    let rfc2822 = &time::format_description::well_known::Rfc2822;
    let at = time::OffsetDateTime::parse(raw, rfc2822).or_else(|_| time::OffsetDateTime::parse(&raw.replace(" GMT", " +0000"), rfc2822)).ok()?;
    Some(u64::try_from(at.unix_timestamp() - now.unix_secs()).unwrap_or(0))
}

/// What one vendor response said about the quota.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub status: u16,
    pub retry_after_secs: Option<u64>,
    /// The declared quota-state headers, lower-cased name to verbatim value.
    pub headers: BTreeMap<String, String>,
}

impl Usage {
    /// The usage entry of one response under `declaration`.
    pub fn observe(declaration: &LimiterDeclaration, status: u16, headers: &[(String, String)], now: Instant) -> Usage {
        let retry_after_secs = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("retry-after")).and_then(|(_, v)| parse_retry_after(v, now));
        let headers = headers.iter().filter(|(k, _)| declaration.forwards(k)).map(|(k, v)| (k.to_ascii_lowercase(), v.clone())).collect();
        Usage { status, retry_after_secs, headers }
    }
}

/// One traffic class's live batch and the accounting owed on it
/// (`connector.meter.permit-batch`).
#[derive(Debug, Clone, Default)]
pub struct Pool {
    remaining: u32,
    expires_at: Option<Instant>,
    granted: u32,
    spent: u32,
    usage: Vec<Usage>,
}

impl Pool {
    /// Spend one permit when the batch is live at `now`. An expired batch's permits stay
    /// counted as granted, so the next report surrenders them.
    pub fn take(&mut self, now: Instant) -> bool {
        let live = self.remaining > 0 && self.expires_at.is_some_and(|t| now < t);
        if live {
            self.remaining -= 1;
            self.spent += 1;
        } else {
            self.remaining = 0;
        }
        live
    }

    /// Replace the live batch with `permits` spendable for `ttl_secs` from `now`. The
    /// replaced batch's unspent permits stay counted as granted.
    pub fn grant(&mut self, permits: u32, ttl_secs: u64, now: Instant) {
        self.remaining = permits;
        self.granted = self.granted.saturating_add(permits);
        self.expires_at = Some(now.plus_secs(ttl_secs.max(1)));
    }

    pub fn observe(&mut self, usage: Usage) {
        self.usage.push(usage);
    }

    /// Drain the owed accounting into a report, or `None` when nothing is owed.
    pub fn drain(&mut self, quota: &str, class: &str, run_id: &str, now: Instant) -> Option<Report> {
        if self.granted == 0 && self.spent == 0 && self.usage.is_empty() {
            return None;
        }
        let report = Report {
            quota: quota.to_string(),
            class: class.to_string(),
            granted: self.granted,
            spent: self.spent,
            usage: std::mem::take(&mut self.usage),
            observed_at: now,
            run_id: run_id.to_string(),
        };
        self.granted = 0;
        self.spent = 0;
        Some(report)
    }
}

/// One report body (`connector.meter.report`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub quota: String,
    pub class: String,
    pub granted: u32,
    pub spent: u32,
    pub usage: Vec<Usage>,
    pub observed_at: Instant,
    pub run_id: String,
}

impl Report {
    /// The wire form: counts and verbatim vendor headers, nothing priced
    /// (`connector.meter.counting-not-pricing`).
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "quota": self.quota,
            "class": self.class,
            "granted": self.granted,
            "spent": self.spent,
            "usage": self.usage.iter().map(|u| serde_json::json!({
                "status": u.status,
                "retry_after_secs": u.retry_after_secs,
                "headers": u.headers,
            })).collect::<Vec<_>>(),
            "observed_at": self.observed_at.to_rfc3339(),
            "run_id": self.run_id,
        })
    }
}
