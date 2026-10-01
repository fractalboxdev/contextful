//! The scope probe at session open: the host calls the declared identity endpoint with
//! the bound credential, through the mediated client, and opens the session only when
//! the granted scopes sit within the declared expectation.

use crate::client::{classify, Client, HeaderValue};
use contextful_core::connector::attach::{scrub, Allowlist};
use contextful_core::connector::probe::ScopeProbe;
use contextful_core::connector::reference::Hydrated;
use contextful_core::run::{Failure, FailureTag};

fn refuse(e: contextful_core::connector::ConnectorError) -> Failure {
    Failure::deterministic(FailureTag::Config, e.to_string())
}

/// Call `probe`'s endpoint with the bound credential and judge the grant it reports.
/// Answers the granted scopes. The request takes a client on the manifest's allowlist,
/// so the probe's host and transport meet the rules of every credentialed request
/// (`connector.declare-capability.probe-transport`); a `401`, `403`, `429` or `5xx`
/// classifies as any vendor answer does.
pub fn probe(allow: &Allowlist, probe: &ScopeProbe, credential: (&str, &Hydrated)) -> Result<Vec<String>, Failure> {
    // The grant rides a header, so the identity body is never read.
    let client = Client::new(allow.clone(), probe.endpoint.clone()).without_body();
    probe_through(&client, allow, probe, credential)
}

/// [`probe`] through `client`, the mediated client a source's own requests take, so the
/// probe passes the same hook and reservation as a page request
/// (`connector.meter.reservation-point`). `client`'s origin is the probe endpoint.
pub fn probe_through(client: &Client, allow: &Allowlist, probe: &ScopeProbe, credential: (&str, &Hydrated)) -> Result<Vec<String>, Failure> {
    let (name, value) = credential;
    probe.check_transport(allow, name).map_err(refuse)?;
    let headers = [(name.to_string(), HeaderValue::Sensitive(value.clone()))];
    let resp = client.send("GET", &probe.endpoint, &headers, None)?;
    if !(200..300).contains(&resp.status) {
        let retry_after = resp.header("retry-after").and_then(|v| v.trim().parse().ok());
        return Err(classify(resp.status, retry_after, &format!("the scope probe `{}`", scrub(&resp.url))));
    }
    // Repeated field lines form one comma-separated list (RFC 9110 §5.3), so a scope on
    // any line counts toward the grant.
    let lines: Vec<&str> =
        resp.headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case(&probe.scopes_header)).map(|(_, v)| v.as_str()).collect();
    let granted = (!lines.is_empty()).then(|| lines.join(","));
    probe.judge(granted.as_deref()).map_err(refuse)
}

/// Open a session: with a probe declared, the probe runs first and a refusal returns
/// before `open` runs, so no read precedes a verified grant
/// (`connector.declare-capability.scope-probe`).
pub fn open_session<T>(
    allow: &Allowlist,
    declared: Option<&ScopeProbe>,
    credential: (&str, &Hydrated),
    open: impl FnOnce() -> Result<T, Failure>,
) -> Result<T, Failure> {
    if let Some(p) = declared {
        probe(allow, p, credential)?;
    }
    open()
}
