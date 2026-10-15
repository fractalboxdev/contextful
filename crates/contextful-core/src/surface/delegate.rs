//! `surface.delegate`: a store-driven item's body on deploy-bound code outside the binary.
//! The binding a daemon's `[control.delegates.<name>]` declares (`binding`, `host`,
//! `call-timeout`, `call-timeout-ceiling`, `version`), the envelope one request carries
//! (`envelope`), the signatures on request and answer (`signed`, `rejected`) and the closed
//! outcome vocabulary an answer uses (`outcome`).

use super::worker::{constant_eq, hex, hmac_sha256, CALLBACK_SKEW_SECS, SIGNATURE_HEADER, STEP_RESULT_CAP, TIMESTAMP_HEADER};
use super::SurfaceError;
use crate::run::ports::Row;
use crate::time::Instant;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use url::Url;

/// The virtual host a deploy's outbound handler intercepts for a delegate
/// (`surface.delegate.host`).
pub const VIRTUAL_HOST: &str = "contextful.delegate";

/// A `call` delegate's default timeout, in seconds (`surface.delegate.call-timeout`).
pub const CALL_TIMEOUT_SECS: u64 = 300;

/// The longest `call` timeout a binding declares, in seconds
/// (`surface.delegate.call-timeout-ceiling`).
pub const CALL_TIMEOUT_CEILING_SECS: u64 = 3600;

/// The prefix a delegated body is named under inside a job declaration: `delegate:<name>`.
pub const BODY_PREFIX: &str = "delegate:";

/// How a delegate takes its request (`surface.delegate.call-mode`, `surface.delegate.submit-mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// One request held open; its response body is the answer.
    Call,
    /// The worker protocol: answered at once, heartbeats, and a callback.
    Submit,
}

/// One `[control.delegates.<name>]` binding (`surface.delegate.binding`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub name: String,
    pub url: Url,
    pub mode: Mode,
    /// How long a `call` request stays open before its attempt closes.
    pub timeout_secs: u64,
    /// A version the operator rotates when the delegate's semantics change; it joins the
    /// owner pins (`surface.delegate.version`).
    pub version: Option<String>,
}

/// Why a binding does not read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BindingError {
    #[error("{0}")]
    Refused(#[from] SurfaceError),
    /// A binding missing a key, or carrying one outside its shape.
    #[error("{0}")]
    Invalid(String),
}

impl Binding {
    /// Read one binding from its table under `[control.delegates]`.
    pub fn parse(name: &str, table: &toml::value::Table) -> Result<Binding, BindingError> {
        let invalid = |m: String| BindingError::Invalid(format!("`[control.delegates.{name}]` {m}"));
        if name.trim().is_empty() {
            return Err(BindingError::Invalid("a delegate has a name".into()));
        }
        let url = table.get("url").and_then(|v| v.as_str()).ok_or_else(|| invalid("declares `url`".into()))?;
        let url = host_admitted(name, url)?;
        let mode = match table.get("mode").map(|v| v.as_str()) {
            None | Some(Some("call")) => Mode::Call,
            Some(Some("submit")) => Mode::Submit,
            Some(other) => return Err(invalid(format!("`mode` is `call` or `submit`, not {other:?}"))),
        };
        let timeout_secs = match table.get("timeout") {
            None => CALL_TIMEOUT_SECS,
            Some(v) => {
                let text = v.as_str().ok_or_else(|| invalid("`timeout` is a duration string such as \"300s\"".into()))?;
                let secs = crate::time::duration_secs(text).ok_or_else(|| invalid(format!("`timeout` `{text}` is `<n>s`, `<n>m`, `<n>h` or `<n>d`")))?;
                if secs == 0 {
                    return Err(invalid("`timeout` is at least 1 s".into()));
                }
                if secs > CALL_TIMEOUT_CEILING_SECS {
                    return Err(SurfaceError::DelegateTimeoutRefused(format!(
                        "`[control.delegates.{name}] timeout` of {secs} s passes the {CALL_TIMEOUT_CEILING_SECS} s ceiling"
                    ))
                    .into());
                }
                secs
            }
        };
        let version = match table.get("version") {
            None => None,
            Some(v) => Some(v.as_str().filter(|s| !s.trim().is_empty()).ok_or_else(|| invalid("`version` is a non-empty string".into()))?.to_string()),
        };
        for key in table.keys() {
            if !matches!(key.as_str(), "url" | "mode" | "timeout" | "version") {
                return Err(invalid(format!("carries unknown key `{key}`")));
            }
        }
        Ok(Binding { name: name.to_string(), url, mode, timeout_secs, version })
    }

    /// The name a job's `body = { delegate = "<name>" }` resolves to.
    pub fn body_name(&self) -> String {
        format!("{BODY_PREFIX}{}", self.name)
    }
}

/// Read every binding under a `[control]` table's `delegates` key, by name.
pub fn bindings(control: &toml::value::Table) -> Result<BTreeMap<String, Binding>, BindingError> {
    let mut out = BTreeMap::new();
    let Some(delegates) = control.get("delegates") else { return Ok(out) };
    let delegates = delegates.as_table().ok_or_else(|| BindingError::Invalid("`[control.delegates]` is a table of bindings".into()))?;
    for (name, value) in delegates {
        let table = value.as_table().ok_or_else(|| BindingError::Invalid(format!("`[control.delegates.{name}]` is a table")))?;
        out.insert(name.clone(), Binding::parse(name, table)?);
    }
    Ok(out)
}

/// A delegate URL is `http` to a loopback host or [`VIRTUAL_HOST`], carrying no credential
/// in its userinfo, query or fragment; anything else raises `DelegateHostRefused`
/// (`surface.delegate.host`).
fn host_admitted(name: &str, text: &str) -> Result<Url, SurfaceError> {
    let refused = |why: &str| SurfaceError::DelegateHostRefused(format!("`[control.delegates.{name}] url` `{text}` {why}"));
    let url = Url::parse(text).map_err(|_| refused("is a URL"))?;
    if url.scheme() != "http" {
        return Err(refused("is plain `http`: the stock container reaches a delegate over loopback or the deploy's outbound interception"));
    }
    let loopback = match url.host() {
        Some(url::Host::Domain(domain)) => domain == "localhost" || domain == VIRTUAL_HOST,
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => return Err(refused("names a host")),
    };
    if !loopback {
        return Err(refused(&format!("names a loopback host or `{VIRTUAL_HOST}`")));
    }
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err(refused("carries no credential: no userinfo, query or fragment"));
    }
    Ok(url)
}

/// The entity an isolated item runs under, with the fence its lease holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entity {
    pub namespace: String,
    pub key: String,
    pub fence: u64,
}

/// What one delegate request carries (`surface.delegate.envelope`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub job: String,
    pub execution_id: String,
    /// The row-scoped step label the answer records under.
    pub step: String,
    /// The SHA-256 hex of the input row's canonical bytes.
    pub input_hash: String,
    /// The delegate call's ordinal within the item's journal.
    pub attempt: u32,
    pub idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<Entity>,
    pub as_of: String,
    /// The applied snapshot version the job fires from; absent for a fire from the local
    /// declaration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
    /// The execution's owner pins, as the engine compares them.
    pub pins: BTreeMap<String, String>,
    pub tables: Vec<String>,
    pub row: Row,
    /// The send instant plus the delegate's timeout on the engine's clock, RFC 3339 UTC.
    pub deadline: String,
}

/// The hex signature a `call` request carries: HMAC-SHA256 under `key` over
/// `call\n<timestamp>\n` and the body (`surface.delegate.signed`).
pub fn sign_request(key: &[u8], timestamp: i64, body: &[u8]) -> String {
    let mut message = format!("call\n{timestamp}\n").into_bytes();
    message.extend_from_slice(body);
    hex(&hmac_sha256(key, &message))
}

/// The hex signature a `call` answer carries: HMAC-SHA256 under `key` over
/// `answer\n<idempotency_key>\n<timestamp>\n` and the body, so an answer verifies only
/// for the request it answers (`surface.delegate.signed`).
pub fn sign_answer(key: &[u8], idempotency_key: &str, timestamp: i64, body: &[u8]) -> String {
    let mut message = format!("answer\n{idempotency_key}\n{timestamp}\n").into_bytes();
    message.extend_from_slice(body);
    hex(&hmac_sha256(key, &message))
}

/// Admit a `call` answer to the request carrying `idempotency_key`: signed under `key`
/// over that key, its timestamp and body, the timestamp within [`CALLBACK_SKEW_SECS`] of
/// `now`; anything else raises `DelegateAnswerRejected` (`surface.delegate.rejected`).
pub fn verify_answer(key: &[u8], idempotency_key: &str, header: impl Fn(&str) -> Option<String>, body: &[u8], now: Instant) -> Result<(), SurfaceError> {
    let rejected = |why: String| SurfaceError::DelegateAnswerRejected(why);
    let (Some(timestamp), Some(signature)) = (header(TIMESTAMP_HEADER), header(SIGNATURE_HEADER)) else {
        return Err(rejected(format!("an answer carries `{TIMESTAMP_HEADER}` and `{SIGNATURE_HEADER}`")));
    };
    let timestamp: i64 = timestamp.parse().map_err(|_| rejected(format!("`{TIMESTAMP_HEADER}` `{timestamp}` is no Unix second")))?;
    if !constant_eq(&sign_answer(key, idempotency_key, timestamp, body), &signature) {
        return Err(rejected("the answer's signature does not verify under the deployment key for this request".into()));
    }
    let skew = now.unix_secs().abs_diff(timestamp);
    if skew > CALLBACK_SKEW_SECS {
        return Err(rejected(format!("the answer's timestamp is {skew} s from this clock, past {CALLBACK_SKEW_SECS} s")));
    }
    Ok(())
}

/// A delegate's answer (`surface.delegate.outcome`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum Answer {
    /// Rows per declared output table; they land as the item's rows.
    Ok { rows: BTreeMap<String, Vec<Row>> },
    /// Park the item until `retry_at`, spending no attempt (`surface.delegate.defer-parks`).
    Defer {
        retry_at: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Spend one failed attempt.
    Transient {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Write a terminal receipt.
    Permanent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl Answer {
    /// Read an answer body under the step-result cap, in the outcome vocabulary, its rows
    /// only for tables in `tables`; anything else raises `DelegateAnswerRejected`
    /// (`surface.delegate.rejected`, `surface.delegate.outcome`).
    pub fn decode(body: &[u8], tables: &[String]) -> Result<Answer, SurfaceError> {
        let rejected = |why: String| SurfaceError::DelegateAnswerRejected(why);
        if body.len() > STEP_RESULT_CAP {
            return Err(rejected(format!("an answer of {} B exceeds the {STEP_RESULT_CAP} B cap", body.len())));
        }
        let answer: Answer = serde_json::from_slice(body).map_err(|e| rejected(format!("an answer is `ok`, `defer`, `transient` or `permanent`: {e}")))?;
        match &answer {
            Answer::Ok { rows } => {
                if let Some(table) = rows.keys().find(|t| !tables.contains(t)) {
                    return Err(rejected(format!("an `ok` answer carries rows for `{table}`, which the job does not declare in `tables`")));
                }
            }
            Answer::Defer { retry_at, .. } => {
                Instant::parse(retry_at).map_err(|e| rejected(format!("a `defer` answer's `retry_at` is an RFC 3339 UTC instant: {e}")))?;
            }
            Answer::Transient { .. } | Answer::Permanent { .. } => {}
        }
        Ok(answer)
    }

    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("an answer serializes")
    }
}
