//! The mediated client: the one path every outbound request takes. It judges the request
//! against the declaration ahead of socket I/O, resolves the host once and connects to
//! the address it vetted, attaches credentials, and follows a hop only within the
//! configured origin.

use contextful_core::connector::attach::{check_hop, check_transport, scrub, vet_address, Allowlist};
use contextful_core::connector::reference::Hydrated;
use contextful_core::connector::ConnectorError;
use contextful_core::run::{Failure, FailureTag};
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Mutex;
use std::time::Duration;
use ureq::config::Config;
use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};
use url::Url;

/// Hops one request follows before it fails.
pub const MAX_HOPS: usize = 10;
/// Largest response body a request reads.
pub const MAX_BODY_BYTES: u64 = 256 * 1024 * 1024;
/// Wall clock one request may take.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// A header value: plain text, or material hydrated from a reference.
#[derive(Debug, Clone)]
pub enum HeaderValue {
    Plain(String),
    Sensitive(Hydrated),
}

impl HeaderValue {
    fn text(&self) -> &str {
        match self {
            HeaderValue::Plain(s) => s,
            HeaderValue::Sensitive(h) => h.reveal(),
        }
    }
}

/// A response the vendor answered.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// The URL whose answer this is, after every followed hop.
    pub url: Url,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// A resolver answering one pre-vetted address, so the connect uses the address the
/// check judged, with no second lookup (`connector.attach.resolve-once`).
#[derive(Debug)]
struct Pinned(SocketAddr);

impl Resolver for Pinned {
    fn resolve(&self, _: &ureq::http::Uri, _: &Config, _: NextTimeout) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let mut v = self.empty();
        v.push(self.0);
        Ok(v)
    }
}

fn deny(e: ConnectorError) -> Failure {
    Failure::deterministic(FailureTag::Config, e.to_string())
}

/// Classify a vendor answer that is not a success.
pub fn classify(status: u16, retry_after: Option<u64>, what: &str) -> Failure {
    match status {
        401 | 403 => Failure::new(FailureTag::AuthExpired, format!("{what} answered {status}")),
        429 => {
            let f = Failure::new(FailureTag::RateLimited, format!("{what} answered 429"));
            retry_after.map_or(f.clone(), |s| f.with_retry_after(s))
        }
        500..=599 => Failure::new(FailureTag::Transient, format!("{what} answered {status}")),
        _ => Failure::new(FailureTag::Permanent, format!("{what} answered {status}")),
    }
}

/// The mediated client of one source.
#[derive(Debug)]
pub struct Client {
    allow: Allowlist,
    /// The configured origin every hop keeps.
    origin: Url,
    /// Header names that carried a credential on any request, by name only.
    sensitive: Mutex<Vec<String>>,
}

impl Client {
    pub fn new(allow: Allowlist, origin: Url) -> Client {
        Client { allow, origin, sensitive: Mutex::default() }
    }

    /// Header names that carried a credential (`connector.attach.sensitive-header-record`).
    pub fn sensitive_headers(&self) -> Vec<String> {
        self.sensitive.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// Judge `url` against the declaration and vet the one address its host resolves to.
    fn admit(&self, url: &Url, headers: &[(String, HeaderValue)]) -> Result<SocketAddr, Failure> {
        let host = url.host_str().unwrap_or_default();
        if !self.allow.permits(host) {
            return Err(deny(ConnectorError::SecretUnpermittedRequest(format!("`{}` is not a host the declaration covers", scrub(url)))));
        }
        for (name, v) in headers {
            if matches!(v, HeaderValue::Sensitive(_)) {
                check_transport(url, name).map_err(deny)?;
            }
        }
        let port = url.port_or_known_default().unwrap_or(443);
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        let addrs: Vec<SocketAddr> = (bare, port)
            .to_socket_addrs()
            .map_err(|e| Failure::new(FailureTag::Transient, format!("resolving `{host}`: {e}")))?
            .collect();
        let first = addrs.first().copied().ok_or_else(|| Failure::new(FailureTag::Transient, format!("`{host}` resolves to no address")))?;
        for a in &addrs {
            vet_address(self.origin.host_str().unwrap_or_default(), a.ip()).map_err(deny)?;
        }
        Ok(first)
    }

    /// Send one request, following same-origin hops.
    pub fn send(&self, method: &str, url: &Url, headers: &[(String, HeaderValue)], body: Option<&[u8]>) -> Result<Response, Failure> {
        self.exchange(method, url, headers, body, true)
    }

    /// Send one request and answer whatever comes back, a redirect included, following nothing.
    pub fn send_once(&self, method: &str, url: &Url, headers: &[(String, HeaderValue)], body: Option<&[u8]>) -> Result<Response, Failure> {
        self.exchange(method, url, headers, body, false)
    }

    fn exchange(&self, method: &str, url: &Url, headers: &[(String, HeaderValue)], body: Option<&[u8]>, follow: bool) -> Result<Response, Failure> {
        let mut current = url.clone();
        check_hop(&self.origin, &current).map_err(deny)?;
        for _ in 0..=MAX_HOPS {
            let addr = self.admit(&current, headers)?;
            let mut builder = Config::builder().max_redirects(0).http_status_as_error(false).timeout_global(Some(REQUEST_TIMEOUT)).save_redirect_history(false);
            if !headers.is_empty() {
                // A source declaring any header takes the hardened client, which bypasses the system proxy.
                builder = builder.proxy(None);
            }
            let agent = ureq::Agent::with_parts(builder.build(), DefaultConnector::new(), Pinned(addr));
            let mut req = ureq::http::Request::builder().method(method).uri(current.as_str());
            for (name, v) in headers {
                req = req.header(name.as_str(), v.text());
                if matches!(v, HeaderValue::Sensitive(_)) {
                    if let Ok(mut s) = self.sensitive.lock() {
                        if !s.contains(name) {
                            s.push(name.clone());
                        }
                    }
                }
            }
            let request = req.body(body.map(<[u8]>::to_vec).unwrap_or_default()).map_err(|e| Failure::new(FailureTag::Permanent, format!("building a request to `{}`: {e}", scrub(&current))))?;
            let mut resp = agent.run(request).map_err(|e| Failure::new(FailureTag::Transient, format!("request to `{}` failed: {}", scrub(&current), transport_error(&e))))?;
            let status = resp.status().as_u16();
            let headers_out: Vec<(String, String)> =
                resp.headers().iter().map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or_default().to_string())).collect();
            if follow && (300..400).contains(&status) {
                let location = headers_out.iter().find(|(k, _)| k.eq_ignore_ascii_case("location")).map(|(_, v)| v.clone());
                if let Some(loc) = location {
                    let next = current.join(&loc).map_err(|e| Failure::new(FailureTag::Permanent, format!("a redirect from `{}` names no URL: {e}", scrub(&current))))?;
                    check_hop(&self.origin, &next).map_err(deny)?;
                    current = next;
                    continue;
                }
            }
            let body = resp
                .body_mut()
                .with_config()
                .limit(MAX_BODY_BYTES)
                .read_to_vec()
                .map_err(|e| Failure::new(FailureTag::Transient, format!("reading `{}`: {}", scrub(&current), transport_error(&e))))?;
            return Ok(Response { status, headers: headers_out, body, url: current });
        }
        Err(Failure::new(FailureTag::Permanent, format!("`{}` redirected more than {MAX_HOPS} times", scrub(url))))
    }
}

/// A transport error's text, with any URL it quotes scrubbed.
fn transport_error(e: &ureq::Error) -> String {
    let text = e.to_string();
    text.split_whitespace().map(|w| if w.contains("://") { contextful_core::connector::attach::scrub_text(w) } else { w.to_string() }).collect::<Vec<_>>().join(" ")
}
