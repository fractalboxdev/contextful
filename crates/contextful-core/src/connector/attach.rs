//! `connector.attach` and `connector.declare-capability`: the outbound allowlist, the
//! addresses a permitted name may resolve to, the origin every hop keeps, and the URL
//! form that reaches a message.

use super::ConnectorError;
use std::net::IpAddr;
use url::Url;

/// One allowlist entry: an exact host, or `*.<domain>` covering every subdomain and not the apex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostEntry {
    Exact(String),
    Subdomains(String),
}

/// The outbound allowlist (`connector.declare-capability.host-allowlist`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allowlist(pub Vec<HostEntry>);

impl Allowlist {
    /// Parse the entries. An empty list, a bare wildcard, an empty entry, and an entry
    /// carrying a scheme, port or path refuse (`connector.declare-capability.allowlist-shape`).
    pub fn parse<S: AsRef<str>>(entries: &[S]) -> Result<Allowlist, ConnectorError> {
        let reject = |why: String| Err(ConnectorError::ConnectorAllowlistRejected(why));
        if entries.is_empty() {
            return reject("the allowlist is empty".into());
        }
        let mut out = Vec::new();
        for e in entries.iter().map(AsRef::as_ref) {
            let t = e.trim();
            if t.is_empty() {
                return reject("an allowlist entry is empty".into());
            }
            if t == "*" || t == "*." {
                return reject("a bare wildcard admits every host".into());
            }
            // A bracketed IPv6 literal is a host; its colons are no port.
            if let Some(inner) = t.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
                if inner.parse::<std::net::Ipv6Addr>().is_ok() {
                    out.push(HostEntry::Exact(format!("[{}]", inner.to_ascii_lowercase())));
                    continue;
                }
                return reject(format!("entry `{t}` is not a bracketed IPv6 literal"));
            }
            if t.contains("://") || t.contains('/') || t.contains(':') || t.contains('?') || t.contains('@') {
                return reject(format!("entry `{t}` carries a scheme, port or path; an entry is a host"));
            }
            let lower = t.to_ascii_lowercase();
            out.push(match lower.strip_prefix("*.") {
                Some(domain) if !domain.is_empty() && !domain.contains('*') => HostEntry::Subdomains(domain.to_string()),
                Some(_) => return reject(format!("entry `{t}` is not `*.<domain>`")),
                None if lower.contains('*') => return reject(format!("entry `{t}` places a wildcard inside a name")),
                None => HostEntry::Exact(lower),
            });
        }
        Ok(Allowlist(out))
    }

    /// Whether `host` is covered: an exact match, or a proper subdomain of a wildcard entry.
    pub fn permits(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.0.iter().any(|e| match e {
            HostEntry::Exact(h) => *h == host,
            HostEntry::Subdomains(d) => proper_subdomain(&host, d),
        })
    }

    /// Sufficient host inclusion check over parsed entries. A false answer alone does
    /// not establish widening (`connector.widen.host-inclusion`).
    pub fn included_in(&self, predecessor: &Allowlist) -> bool {
        self.0.iter().all(|candidate| predecessor.0.iter().any(|parent| match (parent, candidate) {
            (HostEntry::Exact(p), HostEntry::Exact(c)) => p == c,
            (HostEntry::Exact(_), HostEntry::Subdomains(_)) => false,
            (HostEntry::Subdomains(p), HostEntry::Exact(c)) => proper_subdomain(c, p),
            (HostEntry::Subdomains(p), HostEntry::Subdomains(c)) => p == c || c.ends_with(&format!(".{p}")),
        }))
    }

    /// A concrete newly permitted host, replayed through both outbound matchers.
    /// Absence supplies no inclusion verdict (`connector.widen.host-witness`).
    pub fn widening_witness(&self, predecessor: &Allowlist) -> Option<String> {
        // More distinct labels than predecessor entries: exact hosts cannot cover all
        // probes; a narrower wildcard cannot cover a direct child of this suffix.
        self.0.iter().find_map(|entry| match entry {
            HostEntry::Exact(host) => (self.permits(host) && !predecessor.permits(host)).then(|| host.clone()),
            HostEntry::Subdomains(domain) => (0..=predecessor.0.len())
                .map(|n| format!("w{n}.{domain}"))
                .find(|host| self.permits(host) && !predecessor.permits(host)),
        })
    }

    /// A source binding a credential carries one non-wildcard host (`connector.attach.bound-host`).
    pub fn check_bound(&self) -> Result<&str, ConnectorError> {
        match self.0.as_slice() {
            [HostEntry::Exact(h)] => Ok(h),
            _ if self.0.iter().any(|e| matches!(e, HostEntry::Subdomains(_))) => Err(ConnectorError::SecretWildcardHost(
                "a source binding a credential declares a wildcard host; a credential attaches to one exact host".into(),
            )),
            _ => Err(ConnectorError::SecretWildcardHost("a source binding a credential declares more than one host".into())),
        }
    }
}

fn proper_subdomain(host: &str, domain: &str) -> bool {
    host.len() > domain.len() + 1 && host.ends_with(&format!(".{domain}"))
}

/// Parse a configured endpoint. One carrying userinfo refuses (`connector.attach.credential-in-a-url`).
pub fn endpoint(source: &str, raw: &str) -> Result<Url, ConnectorError> {
    let url = Url::parse(raw).map_err(|e| ConnectorError::SecretUnpermittedRequest(format!("source `{source}`: `{}` is not a URL: {e}", scrub_text(raw))))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ConnectorError::SecretCredentialInUrl(format!(
            "source `{source}` configures `{}` with userinfo; bind the credential as a header template",
            scrub(&url)
        )));
    }
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(ConnectorError::SecretUnpermittedRequest(format!("source `{source}`: `{}` is not an http(s) URL with a host", scrub(&url))));
    }
    Ok(url)
}

/// The form of a URL that reaches a message, a log line or a landed column: scheme,
/// host, port and path (`connector.attach.url-scrubbing`).
pub fn scrub(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(p) => format!("{}://{host}:{p}{}", url.scheme(), url.path()),
        None => format!("{}://{host}{}", url.scheme(), url.path()),
    }
}

/// [`scrub`] over text that may not parse: query, fragment and userinfo dropped by hand.
pub fn scrub_text(raw: &str) -> String {
    if let Ok(u) = Url::parse(raw) {
        return scrub(&u);
    }
    let cut = raw.find(['?', '#']).map_or(raw, |i| &raw[..i]);
    match (cut.find("://"), cut.rfind('@')) {
        (Some(s), Some(at)) if at > s => format!("{}{}", &cut[..s + 3], &cut[at + 1..]),
        _ => cut.to_string(),
    }
}

/// Whether `host` names the local machine: the name `localhost`, an IPv4 address in
/// 127.0.0.0/8, or `::1`. Every loopback exemption reads this one predicate.
pub fn is_loopback_host(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost") || h.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Whether an address is private, link-local, unique-local, loopback, unspecified,
/// shared or a cloud-metadata address (`connector.attach.private-address`).
pub fn internal_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast() || (o[0] == 100 && (64..128).contains(&o[1]))
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return internal_address(IpAddr::V4(v4));
            }
            let seg = v6.segments();
            v6.is_loopback() || v6.is_unspecified() || (seg[0] & 0xfe00) == 0xfc00 || (seg[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Vet a resolved address for a configured host: an internal address refuses unless the
/// configured host is itself a loopback name or literal.
pub fn vet_address(configured_host: &str, ip: IpAddr) -> Result<(), ConnectorError> {
    if internal_address(ip) && !is_loopback_host(configured_host) {
        return Err(ConnectorError::ConnectorPrivateAddress(format!("`{configured_host}` resolves to internal address {ip}")));
    }
    Ok(())
}

/// Judge a redirect or vendor-supplied next link against the configured origin: the
/// same host and port on the same or a stronger transport (`connector.attach.weakened-hop`).
pub fn check_hop(configured: &Url, next: &Url) -> Result<(), ConnectorError> {
    let same_host = configured.host_str().map(str::to_ascii_lowercase) == next.host_str().map(str::to_ascii_lowercase);
    let same_port = configured.port_or_known_default() == next.port_or_known_default() || (configured.scheme() == "http" && next.scheme() == "https" && configured.port() == next.port());
    let transport_holds = !(configured.scheme() == "https" && next.scheme() != "https");
    if same_host && same_port && transport_holds && matches!(next.scheme(), "http" | "https") {
        Ok(())
    } else {
        Err(ConnectorError::SecretRedirectOffOrigin(format!("a hop from `{}` to `{}` leaves the configured origin", scrub(configured), scrub(next))))
    }
}

/// Refuse a credential-bearing header bound for a cleartext endpoint off loopback
/// (`connector.attach.cleartext-endpoint`).
pub fn check_transport(url: &Url, header: &str) -> Result<(), ConnectorError> {
    if url.scheme() != "https" && !url.host_str().is_some_and(is_loopback_host) {
        return Err(ConnectorError::SecretCleartextEndpoint(format!("header `{header}` carries a credential and `{}` is cleartext", scrub(url))));
    }
    Ok(())
}
