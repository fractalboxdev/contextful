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
    vet_address_admitting(configured_host, ip, &PrivateRanges::default())
}

/// [`vet_address`] with the operator's admitted private ranges: an address inside one of
/// `ranges` passes (`connector.attach.private-range-admitted`).
pub fn vet_address_admitting(configured_host: &str, ip: IpAddr, ranges: &PrivateRanges) -> Result<(), ConnectorError> {
    if internal_address(ip) && !is_loopback_host(configured_host) && !ranges.contains(ip) {
        return Err(ConnectorError::ConnectorPrivateAddress(format!("`{configured_host}` resolves to internal address {ip}")));
    }
    Ok(())
}

/// The process variable listing admitted private ranges, comma-separated CIDR entries.
pub const PRIVATE_RANGES_VAR: &str = "CONTEXTFUL_EGRESS_PRIVATE_RANGES";

/// One CIDR range, an IPv4-mapped IPv6 range held in its IPv4 form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cidr {
    V4(u32, u8),
    V6(u128, u8),
}

impl Cidr {
    /// Parse `a.b.c.d/n`, `x::y/n` or a bare address as a single-host range. Host bits set
    /// past the prefix refuse, so an entry states exactly the range it admits.
    fn parse(entry: &str) -> Option<Cidr> {
        let (addr, prefix) = match entry.split_once('/') {
            Some((a, p)) => (a.parse::<IpAddr>().ok()?, Some(p.parse::<u8>().ok()?)),
            None => (entry.parse::<IpAddr>().ok()?, None),
        };
        let cidr = match addr {
            IpAddr::V4(v4) => Cidr::V4(u32::from(v4), prefix.unwrap_or(32)),
            IpAddr::V6(v6) => Cidr::V6(u128::from(v6), prefix.unwrap_or(128)),
        };
        let canonical = cidr.canonical();
        let fits = match canonical {
            Cidr::V4(n, p) => p <= 32 && n & !mask4(p) == 0,
            Cidr::V6(n, p) => p <= 128 && n & !mask6(p) == 0,
        };
        fits.then_some(canonical)
    }

    /// A range inside `::ffff:0:0/96` in its IPv4 form.
    fn canonical(self) -> Cidr {
        match self {
            Cidr::V6(n, p) if (96..=128).contains(&p) && n >> 32 == 0xffff => Cidr::V4(n as u32, p - 96),
            other => other,
        }
    }

    fn contains(self, ip: IpAddr) -> bool {
        match (self, canonical_ip(ip)) {
            (Cidr::V4(n, p), IpAddr::V4(a)) => u32::from(a) & mask4(p) == n,
            (Cidr::V6(n, p), IpAddr::V6(a)) => u128::from(a) & mask6(p) == n,
            _ => false,
        }
    }

    /// Whether two ranges share an address: the shorter prefix covers the other's network.
    fn overlaps(self, other: Cidr) -> bool {
        match (self, other) {
            (Cidr::V4(a, p), Cidr::V4(b, q)) => {
                let m = mask4(p.min(q));
                a & m == b & m
            }
            (Cidr::V6(a, p), Cidr::V6(b, q)) => {
                let m = mask6(p.min(q));
                a & m == b & m
            }
            _ => false,
        }
    }
}

fn mask4(prefix: u8) -> u32 {
    if prefix == 0 { 0 } else { u32::MAX << (32 - u32::from(prefix.min(32))) }
}

fn mask6(prefix: u8) -> u128 {
    if prefix == 0 { 0 } else { u128::MAX << (128 - u32::from(prefix.min(128))) }
}

fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

/// The ranges no admitted range may touch, with the name a refusal gives each: loopback,
/// the unspecified address that connects to the local host, link-local and the metadata address.
const UNADMITTABLE: [(&str, &str); 7] = [
    ("127.0.0.0/8", "loopback"),
    ("0.0.0.0/32", "the unspecified address"),
    ("169.254.0.0/16", "link-local"),
    ("::1/128", "loopback"),
    ("::/128", "the unspecified address"),
    ("fe80::/10", "link-local"),
    ("fd00:ec2::254/128", "the metadata address"),
];

/// The operator's admitted private ranges (`connector.attach.private-range-admitted`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrivateRanges(Vec<Cidr>);

impl PrivateRanges {
    /// Parse a comma-separated list of CIDR entries; blank entries are skipped. An entry that
    /// is no CIDR range, or one overlapping loopback, link-local or the metadata address,
    /// raises `ConnectorPrivateRangeRejected` naming it (`connector.attach.private-range-rejected`).
    pub fn parse(list: &str) -> Result<PrivateRanges, ConnectorError> {
        let mut out = Vec::new();
        for entry in list.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let reject = |why: String| ConnectorError::ConnectorPrivateRangeRejected(format!("`{entry}` in {PRIVATE_RANGES_VAR} {why}"));
            let cidr = Cidr::parse(entry).ok_or_else(|| reject("is not a CIDR range".into()))?;
            for (raw, name) in UNADMITTABLE {
                let forbidden = Cidr::parse(raw).expect("a fixed range parses");
                if cidr.overlaps(forbidden) {
                    return Err(reject(format!("overlaps {name} {raw}")));
                }
            }
            out.push(cidr);
        }
        Ok(PrivateRanges(out))
    }

    /// Whether no range is admitted.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether `ip`, an IPv4-mapped address in its IPv4 form, lies inside an admitted range.
    pub fn contains(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|c| c.contains(ip))
    }
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
