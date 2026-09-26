//! `connector.declare-capability`'s scope probe: the identity endpoint a manifest
//! declares, the header carrying the granted scopes, the grant it expects, and the
//! judgment of an answer against that expectation.

use super::attach::{check_transport, endpoint, scrub, Allowlist};
use super::ConnectorError;
use std::collections::BTreeSet;
use url::Url;

/// A declared scope probe (`connector.declare-capability.scope-probe`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeProbe {
    /// The identity endpoint the host calls with the bound credential.
    pub endpoint: Url,
    /// The response header carrying the granted scopes.
    pub scopes_header: String,
    /// The expected grant: a ceiling, so a scope absent from it refuses whether or not
    /// it is known.
    pub expect: Vec<String>,
}

impl ScopeProbe {
    /// Declare a probe. An endpoint carrying userinfo or not an http(s) URL refuses as
    /// any configured endpoint does. An empty expectation admits no granted scope.
    pub fn new<S: AsRef<str>>(endpoint_raw: &str, scopes_header: &str, expect: &[S]) -> Result<ScopeProbe, ConnectorError> {
        Ok(ScopeProbe {
            endpoint: endpoint("scope_probe", endpoint_raw)?,
            scopes_header: scopes_header.trim().to_string(),
            expect: expect.iter().map(|s| s.as_ref().trim().to_string()).filter(|s| !s.is_empty()).collect(),
        })
    }

    /// Hold the probe to the manifest's allowlist and to TLS or loopback, ahead of any
    /// I/O (`connector.declare-capability.probe-transport`). The probe carries the bound
    /// credential in `header`.
    pub fn check_transport(&self, allow: &Allowlist, header: &str) -> Result<(), ConnectorError> {
        if !allow.permits(self.endpoint.host_str().unwrap_or_default()) {
            return Err(ConnectorError::SecretUnpermittedRequest(format!("the scope probe's `{}` is not a host the allowlist covers", scrub(&self.endpoint))));
        }
        check_transport(&self.endpoint, header)
    }

    /// The granted scopes outside the expectation, in granted order.
    pub fn violations(&self, granted: &str) -> Vec<String> {
        scope_violations(granted, &self.expect)
    }

    /// Judge the probe's granted-scopes header value, absent when the response carried
    /// none. Answers the granted scopes when every one sits within the expectation.
    pub fn judge(&self, granted: Option<&str>) -> Result<Vec<String>, ConnectorError> {
        let Some(granted) = granted else {
            return Err(ConnectorError::ConnectorScopeUnverified(format!(
                "`{}` answered no `{}` header; a grant the vendor does not state is not verified",
                scrub(&self.endpoint),
                self.scopes_header
            )));
        };
        let over = self.violations(granted);
        if !over.is_empty() {
            return Err(ConnectorError::ConnectorScopeExceeded(format!(
                "the bound credential is granted {} beyond the expected {}",
                over.join(", "),
                self.expect.join(", ")
            )));
        }
        Ok(scopes(granted).map(str::to_string).collect())
    }
}

/// The scopes of a granted-scopes header value, separated by commas or whitespace.
fn scopes(granted: &str) -> impl Iterator<Item = &str> {
    granted.split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty())
}

/// The scopes of `granted` absent from `expect`, compared case-insensitively, in granted order.
pub fn scope_violations(granted: &str, expect: &[String]) -> Vec<String> {
    let expect: BTreeSet<String> = expect.iter().map(|s| s.trim().to_ascii_lowercase()).collect();
    scopes(granted).filter(|s| !expect.contains(&s.to_ascii_lowercase())).map(str::to_string).collect()
}
