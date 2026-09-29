//! The mediated client: the one path every outbound request takes. Each hop passes the
//! allowlist, then the pre-send hook carrying its intent (an operator hook in front of
//! the limiter reservation), then name resolution through the transport port; the client
//! vets every address it answers, attaches credentials, sends through the port to a
//! vetted address, and follows a hop only within the configured origin.

use crate::egress::{self, Inbound, Intent, Outbound, Outcome, PreSendHook, Reserve, Transport, TransportFault};
use crate::limiter::Meter;
use contextful_core::connector::attach::{check_hop, check_transport, scrub, vet_address, Allowlist};
use contextful_core::connector::reference::Hydrated;
use contextful_core::connector::ConnectorError;
use contextful_core::run::{Failure, FailureTag};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use url::Url;

/// Hops one request follows before it fails.
pub const MAX_HOPS: usize = 10;
/// Largest response body a request reads.
pub const MAX_BODY_BYTES: u64 = 256 * 1024 * 1024;
/// Wall clock one request may take.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The phrase a failure carries when a response body passes the client's ceiling.
const OVER_LIMIT: &str = "answered a body over";

/// Whether `f` is a response body passing the client's ceiling: deterministic, and past
/// it on every retry.
pub fn is_over_limit(f: &Failure) -> bool {
    f.deterministic && f.message.contains(OVER_LIMIT)
}

/// A header value: plain text, or material hydrated from a reference.
#[derive(Debug, Clone)]
pub enum HeaderValue {
    Plain(String),
    Sensitive(Hydrated),
}

impl HeaderValue {
    /// The value as it goes on the wire; only a transport reads it.
    pub fn text(&self) -> &str {
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
    /// Every header line in arrival order. A value holding bytes outside visible ASCII
    /// decodes lossily, so it never reads as empty.
    pub headers: Vec<(String, String)>,
    /// The body, empty from a client built [`Client::without_body`].
    pub body: Vec<u8>,
    /// The URL whose answer this is, after every followed hop.
    pub url: Url,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
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
    /// The network port every lookup and every send takes.
    transport: Arc<dyn Transport>,
    /// Wall clock one request may take.
    timeout: Duration,
    /// The largest response body a request reads.
    max_body: u64,
    /// Whether a request reads the response body at all.
    read_body: bool,
    /// The operator hook every hop passes ahead of the reservation.
    hook: Option<Arc<dyn PreSendHook>>,
    /// The reservation every hop takes, when the connector declares a limiter.
    reserve: Option<Arc<dyn Reserve>>,
    /// The traffic class and run id each intent carries.
    class: Option<String>,
    run_id: Option<String>,
    /// Headers the client itself frames every request with. They are no declaration, so
    /// they never select the hardened client (`connector.attach.header-selects-client`).
    framing: Vec<(String, HeaderValue)>,
    /// Whether a permitted host may resolve to an internal address: the operator's own
    /// limiter only (`connector.meter.limiter-address`).
    internal: bool,
}

/// A hop the hook admitted, and what it came to.
enum Hop {
    Answered(Inbound),
    Failed(Failure),
}

impl Client {
    /// A client over the [`egress::system`] transport.
    pub fn new(allow: Allowlist, origin: Url) -> Client {
        Client {
            allow,
            origin,
            sensitive: Mutex::default(),
            transport: egress::system(),
            timeout: REQUEST_TIMEOUT,
            max_body: MAX_BODY_BYTES,
            read_body: true,
            hook: None,
            reserve: None,
            class: None,
            run_id: None,
            framing: Vec::new(),
            internal: false,
        }
    }

    /// The client reaching the network through `transport` (`connector.attach.transport-port`).
    pub fn with_transport(mut self, transport: Arc<dyn Transport>) -> Client {
        self.transport = transport;
        self
    }

    /// The client passing every hop through the operator's `hook`, in front of any
    /// reservation (`connector.meter.hook-composition`).
    pub fn with_hook(mut self, hook: Arc<dyn PreSendHook>) -> Client {
        self.hook = Some(hook);
        self
    }

    /// The client whose intents carry `run_id`.
    pub fn for_run(mut self, run_id: &str) -> Client {
        self.run_id = Some(run_id.to_string());
        self
    }

    /// The client whose intents carry the traffic class `class`.
    pub fn in_class(mut self, class: &str) -> Client {
        self.class = Some(class.to_string());
        self
    }

    /// The client framing every request with the plain header `name: value`. A request
    /// declaring the same name sends its own value in its place.
    pub(crate) fn framed(mut self, name: &str, value: &str) -> Client {
        self.framing.push((name.to_string(), HeaderValue::Plain(value.to_string())));
        self
    }

    /// The client with a shorter wall clock per request than [`REQUEST_TIMEOUT`].
    pub fn with_timeout(mut self, timeout: Duration) -> Client {
        self.timeout = timeout.min(REQUEST_TIMEOUT);
        self
    }

    /// The client with the wall clock of the one inference endpoint.
    pub(crate) fn with_completion_timeout(mut self, timeout: Duration) -> Client {
        self.timeout = timeout;
        self
    }

    /// The client admitting an internal address for its permitted hosts. Only the
    /// operator-authored limiter endpoint takes it; a vendor host never does.
    pub(crate) fn admitting_internal(mut self) -> Client {
        self.internal = true;
        self
    }

    /// The client with a lower body ceiling than [`MAX_BODY_BYTES`].
    pub fn with_body_limit(mut self, bytes: u64) -> Client {
        self.max_body = bytes.min(MAX_BODY_BYTES);
        self
    }

    /// The client reading no response body: an answer carries its status and headers,
    /// and its body is dropped unread.
    pub fn without_body(mut self) -> Client {
        self.read_body = false;
        self
    }

    /// The client reserving every request against `meter` (`connector.meter.reservation-point`).
    /// Its intents carry the declared traffic class and, unless set, the limiter's run id.
    pub fn metered(mut self, meter: Meter) -> Client {
        self.class = Some(meter.declaration.class.clone());
        if self.run_id.is_none() {
            self.run_id = meter.run_id();
        }
        self.reserve = Some(Arc::new(meter));
        self
    }

    /// The client taking every hop's reservation from `reserve`.
    pub fn reserving(mut self, reserve: Arc<dyn Reserve>) -> Client {
        self.reserve = Some(reserve);
        self
    }

    /// Header names that carried a credential (`connector.attach.sensitive-header-record`).
    pub fn sensitive_headers(&self) -> Vec<String> {
        self.sensitive.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// Judge `url` against the declaration, ahead of any hook or lookup.
    fn permit(&self, url: &Url, headers: &[(String, HeaderValue)]) -> Result<(), Failure> {
        let host = url.host_str().unwrap_or_default();
        if !self.allow.permits(host) {
            return Err(deny(ConnectorError::SecretUnpermittedRequest(format!("`{}` is not a host the declaration covers", scrub(url)))));
        }
        for (name, v) in headers {
            if matches!(v, HeaderValue::Sensitive(_)) {
                check_transport(url, name).map_err(deny)?;
            }
        }
        Ok(())
    }

    /// The pre-send hook: the operator hook, then the reservation
    /// (`connector.meter.pre-send-hook`). Answers whether the operator hook admitted.
    fn admit(&self, intent: &Intent) -> Result<(), (Failure, bool)> {
        if let Some(hook) = &self.hook {
            hook.admit(intent).map_err(|why| {
                let refused = ConnectorError::ConnectorEgressRefused(format!("`{}`: {why}", intent.host));
                (Failure::deterministic(FailureTag::Config, refused.to_string()), false)
            })?;
        }
        if let Some(reserve) = &self.reserve {
            reserve.reserve(intent).map_err(|f| (f, true))?;
        }
        Ok(())
    }

    /// Resolve `url`'s host through the transport and vet every address it answers.
    fn resolve(&self, url: &Url) -> Result<Vec<SocketAddr>, Failure> {
        let host = url.host_str().unwrap_or_default();
        let port = url.port_or_known_default().unwrap_or(443);
        let addrs = self.transport.resolve(host, port).map_err(|e| Failure::new(FailureTag::Transient, format!("resolving `{host}`: {e}")))?;
        if addrs.is_empty() {
            return Err(Failure::new(FailureTag::Transient, format!("`{host}` resolves to no address")));
        }
        if !self.internal {
            for a in &addrs {
                vet_address(self.origin.host_str().unwrap_or_default(), a.ip()).map_err(deny)?;
            }
        }
        Ok(addrs)
    }

    /// Send one request, following same-origin hops.
    pub fn send(&self, method: &str, url: &Url, headers: &[(String, HeaderValue)], body: Option<&[u8]>) -> Result<Response, Failure> {
        self.exchange(method, url, headers, body, true)
    }

    /// Send one request and answer whatever comes back, a redirect included, following nothing.
    pub fn send_once(&self, method: &str, url: &Url, headers: &[(String, HeaderValue)], body: Option<&[u8]>) -> Result<Response, Failure> {
        self.exchange(method, url, headers, body, false)
    }

    fn intent(&self, method: &str, url: &Url, body: &[u8]) -> Intent {
        Intent {
            method: method.to_string(),
            url: scrub(url),
            host: url.host_str().unwrap_or_default().to_string(),
            port: url.port_or_known_default().unwrap_or(443),
            class: self.class.clone(),
            run_id: self.run_id.clone(),
            body_bytes: body.len() as u64,
        }
    }

    /// One hop the hook admitted: resolve, vet, send.
    fn hop(&self, method: &str, url: &Url, headers: &[(String, HeaderValue)], body: &[u8]) -> Hop {
        let addrs = match self.resolve(url) {
            Ok(a) => a,
            Err(f) => return Hop::Failed(f),
        };
        for (name, v) in headers {
            if matches!(v, HeaderValue::Sensitive(_)) {
                if let Ok(mut s) = self.sensitive.lock() {
                    if !s.contains(name) {
                        s.push(name.clone());
                    }
                }
            }
        }
        let framed: Vec<(String, HeaderValue)>;
        let wire = if self.framing.is_empty() {
            headers
        } else {
            let own = self.framing.iter().filter(|(n, _)| !headers.iter().any(|(d, _)| d.eq_ignore_ascii_case(n)));
            framed = own.chain(headers).cloned().collect();
            &framed
        };
        let outbound = Outbound {
            method,
            url,
            addrs: &addrs,
            headers: wire,
            body,
            // A request carrying a declared header takes the hardened client, which bypasses the system proxy.
            direct: !headers.is_empty(),
            timeout: self.timeout,
            max_body: self.max_body,
            read_body: self.read_body,
        };
        match self.transport.send(&outbound) {
            Ok(inbound) => Hop::Answered(inbound),
            // A body past the ceiling is past it on every retry.
            Err(TransportFault::BodyOverLimit) => {
                Hop::Failed(Failure::deterministic(FailureTag::Permanent, format!("`{}` {OVER_LIMIT} {} bytes", scrub(url), self.max_body)))
            }
            Err(TransportFault::Failed(why)) => Hop::Failed(Failure::new(FailureTag::Transient, format!("request to `{}` failed: {why}", scrub(url)))),
        }
    }

    fn settle(&self, intent: &Intent, outcome: &Outcome) {
        if let Some(hook) = &self.hook {
            hook.settle(intent, outcome);
        }
    }

    fn exchange(&self, method: &str, url: &Url, headers: &[(String, HeaderValue)], body: Option<&[u8]>, follow: bool) -> Result<Response, Failure> {
        let mut current = url.clone();
        let body = body.unwrap_or_default();
        check_hop(&self.origin, &current).map_err(deny)?;
        for _ in 0..=MAX_HOPS {
            // A request the allowlist refuses never reaches the hook or the limiter
            // (`connector.meter.allowlist-precedence`).
            self.permit(&current, headers)?;
            let intent = self.intent(method, &current, body);
            if let Err((f, operator_admitted)) = self.admit(&intent) {
                if operator_admitted {
                    self.settle(&intent, &Outcome { failure: Some(f.message.clone()), ..Outcome::default() });
                }
                return Err(f);
            }
            let inbound = match self.hop(method, &current, headers, body) {
                Hop::Answered(inbound) => inbound,
                Hop::Failed(f) => {
                    self.settle(&intent, &Outcome { failure: Some(f.message.clone()), sent: 0, ..Outcome::default() });
                    return Err(f);
                }
            };
            let Inbound { status, headers: headers_out, body: read } = inbound;
            self.settle(&intent, &Outcome { status: Some(status), failure: None, sent: body.len() as u64, received: read.len() as u64 });
            if let Some(reserve) = &self.reserve {
                reserve.observe(status, &headers_out);
            }
            if follow && (300..400).contains(&status) {
                let location = headers_out.iter().find(|(k, _)| k.eq_ignore_ascii_case("location")).map(|(_, v)| v.clone());
                if let Some(loc) = location {
                    let next = current.join(&loc).map_err(|e| Failure::new(FailureTag::Permanent, format!("a redirect from `{}` names no URL: {e}", scrub(&current))))?;
                    check_hop(&self.origin, &next).map_err(deny)?;
                    current = next;
                    continue;
                }
            }
            return Ok(Response { status, headers: headers_out, body: read, url: current });
        }
        Err(Failure::new(FailureTag::Permanent, format!("`{}` redirected more than {MAX_HOPS} times", scrub(url))))
    }
}
