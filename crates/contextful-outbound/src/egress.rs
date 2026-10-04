//! The two ports every outbound hop passes. The transport port carries the network: its
//! resolve half answers a host's addresses and its send half delivers one request to an
//! address the mediated client vetted, following no redirect. The pre-send hook sees each
//! hop's intent after the allowlist and before resolution, and each admitted hop's
//! outcome; the limiter reservation is its innermost stage, behind any operator hook.

use crate::client::HeaderValue;
use contextful_core::run::Failure;
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

/// One request the send half delivers.
pub struct Outbound<'a> {
    pub method: &'a str,
    /// The full request URL; the transport connects to [`Outbound::addrs`], never to a
    /// fresh lookup of its host.
    pub url: &'a Url,
    /// The addresses the client vetted for the URL's host and port.
    pub addrs: &'a [SocketAddr],
    pub headers: &'a [(String, HeaderValue)],
    pub body: &'a [u8],
    /// Whether the request bypasses the system proxy (`connector.attach.header-selects-client`).
    pub direct: bool,
    /// Wall clock the whole exchange may take.
    pub timeout: Duration,
    /// Largest response body the transport reads.
    pub max_body: u64,
    /// Whether the transport reads the response body at all.
    pub read_body: bool,
}

/// The answer to one delivered request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inbound {
    pub status: u16,
    /// Every header line in arrival order.
    pub headers: Vec<(String, String)>,
    /// The body the transport read: empty when the request read none.
    pub body: Vec<u8>,
}

/// Why a send failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportFault {
    /// The response body runs past [`Outbound::max_body`].
    BodyOverLimit,
    /// Any other failure, its text carrying no query, fragment or userinfo.
    Failed(String),
}

/// The network port of the mediated client (`connector.attach.transport-port`).
pub trait Transport: Send + Sync {
    /// Every address `host` answers at `port` (`connector.attach.resolve-half`).
    fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, String>;
    /// Deliver one request to one of its vetted addresses, following no redirect.
    fn send(&self, request: &Outbound<'_>) -> Result<Inbound, TransportFault>;
}

/// One hop's intent, as the pre-send hook sees it (`connector.meter.pre-send-hook`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    pub method: String,
    /// The hop's URL scrubbed of query, fragment and userinfo.
    pub url: String,
    pub host: String,
    pub port: u16,
    /// The traffic class a limiter declaration names, when the client is metered.
    pub class: Option<String>,
    pub run_id: Option<String>,
    /// Request body bytes.
    pub body_bytes: u64,
}

/// What one admitted hop came to (`connector.meter.hook-settle`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// The status the vendor answered; `None` when the hop failed first.
    pub status: Option<u16>,
    /// The failure that ended the hop, scrubbed.
    pub failure: Option<String>,
    /// Request body bytes sent.
    pub sent: u64,
    /// Response body bytes the host read.
    pub received: u64,
}

/// An operator's pre-send hook. It composes in front of the limiter reservation, so a
/// hop it refuses makes no limiter call (`connector.meter.hook-composition`).
pub trait PreSendHook: Send + Sync {
    /// Admit the hop, or refuse it with a reason; a refusal raises
    /// `ConnectorEgressRefused` and resolves no name.
    fn admit(&self, intent: &Intent) -> Result<(), String>;
    /// The outcome of a hop [`PreSendHook::admit`] admitted.
    fn settle(&self, intent: &Intent, outcome: &Outcome);
    /// A source settles the scope with its batch ordinal, or none when no batch follows.
    fn finish(&self, _batch_seq: Option<i32>) -> Result<(), Failure> {
        Ok(())
    }
}

/// The innermost stage of the pre-send hook: a reservation against a shared quota. A hop
/// it does not grant fails with the failure it answers.
pub trait Reserve: Send + Sync {
    fn reserve(&self, intent: &Intent) -> Result<(), Failure>;
    /// One vendor response's status and headers, for the quota's usage report.
    fn observe(&self, _status: u16, _headers: &[(String, String)]) {}
}

/// A transport answering every lookup with a refusal: the port a build without an HTTP
/// adapter starts from until the embedder supplies its own.
#[derive(Debug, Default, Clone, Copy)]
pub struct Unlinked;

impl Transport for Unlinked {
    fn resolve(&self, host: &str, _port: u16) -> Result<Vec<SocketAddr>, String> {
        Err(format!("no transport is linked to reach `{host}`; build with `transport-ureq` or supply one"))
    }

    fn send(&self, request: &Outbound<'_>) -> Result<Inbound, TransportFault> {
        Err(TransportFault::Failed(format!("no transport is linked to reach `{}`", request.url.host_str().unwrap_or_default())))
    }
}

/// The transport a client takes unless given another: the ureq adapter under the
/// `transport-ureq` feature, [`Unlinked`] without it.
pub fn system() -> Arc<dyn Transport> {
    #[cfg(feature = "transport-ureq")]
    {
        Arc::new(crate::ureq_transport::Ureq::new())
    }
    #[cfg(not(feature = "transport-ureq"))]
    {
        Arc::new(Unlinked)
    }
}

impl fmt::Debug for dyn Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Transport")
    }
}

impl fmt::Debug for dyn PreSendHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PreSendHook")
    }
}

impl fmt::Debug for dyn Reserve {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Reserve")
    }
}
