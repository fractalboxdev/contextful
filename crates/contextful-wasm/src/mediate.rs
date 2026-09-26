//! The guest's one path to the network: its `wasi:http/outgoing-handler` import. Each
//! request passes the allowlist, then the reservation, then the mediated client of its
//! origin, which vets the address, pins the origin and attaches the host-held headers.
//! The guest names a request and receives a response; no import hands it credential
//! bytes (`connector.attach.no-material-to-a-guest`).
//!
//! Every wait — the reservation, an in-flight slot, the body, the exchange — happens
//! inside the returned future, so the call deadline in `host` bounds it.

use crate::limits::{IN_FLIGHT, REQUEST_BODY_BYTES};
use bytes::Bytes;
use contextful_core::connector::attach::{scrub, Allowlist};
use contextful_core::run::Failure;
use contextful_runtime::client::{Client, HeaderValue, Response};
use http_body_util::{BodyExt, Empty, Full};
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use url::Url;
use wasmtime_wasi_http::{Error, RequestOptions, WasiBody, WasiHttpHooks};

/// A limiter's answer to one reservation (`connector.meter`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reservation {
    /// One permit, spent on the request.
    Granted,
    /// No permit; the request returns to the guest as a synthesized `429`.
    Denied { retry_after_secs: u64 },
    /// The limiter could not answer; the request fails as a transport failure.
    Unreachable(String),
}

/// The reservation point a session draws a permit from ahead of each outbound request.
pub trait Reserve: Send + Sync {
    fn reserve(&self) -> Reservation;
}

/// What the mediation point did during a session, by host and name, never by value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Traffic {
    /// Hosts of requests that went out.
    pub sent: Vec<String>,
    /// Hosts the allowlist refused.
    pub denied: Vec<String>,
    /// Refusals the mediated client raised ahead of the socket.
    pub refused: Vec<String>,
    /// Requests held back because the limiter could not answer.
    pub held_back: Vec<String>,
    /// Requests the limiter denied, each answered with a synthesized `429`.
    pub throttled: u64,
}

type Answer = Result<(http::Response<WasiBody>, Box<dyn Future<Output = Result<(), Error>> + Send>), Error>;

/// The session's hooks into `wasi:http`.
pub(crate) struct Mediator {
    allow: Allowlist,
    attach: Vec<(String, HeaderValue)>,
    gate: Option<Arc<dyn Reserve>>,
    /// One mediated client per origin, so each client pins the origin its requests keep.
    clients: HashMap<String, Arc<Client>>,
    in_flight: Arc<tokio::sync::Semaphore>,
    pub(crate) traffic: Arc<Mutex<Traffic>>,
}

impl Mediator {
    /// A mediator drawing from `in_flight`, the session's slots. An instance replacing a
    /// trapped one shares them, so requests its predecessor abandoned still count.
    pub(crate) fn new(allow: Allowlist, attach: Vec<(String, HeaderValue)>, gate: Option<Arc<dyn Reserve>>, in_flight: Arc<tokio::sync::Semaphore>) -> Mediator {
        Mediator { allow, attach, gate, clients: HashMap::new(), in_flight, traffic: Arc::default() }
    }

    /// The [`IN_FLIGHT`] slots one session's requests share.
    pub(crate) fn slots() -> Arc<tokio::sync::Semaphore> {
        Arc::new(tokio::sync::Semaphore::new(IN_FLIGHT))
    }

    fn note(&self, f: impl FnOnce(&mut Traffic)) {
        f(&mut self.traffic.lock().unwrap_or_else(|e| e.into_inner()));
    }

    fn client(&mut self, url: &Url) -> Arc<Client> {
        let origin = url.origin().ascii_serialization();
        let allow = self.allow.clone();
        self.clients.entry(origin.clone()).or_insert_with(|| Arc::new(Client::new(allow, Url::parse(&origin).unwrap_or_else(|_| url.clone())))).clone()
    }
}

fn answered(answer: Answer) -> Box<dyn Future<Output = Answer> + Send> {
    Box::new(async move { answer })
}

fn done() -> Box<dyn Future<Output = Result<(), Error>> + Send> {
    Box::new(async { Ok(()) })
}

/// The `429` a denied reservation becomes. No connection opens for it.
fn synthesized_429(retry_after_secs: u64) -> http::Response<WasiBody> {
    let mut resp = http::Response::new(Empty::<Bytes>::new().map_err(|e| match e {}).boxed_unsync());
    *resp.status_mut() = http::StatusCode::TOO_MANY_REQUESTS;
    resp.headers_mut().insert(http::header::RETRY_AFTER, http::HeaderValue::from(retry_after_secs));
    resp
}

fn to_http(r: Response) -> http::Response<WasiBody> {
    let mut resp = http::Response::new(Full::new(Bytes::from(r.body)).map_err(|e| match e {}).boxed_unsync());
    *resp.status_mut() = http::StatusCode::from_u16(r.status).unwrap_or(http::StatusCode::BAD_GATEWAY);
    for (k, v) in r.headers {
        // The body arrives whole and decoded; its transfer framing does not travel with it.
        if k.eq_ignore_ascii_case("transfer-encoding") {
            continue;
        }
        if let (Ok(k), Ok(v)) = (http::HeaderName::from_bytes(k.as_bytes()), http::HeaderValue::from_str(&v)) {
            resp.headers_mut().append(k, v);
        }
    }
    resp
}

impl WasiHttpHooks for Mediator {
    fn send_request(
        &mut self,
        request: http::Request<WasiBody>,
        _options: Option<RequestOptions>,
        _fut: Box<dyn Future<Output = Result<(), Error>> + Send>,
    ) -> Box<dyn Future<Output = Answer> + Send> {
        let Ok(url) = Url::parse(&request.uri().to_string()) else {
            return answered(Err(Error::HttpRequestUriInvalid));
        };
        let host = url.host_str().unwrap_or_default().to_string();
        // The allowlist runs first, so a request that never goes out spends no permit.
        if !self.allow.permits(&host) {
            self.note(|t| t.denied.push(host));
            return answered(Err(Error::HttpRequestDenied));
        }
        let gate = self.gate.clone();
        let mut headers: Vec<(String, HeaderValue)> = request
            .headers()
            .iter()
            .filter(|(k, _)| **k != http::header::HOST && !self.attach.iter().any(|(n, _)| n.eq_ignore_ascii_case(k.as_str())))
            .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_string(), HeaderValue::Plain(v.to_string()))))
            .collect();
        headers.extend(self.attach.iter().cloned());
        let client = self.client(&url);
        let method = request.method().as_str().to_string();
        let slots = self.in_flight.clone();
        let traffic = self.traffic.clone();
        Box::new(async move {
            if let Some(gate) = gate {
                let reservation = tokio::task::spawn_blocking(move || gate.reserve()).await.map_err(|e| Error::InternalError(Some(e.to_string())))?;
                match reservation {
                    Reservation::Granted => {}
                    Reservation::Denied { retry_after_secs } => {
                        traffic.lock().unwrap_or_else(|e| e.into_inner()).throttled += 1;
                        return Ok((synthesized_429(retry_after_secs), done()));
                    }
                    Reservation::Unreachable(why) => {
                        traffic.lock().unwrap_or_else(|e| e.into_inner()).held_back.push(why.clone());
                        return Err(Error::InternalError(Some(format!("the limiter could not answer: {why}"))));
                    }
                }
            }
            let slot = slots.acquire_owned().await.map_err(|e| Error::InternalError(Some(e.to_string())))?;
            let Some(body) = bounded_body(request.into_body()).await? else {
                let why = format!("a guest request body to `{}` runs past {REQUEST_BODY_BYTES} bytes", scrub(&url));
                traffic.lock().unwrap_or_else(|e| e.into_inner()).refused.push(why);
                return Err(Error::HttpRequestBodySize(Some(REQUEST_BODY_BYTES as u64)));
            };
            let body = (!body.is_empty()).then_some(body);
            let sent = scrub(&url);
            // The slot travels with the exchange, so a call abandoned at its deadline
            // frees it only once the exchange ends.
            let answer = tokio::task::spawn_blocking(move || {
                let _slot = slot;
                client.send(&method, &url, &headers, body.as_deref())
            })
            .await
            .map_err(|e| Error::InternalError(Some(e.to_string())))?;
            match answer {
                Ok(resp) => {
                    traffic.lock().unwrap_or_else(|e| e.into_inner()).sent.push(host);
                    Ok((to_http(resp), done()))
                }
                Err(failure) => Err(refusal(&traffic, &sent, failure)),
            }
        })
    }
}

/// The request body, read up to [`REQUEST_BODY_BYTES`]; `None` once it runs past.
async fn bounded_body(mut body: WasiBody) -> Result<Option<Vec<u8>>, Error> {
    let mut out = Vec::new();
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame?.into_data() {
            if out.len() + data.len() > REQUEST_BODY_BYTES {
                return Ok(None);
            }
            out.extend_from_slice(&data);
        }
    }
    Ok(Some(out))
}

/// A failed exchange, as the guest sees it. A refusal decided ahead of the socket is
/// recorded, so the call fails even when the guest swallows it.
fn refusal(traffic: &Mutex<Traffic>, url: &str, failure: Failure) -> Error {
    if failure.deterministic {
        traffic.lock().unwrap_or_else(|e| e.into_inner()).refused.push(failure.message.clone());
        return Error::HttpRequestDenied;
    }
    Error::InternalError(Some(format!("request to `{url}` failed: {}", failure.message)))
}
