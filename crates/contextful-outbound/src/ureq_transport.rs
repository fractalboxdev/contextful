//! The ureq adapter of the transport port, behind the `transport-ureq` feature.
//! Its resolver answers only the addresses a send hands it, so a connect uses the address
//! the client vetted with no second lookup (`connector.attach.resolve-once`).

use crate::egress::{Inbound, Outbound, Transport, TransportFault};
use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use ureq::config::Config;
use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

/// A resolver answering the addresses pinned for each host and port. A host no send
/// pinned resolves to nothing.
#[derive(Debug, Clone, Default)]
struct Vetted(Arc<Mutex<HashMap<String, Vec<SocketAddr>>>>);

impl Vetted {
    fn key(host: &str, port: u16) -> String {
        format!("{}:{port}", host.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase())
    }

    fn pin(&self, host: &str, port: u16, addrs: Vec<SocketAddr>) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).insert(Vetted::key(host, port), addrs);
    }
}

impl Resolver for Vetted {
    fn resolve(&self, uri: &ureq::http::Uri, _: &Config, _: NextTimeout) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let host = uri.host().unwrap_or_default();
        let port = uri.port_u16().unwrap_or(if uri.scheme_str() == Some("https") { 443 } else { 80 });
        let pinned = self.0.lock().unwrap_or_else(|e| e.into_inner()).get(&Vetted::key(host, port)).cloned().unwrap_or_default();
        if pinned.is_empty() {
            return Err(ureq::Error::HostNotFound);
        }
        let mut v = self.empty();
        for addr in pinned.into_iter().take(16) {
            v.push(addr);
        }
        Ok(v)
    }
}

/// The ureq transport: one agent that bypasses the system proxy and one that keeps it,
/// each reading the proxy environment when the transport is built.
pub struct Ureq {
    vetted: Vetted,
    direct: ureq::Agent,
    proxied: ureq::Agent,
}

impl std::fmt::Debug for Ureq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ureq")
    }
}

impl Default for Ureq {
    fn default() -> Ureq {
        Ureq::new()
    }
}

fn agent(vetted: &Vetted, direct: bool) -> ureq::Agent {
    let mut builder = Config::builder().max_redirects(0).http_status_as_error(false).save_redirect_history(false);
    if direct {
        builder = builder.proxy(None);
    }
    ureq::Agent::with_parts(builder.build(), DefaultConnector::new(), vetted.clone())
}

/// A transport error's text, with any URL it quotes scrubbed.
fn text(e: &ureq::Error) -> String {
    e.to_string()
        .split_whitespace()
        .map(|w| if w.contains("://") { contextful_core::connector::attach::scrub_text(w) } else { w.to_string() })
        .collect::<Vec<_>>()
        .join(" ")
}

impl Ureq {
    pub fn new() -> Ureq {
        let vetted = Vetted::default();
        let (direct, proxied) = (agent(&vetted, true), agent(&vetted, false));
        Ureq { vetted, direct, proxied }
    }
}

impl Transport for Ureq {
    fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        (bare, port).to_socket_addrs().map(Iterator::collect).map_err(|e| e.to_string())
    }

    fn send(&self, request: &Outbound<'_>) -> Result<Inbound, TransportFault> {
        let url = request.url;
        self.vetted.pin(url.host_str().unwrap_or_default(), url.port_or_known_default().unwrap_or(443), request.addrs.to_vec());
        let agent = if request.direct { &self.direct } else { &self.proxied };
        let mut req = ureq::http::Request::builder().method(request.method).uri(url.as_str());
        for (name, v) in request.headers {
            req = req.header(name.as_str(), v.text());
        }
        let built = req.body(request.body.to_vec()).map_err(|e| TransportFault::Failed(format!("building the request: {e}")))?;
        let built = agent.configure_request(built).timeout_global(Some(request.timeout)).build();
        let mut resp = agent.run(built).map_err(|e| TransportFault::Failed(text(&e)))?;
        let status = resp.status().as_u16();
        let headers = resp.headers().iter().map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned())).collect();
        if !request.read_body {
            return Ok(Inbound { status, headers, body: Vec::new() });
        }
        let body = resp.body_mut().with_config().limit(request.max_body).read_to_vec().map_err(|e| match e {
            ureq::Error::BodyExceedsLimit(_) => TransportFault::BodyOverLimit,
            other => TransportFault::Failed(text(&other)),
        })?;
        Ok(Inbound { status, headers, body })
    }
}
