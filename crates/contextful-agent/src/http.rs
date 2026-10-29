//! The network transport: MCP Streamable HTTP at `POST /mcp`, each request admitted on
//! its own credential (`read.register`).
//!
//! A request carries one JSON-RPC message and its credential. A credential binding a
//! holder key arrives as `Authorization: DPoP <credential>` with a `DPoP` proof header that
//! key signs (`authority.verify.possession-binding`); one binding no key arrives as
//! `Authorization: Bearer <credential>` and admits when it names the face's audience and
//! lives at most an hour (`authority.verify.network-bearer`). Admission, the
//! revocation read and the proof check run per request, so one listener
//! serves many credentials; the message is answered by the same [`Tools`] the stdio
//! transport runs, audit entry included, as `application/json` (`read.respond.one-projection`). The face holds
//! no protocol session, opens no server stream and serves no raw statement
//! (`read.guard.statement-provenance`).

use crate::mcp::{Caller, Carrier, ReadRecord, ReadSpans, Tools};
use contextful_context::read::Face;
use contextful_core::grant::{Action, TablePattern};
use contextful_core::ports::Clock;
use contextful_core::read::{ReadError, Refusal};
use contextful_core::trace::{TRACEPARENT, TRACESTATE};
use contextful_core::AuthorityError;
use contextful_policy::enforce::refuse::payload;
use contextful_policy::keyset::KeyCheckpoint;
use contextful_policy::possession::{verify_body_digest, ProofRefusal, ProofRequest};
use contextful_policy::revoke::RevocationState;
use contextful_policy::verify::{effect_boundary, verify_network, Admission, AdmittedAuthority};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

/// The `Retry-After` a request shed past the in-flight ceiling carries, in seconds
/// (`read.register.past-ceiling`).
pub const RETRY_AFTER_SECS: u64 = 1;

/// Longest a stopping server waits for the connections it accepted before the stop
/// signal (`read.register.stop-drain`).
pub const DRAIN_BOUND: Duration = Duration::from_secs(20);

/// How often a drain checks whether its accepted connections are answered.
const DRAIN_POLL: Duration = Duration::from_millis(20);

/// Largest request body the face reads (`read.register.request-body`).
pub const REQUEST_BODY_BYTES: usize = 1024 * 1024;

/// The one protocol endpoint (`read.register.network-transport`).
pub const MCP_PATH: &str = "/mcp";

/// The readiness route (`read.register.health`).
pub const HEALTH_PATH: &str = "/health";

/// The store-published workflow state and validated control claim routes.
pub const WORKFLOWS_PATH: &str = "/control/workflows";
pub const RECORD_PATH: &str = "/control/record";
pub const EDIT_PATH: &str = "/control/edit";
pub const APPLY_PATH: &str = "/control/apply";

/// The run routes: `POST /runs/:run_id/stop` and the run-stream socket
/// `GET /runs/:run_id/stream` (`run.cancel.stop-route`, `run.project.unauthenticated-upgrade`).
const RUNS_PREFIX: &str = "/runs/";
const STOP_SUFFIX: &str = "/stop";
const STREAM_SUFFIX: &str = "/stream";

/// The receive route, `POST /receive/:pipeline`, and the header naming the batch key
/// (`run.land.receive-endpoint`).
pub const RECEIVE_PREFIX: &str = "/receive/";
pub const BATCH_KEY_HEADER: &str = "Contextful-Batch-Key";

/// Reads a receive route's body once, given the bound the pipeline declares: `Ok(None)`
/// when the announced length passes the bound, with no body byte read, and an answer when
/// the body does not arrive whole or its possession proof does not cover it.
pub type BodyReader<'r> = dyn FnMut(u64) -> Result<Option<Vec<u8>>, HttpResponse> + 'r;

/// The receive route's handler: the request head, the authority admitted on it before any
/// body byte is read, the pipeline id and the body reader.
pub type ReceiveHandler<'h> = dyn Fn(&HttpRequest, &AdmittedAuthority, &str, &mut BodyReader<'_>) -> HttpResponse + Sync + 'h;

/// The outcome route's handler, `GET /receive/:pipeline/:key`: the request, the authority
/// admitted on it, the pipeline id and the percent-decoded batch key
/// (`run.land.receive-queued-outcome`).
pub type OutcomeHandler<'h> = dyn Fn(&HttpRequest, &AdmittedAuthority, &str, &str) -> HttpResponse + Sync + 'h;

/// The header asking for the queued answer, and the preference it carries
/// (`run.land.receive-queued-ask`).
pub const PREFER_HEADER: &str = "Prefer";
pub const RESPOND_ASYNC: &str = "respond-async";

/// The key a websocket handshake appends before hashing (RFC 6455, section 1.3).
const WEBSOCKET_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Largest request head the face reads; a head past it answers `431`.
const REQUEST_HEAD_BYTES: usize = 64 * 1024;

/// How long a connection may take to deliver its request before it is closed.
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the shedding thread spends on one connection past the ceiling: writing its
/// `503`, then discarding what the caller sends until it closes.
const SHED_LINGER: Duration = Duration::from_millis(250);

/// Connections past the ceiling waiting for the shedding thread; one arriving with the
/// queue full is closed unanswered.
const SHED_QUEUE: usize = 64;

/// Headers one request head may carry.
const REQUEST_HEADERS: usize = 64;

/// The challenges a `401` carries: the two presentations the face admits.
const CHALLENGE: &str = "DPoP realm=\"contextful\", algs=\"EdDSA\", Bearer realm=\"contextful\"";

/// JSON-RPC error codes the transport answers before a message reaches the tools.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;

/// The revocation state a request is admitted against, read afresh per request
/// (`read.register.per-request-revocation`); an error names why it could not be read.
pub type Revocation<'a> = dyn Fn() -> Result<RevocationState, String> + Sync + 'a;

/// One request as the listener received it.
#[derive(Debug, Clone, Default)]
pub struct HttpRequest {
    pub method: String,
    /// Path and query, as sent; the target a possession proof covers.
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpRequest {
    /// The first header of `name`, compared case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or_default()
    }

    /// Each `name=value` pair of the target's query, undecoded.
    fn query_pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        let query = self.target.split_once('?').map_or("", |(_, q)| q);
        query.split('&').filter(|p| !p.is_empty()).map(|p| p.split_once('=').unwrap_or((p, "")))
    }

    /// The credential of an `Authorization: DPoP` or `Authorization: Bearer` header, the
    /// scheme compared case-insensitively. Either scheme reaches admission, where the
    /// credential's confirmation claim, not the scheme, decides whether a proof is required.
    pub fn credential(&self) -> Option<&str> {
        let (scheme, credential) = self.header("Authorization")?.trim().split_once(' ')?;
        let admitted = scheme.eq_ignore_ascii_case("Bearer") || scheme.eq_ignore_ascii_case("DPoP");
        Some(credential.trim()).filter(|c| admitted && !c.is_empty())
    }

    /// Whether a `Prefer` header carries `respond-async`, the token compared
    /// case-insensitively among comma-separated preferences (`run.land.receive-queued-ask`).
    pub fn prefers_async(&self) -> bool {
        self.header(PREFER_HEADER).is_some_and(|v| v.split(',').any(|p| p.split(';').next().unwrap_or_default().trim().eq_ignore_ascii_case(RESPOND_ASYNC)))
    }
}

/// One response: status, headers and body.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn json(status: u16, body: &Value) -> HttpResponse {
        HttpResponse { status, headers: vec![("Content-Type".into(), "application/json".into())], body: body.to_string().into_bytes() }
    }

    fn empty(status: u16) -> HttpResponse {
        HttpResponse { status, headers: Vec::new(), body: Vec::new() }
    }

    fn with(mut self, name: &str, value: &str) -> HttpResponse {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// `101`: a websocket upgrade answering `key`, then `text` as one unmasked text frame and
    /// a normal close frame.
    fn upgraded(key: &str, text: &[u8]) -> HttpResponse {
        let accept = base64(&sha1(format!("{key}{WEBSOCKET_GUID}").as_bytes()));
        let mut frames = vec![0x81];
        match text.len() {
            n if n < 126 => frames.push(n as u8),
            n if n <= usize::from(u16::MAX) => {
                frames.push(126);
                frames.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                frames.push(127);
                frames.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        frames.extend_from_slice(text);
        frames.extend_from_slice(&[0x88, 0x02, 0x03, 0xE8]);
        HttpResponse {
            status: 101,
            headers: vec![("Upgrade".into(), "websocket".into()), ("Connection".into(), "Upgrade".into()), ("Sec-WebSocket-Accept".into(), accept)],
            body: frames,
        }
    }

    /// A transport-level answer carrying a message and no refusal identifier.
    fn message(status: u16, message: impl Into<String>) -> HttpResponse {
        HttpResponse::json(status, &json!({ "error": { "http": status, "message": message.into() } }))
    }

    /// A JSON-RPC error the transport answers before any tool runs.
    fn rpc_error(code: i64, message: impl Into<String>) -> HttpResponse {
        HttpResponse::json(400, &json!({ "jsonrpc": "2.0", "id": null, "error": { "code": code, "message": message.into() } }))
    }

    /// `503` with the retry hint.
    fn unavailable(message: impl Into<String>) -> HttpResponse {
        HttpResponse::message(503, message).with("Retry-After", &RETRY_AFTER_SECS.to_string())
    }

    /// The header named `name`, compared case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    fn write_to(&self, out: &mut impl Write) -> std::io::Result<()> {
        let mut head = format!("HTTP/1.1 {} {}\r\n", self.status, reason(self.status));
        for (k, v) in &self.headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        if self.status == 101 {
            head.push_str("\r\n");
        } else {
            head.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n", self.body.len()));
        }
        out.write_all(head.as_bytes())?;
        out.write_all(&self.body)?;
        out.flush()
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        101 => "Switching Protocols",
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        403 => "Forbidden",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Content Too Large",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "",
    }
}

/// An admission refusal: `401`, its identifier and message, and the challenge
/// (`read.register.admission-refused`).
fn unadmitted(e: &AuthorityError) -> HttpResponse {
    let text = e.to_string();
    let identifier = text.split_once(':').map_or(text.as_str(), |(id, _)| id).trim().to_string();
    HttpResponse::json(401, &json!({ "error": { "http": 401, "identifier": identifier, "message": text } }))
        .with("WWW-Authenticate", CHALLENGE)
}

/// Where the face holds its verification keys, audience and revocation source.
pub struct Admitting<'a, C> {
    /// Pinned issuer keys, the proof clock and the nonce cache.
    pub checkpoint: &'a KeyCheckpoint<C>,
    /// The audience every credential names (`read.register.serve-declaration`).
    pub audience: &'a str,
    /// The revocation state, read per request.
    pub revocation: &'a Revocation<'a>,
}

/// A retrieval container's snapshot-set hydration as its face sees it: warming until the
/// hydrator marks it complete. The face reports ready throughout
/// (`topology.publish-hostname.container-readiness`) and answers every route but
/// `GET /health` with `503` and `Retry-After` while warming (`read.cache.warming-read`).
#[derive(Debug, Default)]
pub struct Hydration {
    complete: AtomicBool,
}

impl Hydration {
    /// Hydration under way: the snapshot set is not yet local.
    pub fn warming() -> Hydration {
        Hydration { complete: AtomicBool::new(false) }
    }

    /// Mark the snapshot set local; reads answer from it from now on.
    pub fn finish(&self) {
        self.complete.store(true, Ordering::SeqCst);
    }

    /// Whether the snapshot set is local.
    pub fn is_complete(&self) -> bool {
        self.complete.load(Ordering::SeqCst)
    }

    fn state(&self) -> &'static str {
        if self.is_complete() { "complete" } else { "warming" }
    }
}

/// The network transport over one read face.
pub struct HttpFace<'a, C> {
    tools: Tools<'a>,
    admitting: Admitting<'a, C>,
    control: Option<&'a (dyn Fn(&HttpRequest, &AdmittedAuthority) -> HttpResponse + Sync)>,
    connections: Option<&'a (dyn Fn(&HttpRequest, &AdmittedAuthority) -> HttpResponse + Sync)>,
    ceiling: usize,
    in_flight: AtomicUsize,
    draining: AtomicBool,
    exchange: Option<&'a (dyn Fn(&HttpRequest) -> HttpResponse + Sync)>,
    exchange_unconfigured: bool,
    claim_write: Option<&'a (dyn Fn(&HttpRequest, &AdmittedAuthority, &dyn Fn() -> Result<(), AuthorityError>) -> HttpResponse + Sync)>,
    runs: Option<RunRoutes<'a>>,
    awake: Option<&'a (dyn Fn(&HttpRequest, &AdmittedAuthority, &str) -> HttpResponse + Sync)>,
    receive: Option<&'a ReceiveHandler<'a>>,
    receive_outcome: Option<&'a OutcomeHandler<'a>>,
    hydration: Option<&'a Hydration>,
}

/// What the run routes answer from, both called only under an admitted credential: a
/// stop of the named run, and the wire snapshot of a run the credential may watch, `None`
/// for a run it has not seen or may not watch.
pub struct RunRoutes<'a> {
    pub stop: &'a (dyn Fn(&HttpRequest, &AdmittedAuthority, &str) -> HttpResponse + Sync),
    pub snapshot: &'a (dyn Fn(&AdmittedAuthority, &str) -> Option<String> + Sync),
}

/// A request slot held while one request is in flight.
struct Slot<'s>(&'s AtomicUsize);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The declared in-flight ceiling, or `ServeDeclarationMissing` for none or zero
/// (`read.register.serve-declaration`); the error text begins with the identifier.
pub fn ceiling(max_in_flight: Option<usize>) -> Result<usize, String> {
    max_in_flight.filter(|n| *n > 0).ok_or_else(|| {
        ReadError::ServeDeclarationMissing(
            "`--max-in-flight <n>` declares how many requests run at once, a positive count; none ships by default".into(),
        )
        .to_string()
    })
}

/// The declared audience, or `ServeDeclarationMissing` for none or a blank one
/// (`read.register.serve-declaration`); the error text begins with the identifier.
pub fn audience(declared: Option<&str>) -> Result<&str, String> {
    declared.map(str::trim).filter(|a| !a.is_empty()).ok_or_else(|| {
        ReadError::ServeDeclarationMissing(
            "`--audience <aud>` declares the audience every credential names; none ships by default".into(),
        )
        .to_string()
    })
}

impl<'a, C: Clock + Sync> HttpFace<'a, C> {
    /// The face over `face`, recording each answered read through `record`, admitting
    /// through `admitting`, with at most `max_in_flight` requests in flight.
    pub fn new(
        face: &'a Face,
        clock: &'a (dyn Clock + Sync),
        record: &'a dyn ReadRecord,
        admitting: Admitting<'a, C>,
        max_in_flight: Option<usize>,
    ) -> Result<HttpFace<'a, C>, String> {
        let ceiling = ceiling(max_in_flight)?;
        audience(Some(admitting.audience))?;
        let tools = Tools::new(face, clock, record)?;
        Ok(HttpFace { tools, admitting, control: None, connections: None, ceiling, in_flight: AtomicUsize::new(0), draining: AtomicBool::new(false), exchange: None, exchange_unconfigured: false, claim_write: None, runs: None, awake: None, receive: None, receive_outcome: None, hydration: None })
    }

    /// Report `build` at the handshake and `/health` in place of this package's linked
    /// identity (`read.embed.build-identity`).
    pub fn with_build(mut self, build: contextful_core::read::face::BuildIdentity) -> Self {
        self.tools = self.tools.with_build(build);
        self
    }

    /// Record each read tool call's span through `spans`, under the caller's span its
    /// `traceparent` header or `_meta` names (`disclosure.record.face-carrier`).
    pub fn with_spans(mut self, spans: ReadSpans) -> Self {
        self.tools = self.tools.with_spans(spans);
        self
    }

    /// Attach the store's control API without adding tools to the closed read face.
    pub fn with_control(mut self, control: &'a (dyn Fn(&HttpRequest, &AdmittedAuthority) -> HttpResponse + Sync)) -> Self {
        self.control = Some(control);
        self
    }

    /// Attach tenant connection mutations outside the operator control namespace.
    pub fn with_connections(mut self, connections: &'a (dyn Fn(&HttpRequest, &AdmittedAuthority) -> HttpResponse + Sync)) -> Self {
        self.connections = Some(connections);
        self
    }

    /// The binary's exchange route mints the reader credential without putting issuer
    /// signing material in the read-transport package.
    pub fn with_exchange(mut self, exchange: &'a (dyn Fn(&HttpRequest) -> HttpResponse + Sync), unconfigured: bool) -> Self {
        self.exchange = Some(exchange);
        self.exchange_unconfigured = unconfigured;
        self
    }

    /// The binary owns the claim writer; this route never joins the read MCP tool set.
    pub fn with_claim_write(mut self, write: &'a (dyn Fn(&HttpRequest, &AdmittedAuthority, &dyn Fn() -> Result<(), AuthorityError>) -> HttpResponse + Sync)) -> Self {
        self.claim_write = Some(write);
        self
    }

    /// Gate every route but `GET /health` on `hydration`; a face with none serves from start.
    pub fn with_hydration(mut self, hydration: &'a Hydration) -> Self {
        self.hydration = Some(hydration);
        self
    }

    /// Attach the run routes: an authenticated stop and the read-only run-stream socket.
    pub fn with_runs(mut self, runs: RunRoutes<'a>) -> Self {
        self.runs = Some(runs);
        self
    }

    /// Attach the awakeable resume route, `GET` and `POST /awake/:token`, called only under
    /// an admitted credential with the token (`run.suspend.resume-authenticates`).
    pub fn with_awake(mut self, awake: &'a (dyn Fn(&HttpRequest, &AdmittedAuthority, &str) -> HttpResponse + Sync)) -> Self {
        self.awake = Some(awake);
        self
    }

    /// Attach the receive route, `POST /receive/:pipeline`, called once the credential on
    /// the request head admits and before any body byte is read (`run.land.receive-endpoint`).
    pub fn with_receive(mut self, receive: &'a ReceiveHandler<'a>) -> Self {
        self.receive = Some(receive);
        self
    }

    /// Attach the outcome route, `GET /receive/:pipeline/:key`, called once the credential
    /// on the request admits (`run.land.receive-queued-outcome`).
    pub fn with_receive_outcome(mut self, outcome: &'a OutcomeHandler<'a>) -> Self {
        self.receive_outcome = Some(outcome);
        self
    }

    /// Accept connections on `listener` until it fails. Each accepted connection takes a
    /// slot before its thread starts and holds it until its answer is written, so at most
    /// the ceiling's count of connection threads run. One accepted with every slot held
    /// goes, unparsed, to the one shedding thread, and is dropped when
    /// [`SHED_QUEUE`] connections already wait there (`read.register.connection-ceiling`).
    /// Once [`HttpFace::drain`] begins, every connection goes to the shedding thread
    /// (`read.register.draining-health`).
    pub fn serve(&self, listener: TcpListener) -> std::io::Result<()> {
        std::thread::scope(|scope| {
            let (shed, queue) = std::sync::mpsc::sync_channel::<TcpStream>(SHED_QUEUE);
            scope.spawn(move || {
                for stream in queue {
                    let _ = self.shed(stream);
                }
            });
            for stream in listener.incoming() {
                let stream = match stream {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionAborted => continue,
                    Err(e) => return Err(e),
                };
                // The slot is counted before the drain flag is read, so a drain that reads
                // no slot held finds every later connection shed.
                let slot = self.slot();
                if self.draining.load(Ordering::SeqCst) {
                    drop(slot);
                    let _ = shed.try_send(stream);
                    continue;
                }
                match slot {
                    Some(slot) => {
                        scope.spawn(move || {
                            let _ = self.connection(stream, slot);
                        });
                    }
                    None => {
                        let _ = shed.try_send(stream);
                    }
                }
            }
            Ok(())
        })
    }

    /// Read one request under `slot`, answer it and close. The slot frees once the answer
    /// is written, before the close that ends the caller's read.
    fn connection(&self, mut stream: TcpStream, slot: Slot<'_>) -> std::io::Result<()> {
        stream.set_read_timeout(Some(REQUEST_READ_TIMEOUT))?;
        stream.set_write_timeout(Some(REQUEST_READ_TIMEOUT))?;
        let mut unread = false;
        let response = match read_head(&mut stream) {
            Err(response) => response,
            Ok((head, _)) if self.exchange_unconfigured && head.path() == "/auth/exchange" => self.answer(&head),
            Ok((head, rest)) => match receive_route(head.path()).filter(|_| self.receive.is_some()) {
                Some(pipeline) => {
                    let pipeline = pipeline.to_string();
                    let (response, read) = self.receive_request(&head, rest, &mut stream, &pipeline);
                    unread = !read;
                    response
                }
                None => match read_body(&mut stream, head, rest, REQUEST_BODY_BYTES) {
                    Ok(request) => self.answer(&request),
                    Err(response) => response,
                },
            },
        };
        let written = response.write_to(&mut stream);
        drop(slot);
        written?;
        stream.shutdown(std::net::Shutdown::Write)?;
        if unread {
            // A delivery refused on its head leaves its body unread; discarding what the
            // sender still writes keeps the close from resetting the answer away.
            discard(&mut stream)?;
        }
        Ok(())
    }

    /// The receive route: admit the credential on the head, then hand the handler a reader
    /// for the body. The second value tells whether the body was read.
    fn receive_request(&self, head: &HttpRequest, rest: Vec<u8>, stream: &mut TcpStream, pipeline: &str) -> (HttpResponse, bool) {
        if self.hydration.map_or("complete", Hydration::state) == "warming" {
            return (HttpResponse::unavailable("the snapshot set is hydrating; deliveries answer once it is local"), false);
        }
        if head.method != "POST" {
            return (HttpResponse::message(405, "`/receive/:pipeline` answers POST").with("Allow", "POST"), false);
        }
        if head.header("Transfer-Encoding").is_some() {
            return (HttpResponse::message(411, "the receive route reads a `Content-Length` body"), false);
        }
        let length = match head.header("Content-Length").map(|v| v.trim().parse::<usize>()) {
            None => 0,
            Some(Ok(n)) => n,
            Some(Err(_)) => return (HttpResponse::message(400, "`Content-Length` is not a count"), false),
        };
        // The credential admits before any body byte is read; a holder-bound credential's
        // proof covers the body, so it is checked once the body arrives.
        let mut covered = None;
        let authority = match self.admit(head, Some(&mut covered)) {
            Ok(a) => a,
            Err(response) => return (response, false),
        };
        let mut rest = Some(rest);
        let mut consumed = false;
        let response = {
            let mut read = |bound: u64| -> Result<Option<Vec<u8>>, HttpResponse> {
                let Some(rest) = rest.take() else {
                    return Err(HttpResponse::message(500, "a receive body is read once"));
                };
                if length as u64 > bound {
                    return Ok(None);
                }
                let request = read_body(stream, head.clone(), rest, length)?;
                consumed = true;
                if let Some(covered) = &covered {
                    verify_body_digest(covered, &request.body).map_err(|refusal| match refusal {
                        ProofRefusal::Refused(e) => unadmitted(&e),
                        ProofRefusal::NonceCacheFull => HttpResponse::unavailable(ProofRefusal::NonceCacheFull.to_string()),
                    })?;
                }
                Ok(Some(request.body))
            };
            self.receive.expect("the route is installed only with a receive handler")(head, &authority, pipeline, &mut read)
        };
        (response, consumed)
    }

    /// Answer `503` on a connection past the ceiling without parsing its request, then
    /// discard what the caller sends until it closes or [`SHED_LINGER`] passes, so the
    /// close sends no reset ahead of the answer (`read.register.past-ceiling`).
    fn shed(&self, mut stream: TcpStream) -> std::io::Result<()> {
        stream.set_write_timeout(Some(SHED_LINGER))?;
        let why = if self.draining.load(Ordering::SeqCst) {
            "the server is stopping and admits no new request".to_string()
        } else {
            format!("{} requests are in flight, the declared ceiling", self.ceiling)
        };
        HttpResponse::unavailable(why).write_to(&mut stream)?;
        stream.shutdown(std::net::Shutdown::Write)?;
        discard(&mut stream)
    }

    /// Admit no new request, then wait until every connection accepted before the call is
    /// answered or `bound` passes; the count still unanswered (`read.register.stop-drain`,
    /// `read.register.drain-unfinished`).
    pub fn drain(&self, bound: Duration) -> usize {
        self.draining.store(true, Ordering::SeqCst);
        let started = std::time::Instant::now();
        loop {
            let held = self.in_flight.load(Ordering::SeqCst);
            if held == 0 || started.elapsed() >= bound {
                return held;
            }
            std::thread::sleep(DRAIN_POLL);
        }
    }

    /// Take a slot under the ceiling, or none when the ceiling's count is in flight
    /// (`read.register.past-ceiling`).
    fn slot(&self) -> Option<Slot<'_>> {
        let taken = self.in_flight.fetch_add(1, Ordering::SeqCst);
        let slot = Slot(&self.in_flight);
        (taken < self.ceiling).then_some(slot)
    }

    /// Answer one request under a slot already held.
    pub fn answer(&self, request: &HttpRequest) -> HttpResponse {
        let hydration = self.hydration.map_or("complete", Hydration::state);
        // A readiness probe asking for hydration answers as a read does while the set warms
        // (`read.register.health-hydrated`).
        let probe = request.path() == HEALTH_PATH && request.method == "GET" && !request.query_pairs().any(|p| p == ("ready", "hydrated"));
        if hydration == "warming" && !probe {
            return HttpResponse::unavailable("the snapshot set is hydrating; reads answer once it is local");
        }
        match (request.path(), request.method.as_str()) {
            (HEALTH_PATH, "GET") => HttpResponse::json(200, &json!({ "contextful.build": self.tools.build(), "hydration": hydration })),
            (HEALTH_PATH, _) => HttpResponse::message(405, "`/health` answers GET").with("Allow", "GET"),
            (MCP_PATH, "POST") => self.message(request),
            (MCP_PATH, _) => {
                HttpResponse::message(405, "`/mcp` answers POST; the face holds no session and opens no server stream").with("Allow", "POST")
            }
            (path, method) if self.connections.is_some() && connection_method(path).is_some() => {
                let allowed = connection_method(path).expect("matched route");
                if method != allowed {
                    HttpResponse::message(405, format!("`{path}` answers {allowed}")).with("Allow", allowed)
                } else {
                    self.admitted(request, |authority, admission| {
                        if let Err(error) = effect_boundary(authority, admission) {
                            return unadmitted(&error);
                        }
                        self.connections.expect("route exists")(request, authority)
                    })
                }
            }
            (WORKFLOWS_PATH, "GET") | (RECORD_PATH, "GET") | (EDIT_PATH, "POST") | (APPLY_PATH, "POST") if self.control.is_some() => self.control_request(request),
            (WORKFLOWS_PATH, _) if self.control.is_some() => HttpResponse::message(405, "`/control/workflows` answers GET").with("Allow", "GET"),
            (RECORD_PATH, _) if self.control.is_some() => HttpResponse::message(405, "`/control/record` answers GET").with("Allow", "GET"),
            (EDIT_PATH, _) if self.control.is_some() => HttpResponse::message(405, "`/control/edit` answers POST").with("Allow", "POST"),
            (APPLY_PATH, _) if self.control.is_some() => HttpResponse::message(405, "`/control/apply` answers POST").with("Allow", "POST"),
            ("/memory/claims", "POST") if self.claim_write.is_some() => self.claim_write_request(request),
            ("/memory/claims", _) if self.claim_write.is_some() => HttpResponse::message(405, "`/memory/claims` answers POST").with("Allow", "POST"),
            ("/auth/exchange", "POST") if self.exchange.is_some() => self.exchange.expect("checked above")(request),
            ("/auth/exchange", _) if self.exchange.is_some() => HttpResponse::message(405, "`/auth/exchange` answers POST").with("Allow", "POST"),
            (path, method) if self.runs.is_some() && run_route(path).is_some() => {
                let (run, stream) = run_route(path).expect("matched above");
                match (stream, method) {
                    (false, "POST") => self.stop_request(request, run),
                    (false, _) => HttpResponse::message(405, "`/runs/:run_id/stop` answers POST").with("Allow", "POST"),
                    (true, "GET") => self.stream_request(request, run),
                    (true, _) => HttpResponse::message(405, "`/runs/:run_id/stream` answers GET").with("Allow", "GET"),
                }
            }
            (path, method) if self.receive_outcome.is_some() && outcome_route(path).is_some() => {
                let (pipeline, key) = outcome_route(path).expect("matched above");
                match method {
                    "GET" => self.admitted(request, |authority, _| self.receive_outcome.expect("route exists")(request, authority, pipeline, &key)),
                    _ => HttpResponse::message(405, "`/receive/:pipeline/:key` answers GET").with("Allow", "GET"),
                }
            }
            (path, method) if self.awake.is_some() && awake_route(path).is_some() => {
                let token = awake_route(path).expect("matched above");
                match method {
                    "GET" | "POST" => self.admitted(request, |authority, _| self.awake.expect("route exists")(request, authority, token)),
                    _ => HttpResponse::message(405, "`/awake/:token` answers GET and POST").with("Allow", "GET, POST"),
                }
            }
            (other, _) => HttpResponse::message(404, format!("no route `{other}`; the protocol endpoint is `{MCP_PATH}`")),
        }
    }

    /// A stop under an admitted credential; the binary authorizes it against the run
    /// record (`run.cancel.stop-route`).
    fn stop_request(&self, request: &HttpRequest, run: &str) -> HttpResponse {
        self.admitted(request, |authority, _| (self.runs.as_ref().expect("route exists").stop)(request, authority, run))
    }

    /// The run-stream socket: the credential admits on the HTTP request before any run is
    /// looked up or the connection upgrades; a refusal answers `401` as
    /// `RunStreamUnauthorized`, an unseen run `404`, and a seen run upgrades, receives its
    /// snapshot as one text frame and is closed, reading nothing from the caller
    /// (`run.project.unauthenticated-upgrade`, `run.project.read-only-socket`).
    fn stream_request(&self, request: &HttpRequest, run: &str) -> HttpResponse {
        let upgrading = request.header("Upgrade").is_some_and(|u| u.trim().eq_ignore_ascii_case("websocket"));
        let Some(key) = request.header("Sec-WebSocket-Key").map(str::trim).filter(|k| upgrading && !k.is_empty()) else {
            return HttpResponse::message(400, "a run stream is a websocket upgrade carrying `Upgrade: websocket` and `Sec-WebSocket-Key`");
        };
        let answer = self.admitted(request, |authority, _| match (self.runs.as_ref().expect("route exists").snapshot)(authority, run) {
            None => HttpResponse::message(404, format!("no run `{run}` to stream")),
            Some(snapshot) => HttpResponse::upgraded(key, snapshot.as_bytes()),
        });
        if answer.status != 401 {
            return answer;
        }
        let why = serde_json::from_slice::<Value>(&answer.body).ok().and_then(|b| b["error"]["message"].as_str().map(str::to_string)).unwrap_or_default();
        let text = contextful_core::run::RunError::RunStreamUnauthorized(why).to_string();
        HttpResponse::json(401, &json!({ "error": { "http": 401, "identifier": "RunStreamUnauthorized", "message": text } })).with("WWW-Authenticate", CHALLENGE)
    }

    /// Admit the request's credential, then answer its one message.
    fn message(&self, request: &HttpRequest) -> HttpResponse {
        self.admitted(request, |authority, admission| {
            let message: Value = match serde_json::from_slice(&request.body) {
                Ok(m @ Value::Object(_)) => m,
                Ok(_) => return HttpResponse::rpc_error(INVALID_REQUEST, "a request body holds one JSON-RPC message object"),
                Err(e) => return HttpResponse::rpc_error(PARSE_ERROR, format!("the request body is not JSON: {e}")),
            };
            let boundary = |a: &AdmittedAuthority| effect_boundary(a, admission);
            let carrier = Carrier { traceparent: request.header(TRACEPARENT), tracestate: request.header(TRACESTATE) };
            match self.tools.handle(Caller { authority, boundary: &boundary, carrier }, &message) {
                Some(answer) => HttpResponse::json(200, &answer),
                None => HttpResponse::empty(202),
            }
        })
    }

    fn claim_write_request(&self, request: &HttpRequest) -> HttpResponse {
        if request.header("Origin").is_some() {
            return HttpResponse::json(403, &json!({ "error": { "http": 403, "identifier": "MemoryClaimBrowserRefused" } }));
        }
        self.admitted(request, |authority, _| {
            let boundary = || {
                let revocation = (self.admitting.revocation)().map_err(AuthorityError::AuthorityRevoked)?;
                let admission = Admission::new(self.tools.clock().now(), &revocation).expecting(self.admitting.audience);
                effect_boundary(authority, &admission)
            };
            self.claim_write.expect("route exists")(request, authority, &boundary)
        })
    }

    /// Control routes admit only an untenanted Admin grant over `*` (`surface.apply.served-admin-grant`).
    fn control_request(&self, request: &HttpRequest) -> HttpResponse {
        self.admitted(request, |authority, _| {
            if !authority.grants().iter().any(|grant| grant.actions.contains(&Action::Admin) && grant.tables.contains(&TablePattern::All) && grant.tenant.is_none()) {
                return HttpResponse::json(403, &json!({ "error": { "identifier": "ControlAdminGrantMissing" } }));
            }
            let revocation = match (self.admitting.revocation)() {
                Ok(r) => r,
                Err(why) => return HttpResponse::unavailable(format!("the revocation denylist is unreadable: {why}")),
            };
            let boundary = Admission::new(self.tools.clock().now(), &revocation).expecting(self.admitting.audience);
            if let Err(error) = effect_boundary(authority, &boundary) {
                return unadmitted(&error);
            }
            self.control.expect("the route is installed only with a control handler")(request, authority)
        })
    }

    fn admitted(&self, request: &HttpRequest, answer: impl FnOnce(&AdmittedAuthority, &Admission<'_>) -> HttpResponse) -> HttpResponse {
        let revocation = match (self.admitting.revocation)() {
            Ok(r) => r,
            Err(why) => return HttpResponse::unavailable(format!("the revocation denylist is unreadable: {why}")),
        };
        let admission = Admission::new(self.tools.clock().now(), &revocation).expecting(self.admitting.audience);
        match self.admit_under(request, &admission, None) {
            Ok(authority) => answer(&authority, &admission),
            Err(response) => response,
        }
    }

    /// Admit the request's credential; with `deferred`, a holder-bound credential's proof
    /// is verified on the head alone and `deferred` takes the body digest it covers, which
    /// the caller compares once the body arrives.
    fn admit(&self, request: &HttpRequest, deferred: Option<&mut Option<String>>) -> Result<AdmittedAuthority, HttpResponse> {
        let revocation = (self.admitting.revocation)().map_err(|why| HttpResponse::unavailable(format!("the revocation denylist is unreadable: {why}")))?;
        let admission = Admission::new(self.tools.clock().now(), &revocation).expecting(self.admitting.audience);
        self.admit_under(request, &admission, deferred)
    }

    fn admit_under(&self, request: &HttpRequest, admission: &Admission<'_>, deferred: Option<&mut Option<String>>) -> Result<AdmittedAuthority, HttpResponse> {
        let Some(credential) = request.credential() else {
            let missing = ReadError::HttpCredentialMissing(
                "a request carries `Authorization: Bearer <credential>`, or `Authorization: DPoP <credential>` with a `DPoP` proof from the credential's holder key".into(),
            );
            return Err(HttpResponse::json(401, &payload(&Refusal::Read(missing))).with("WWW-Authenticate", CHALLENGE));
        };
        let covered = ProofRequest { method: &request.method, target: &request.target, body: &request.body };
        let checkpoint = self.admitting.checkpoint;
        let keys = checkpoint.keys().map_err(|e| HttpResponse::unavailable(e.to_string()))?;
        let authority = verify_network(credential, &keys, admission, |jkt| match (request.header("DPoP"), deferred) {
            (Some(proof), Some(deferred)) => {
                *deferred = Some(checkpoint.verify_head(jkt, proof, &request.method, &request.target)?);
                Ok(())
            }
            (Some(proof), None) => checkpoint.verify_request(jkt, proof, &covered),
            (None, _) => Err(ProofRefusal::Refused(AuthorityError::PossessionProofInvalid(
                "the credential binds a holder key, and the request carries no `DPoP` proof".into(),
            ))),
        });
        match authority {
            Ok(a) => Ok(a),
            Err(ProofRefusal::NonceCacheFull) => Err(HttpResponse::unavailable(ProofRefusal::NonceCacheFull.to_string())),
            Err(ProofRefusal::Refused(e)) => Err(unadmitted(&e)),
        }
    }
}

fn connection_method(path: &str) -> Option<&'static str> {
    if path == "/connections" {
        return Some("POST");
    }
    let rest = path.strip_prefix("/connections/")?;
    if let Some(id) = rest.strip_suffix("/pause") {
        return (!id.is_empty() && !id.contains('/')).then_some("POST");
    }
    (!rest.is_empty() && !rest.contains('/')).then_some("DELETE")
}

/// Read one request: the head, then a `Content-Length` body of at most
/// [`REQUEST_BODY_BYTES`]. A malformed or oversized request is answered, not read on.
pub fn read_request(stream: &mut impl Read) -> Result<HttpRequest, HttpResponse> {
    let (request, rest) = read_head(stream)?;
    read_body(stream, request, rest, REQUEST_BODY_BYTES)
}

/// Read one request head; the bytes read past it start the body.
fn read_head(stream: &mut impl Read) -> Result<(HttpRequest, Vec<u8>), HttpResponse> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    loop {
        let n = match stream.read(&mut chunk) {
            Ok(0) => return Err(HttpResponse::message(400, "the connection closed before a request head")),
            Ok(n) => n,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                return Err(HttpResponse::message(408, "the request did not arrive in time"))
            }
            Err(e) => return Err(HttpResponse::message(400, e.to_string())),
        };
        buf.extend_from_slice(&chunk[..n]);
        let mut headers = [httparse::EMPTY_HEADER; REQUEST_HEADERS];
        let mut parsed = httparse::Request::new(&mut headers);
        match parsed.parse(&buf) {
            Ok(httparse::Status::Complete(len)) => {
                let request = HttpRequest {
                    method: parsed.method.unwrap_or_default().to_string(),
                    target: parsed.path.unwrap_or_default().to_string(),
                    headers: parsed
                        .headers
                        .iter()
                        .map(|h| (h.name.to_string(), String::from_utf8_lossy(h.value).into_owned()))
                        .collect(),
                    body: Vec::new(),
                };
                return Ok((request, buf.split_off(len)));
            }
            Ok(httparse::Status::Partial) if buf.len() > REQUEST_HEAD_BYTES => {
                return Err(HttpResponse::message(431, format!("the request head exceeds {REQUEST_HEAD_BYTES} B")))
            }
            Ok(httparse::Status::Partial) => continue,
            Err(e) => return Err(HttpResponse::message(400, format!("the request head does not parse: {e}"))),
        }
    }
}

/// Read the `Content-Length` body of `request`, of at most `limit` bytes, `rest` holding
/// the bytes already read past its head.
fn read_body(stream: &mut impl Read, mut request: HttpRequest, rest: Vec<u8>, limit: usize) -> Result<HttpRequest, HttpResponse> {
    if request.header("Transfer-Encoding").is_some() {
        return Err(HttpResponse::message(411, "the face reads a `Content-Length` body"));
    }
    let length = match request.header("Content-Length") {
        None => 0,
        Some(v) => v.trim().parse::<usize>().map_err(|_| HttpResponse::message(400, "`Content-Length` is not a count"))?,
    };
    if length > limit {
        return Err(HttpResponse::message(413, format!("the request body exceeds {limit} B")));
    }
    let mut chunk = [0u8; 4096];
    let mut body = rest;
    body.truncate(length);
    while body.len() < length {
        let want = (length - body.len()).min(chunk.len());
        match stream.read(&mut chunk[..want]) {
            Ok(0) => return Err(HttpResponse::message(400, "the connection closed inside the request body")),
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                return Err(HttpResponse::message(408, "the request body did not arrive in time"))
            }
            Err(e) => return Err(HttpResponse::message(400, e.to_string())),
        }
    }
    request.body = body;
    Ok(request)
}

/// Discard what the caller still sends until it closes or [`SHED_LINGER`] passes.
fn discard(stream: &mut TcpStream) -> std::io::Result<()> {
    let deadline = std::time::Instant::now() + SHED_LINGER;
    let mut discard = [0u8; 4096];
    while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()).filter(|d| !d.is_zero()) {
        stream.set_read_timeout(Some(left))?;
        match stream.read(&mut discard) {
            Ok(n) if n > 0 => continue,
            _ => break,
        }
    }
    Ok(())
}

/// The pipeline of a `/receive/:pipeline` path: one non-empty segment; `None` for any other path.
fn receive_route(path: &str) -> Option<&str> {
    path.strip_prefix(RECEIVE_PREFIX).filter(|p| !p.is_empty() && !p.contains('/'))
}

/// The pipeline and percent-decoded batch key of a `/receive/:pipeline/:key` path: two
/// non-empty segments; `None` for any other path or a key that is not UTF-8 once decoded.
fn outcome_route(path: &str) -> Option<(&str, String)> {
    let (pipeline, key) = path.strip_prefix(RECEIVE_PREFIX)?.split_once('/')?;
    if pipeline.is_empty() || key.is_empty() || key.contains('/') {
        return None;
    }
    let bytes = key.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    Some((pipeline, String::from_utf8(decoded).ok()?))
}

/// The token of an `/awake/:token` path: one non-empty segment; `None` for any other path.
fn awake_route(path: &str) -> Option<&str> {
    path.strip_prefix("/awake/").filter(|t| !t.is_empty() && !t.contains('/'))
}

/// The run id and whether the path is the stream route, for `/runs/:run_id/stop` or
/// `/runs/:run_id/stream`; `None` for any other path.
fn run_route(path: &str) -> Option<(&str, bool)> {
    let rest = path.strip_prefix(RUNS_PREFIX)?;
    let (run, stream) = match (rest.strip_suffix(STOP_SUFFIX), rest.strip_suffix(STREAM_SUFFIX)) {
        (Some(run), _) => (run, false),
        (_, Some(run)) => (run, true),
        _ => return None,
    };
    (!run.is_empty() && !run.contains('/')).then_some((run, stream))
}

/// SHA-1 of `data` (FIPS 180-4), used only for the websocket handshake's accept value.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// Standard padded base64 of `data`.
fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
