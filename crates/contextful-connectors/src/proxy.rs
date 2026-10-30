//! The loopback proxy an exec step's vendor requests pass (`connector.attach.exec-proxy`):
//! the step speaks plain HTTP to `127.0.0.1`, and the host sends each request on to the
//! binding's `endpoint_host` through the mediated client, attaching the credential itself.

use contextful_core::connector::reference::Template;
use contextful_core::run::{Failure, FailureTag};
use contextful_outbound::client::{Client, HeaderValue};
use contextful_outbound::Resolver;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use url::Url;

/// Request head bytes the proxy reads: 64 KiB (`connector.attach.exec-proxy-request`).
pub const PROXY_HEAD_BYTES: usize = 64 * 1024;
/// Request body bytes the proxy forwards: 64 MiB (`connector.attach.exec-proxy-request`).
pub const PROXY_BODY_BYTES: u64 = 64 * 1024 * 1024;
/// The variable naming the proxy in a step's environment (`connector.attach.exec-environment`).
pub const EGRESS_URL: &str = "CONTEXTFUL_EGRESS_URL";
/// How long the proxy waits on a step's request bytes.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// How often the accept loop checks for a stop.
const ACCEPT_TICK: Duration = Duration::from_millis(5);

/// Headers that describe one connection, not the request: the proxy forwards none.
const HOP_BY_HOP: [&str; 11] =
    ["connection", "keep-alive", "proxy-authenticate", "proxy-authorization", "proxy-connection", "te", "trailer", "transfer-encoding", "upgrade", "host", "content-length"];

/// What the proxy forwards through: the mediated client bound to the vendor's origin, the
/// binding's `attach` templates and the resolver that hydrates them.
struct Forward {
    client: Client,
    origin: Url,
    attach: Vec<(String, Template)>,
    resolver: Arc<Resolver>,
    /// Failures the mediated client raised since the last [`Proxy::take_failure`].
    failures: Mutex<Vec<Failure>>,
}

/// One loopback proxy, open from [`Proxy::start`] until it drops.
pub struct Proxy {
    addr: SocketAddr,
    forward: Arc<Forward>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl Proxy {
    /// Listen on an ephemeral loopback port and forward each request to `origin`.
    pub fn start(client: Client, origin: Url, attach: Vec<(String, Template)>, resolver: Arc<Resolver>) -> std::io::Result<Proxy> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let addr = listener.local_addr()?;
        // A non-blocking accept lets the loop see the stop flag without a wake-up dial.
        listener.set_nonblocking(true)?;
        let forward = Arc::new(Forward { client, origin, attach, resolver, failures: Mutex::new(Vec::new()) });
        let stop = Arc::new(AtomicBool::new(false));
        let accept = {
            let (forward, stop) = (forward.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if stream.set_nonblocking(false).is_ok() {
                                let forward = forward.clone();
                                std::thread::spawn(move || serve(&forward, stream));
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(ACCEPT_TICK),
                        Err(_) => std::thread::sleep(ACCEPT_TICK),
                    }
                }
            })
        };
        Ok(Proxy { addr, forward, stop, accept: Some(accept) })
    }

    /// `http://127.0.0.1:<port>`, the value of [`EGRESS_URL`].
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The first failure the mediated client raised since the last call, clearing the rest.
    pub fn take_failure(&self) -> Option<Failure> {
        let mut failures = self.forward.failures.lock().ok()?;
        let first = (!failures.is_empty()).then(|| failures.remove(0));
        failures.clear();
        first
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // The accept loop sees the flag within one tick and closes the listener.
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

/// One parsed request a step sent.
struct Request {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// Why a request went no further than the proxy: the status it answers and its text.
type Refusal = (u16, String);

fn serve(forward: &Forward, mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let answer = match read_request(&mut stream) {
        Ok(request) => send(forward, request),
        Err((status, text)) => plain(status, &text),
    };
    let _ = stream.write_all(&answer);
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Write);
    // Reading what the step still sends keeps the close from resetting the answer away.
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let _ = std::io::copy(&mut (&mut stream).take(PROXY_HEAD_BYTES as u64), &mut std::io::sink());
}

fn read_request(stream: &mut TcpStream) -> Result<Request, Refusal> {
    let mut buffered = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(at) = buffered.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        if buffered.len() > PROXY_HEAD_BYTES {
            return Err((413, format!("the request head passes {PROXY_HEAD_BYTES} bytes")));
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return Err((400, "the request ended before its head".into())),
            Ok(n) => buffered.extend_from_slice(&chunk[..n]),
        }
    };
    if head_end > PROXY_HEAD_BYTES {
        return Err((413, format!("the request head passes {PROXY_HEAD_BYTES} bytes")));
    }
    let head = std::str::from_utf8(&buffered[..head_end]).map_err(|_| (400, "the request head is not UTF-8".to_string()))?;
    let mut lines = head.split("\r\n");
    let mut line = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(target), Some(_version), None) = (line.next(), line.next(), line.next(), line.next()) else {
        return Err((400, "the request line is not `<method> <path> HTTP/1.1`".into()));
    };
    if !target.starts_with('/') || method.is_empty() {
        return Err((400, "the request target is not an origin-form path".into()));
    }
    let mut headers = Vec::new();
    for l in lines {
        let (name, value) = l.split_once(':').ok_or_else(|| (400, "a header line holds no `:`".to_string()))?;
        headers.push((name.trim().to_string(), value.trim().to_string()));
    }
    let named = |n: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.as_str());
    if named("transfer-encoding").is_some() {
        return Err((411, "the proxy reads a body by `Content-Length` alone".into()));
    }
    let length = match named("content-length") {
        None => 0,
        Some(v) => v.parse::<u64>().map_err(|_| (400, "`Content-Length` is not a byte count".to_string()))?,
    };
    if length > PROXY_BODY_BYTES {
        return Err((413, format!("the request body passes {PROXY_BODY_BYTES} bytes")));
    }
    let mut body = buffered[head_end + 4..].to_vec();
    body.truncate(length as usize);
    while (body.len() as u64) < length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return Err((400, "the request ended before its `Content-Length`".into())),
            Ok(n) => body.extend_from_slice(&chunk[..n.min((length as usize) - body.len())]),
        }
    }
    Ok(Request { method: method.to_string(), target: target.to_string(), headers, body })
}

fn send(forward: &Forward, request: Request) -> Vec<u8> {
    let attached = |name: &str| forward.attach.iter().any(|(h, _)| h.eq_ignore_ascii_case(name));
    let mut headers: Vec<(String, HeaderValue)> = request
        .headers
        .into_iter()
        .filter(|(k, _)| !HOP_BY_HOP.iter().any(|h| k.eq_ignore_ascii_case(h)) && !attached(k))
        .map(|(k, v)| (k, HeaderValue::Plain(v)))
        .collect();
    let outcome = (|| {
        for (name, template) in &forward.attach {
            headers.push((name.clone(), HeaderValue::Sensitive(forward.resolver.render(template)?.into())));
        }
        let url = Url::parse(&format!("{}{}", forward.origin.as_str().trim_end_matches('/'), request.target))
            .map_err(|e| Failure::deterministic(FailureTag::Config, format!("the step's path names no URL under `{}`: {e}", forward.origin)))?;
        let body = (!request.body.is_empty()).then_some(request.body.as_slice());
        forward.client.send_once(&request.method, &url, &headers, body)
    })();
    match outcome {
        Ok(response) => {
            let mut out = format!("HTTP/1.1 {} \r\n", response.status).into_bytes();
            for (k, v) in &response.headers {
                if !HOP_BY_HOP.iter().any(|h| k.eq_ignore_ascii_case(h)) {
                    out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
                }
            }
            out.extend_from_slice(format!("Content-Length: {}\r\nConnection: close\r\n\r\n", response.body.len()).as_bytes());
            out.extend_from_slice(&response.body);
            out
        }
        Err(failure) => {
            let text = failure.message.clone();
            if let Ok(mut failures) = forward.failures.lock() {
                failures.push(failure);
            }
            plain(502, &text)
        }
    }
}

fn plain(status: u16, text: &str) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status} \r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", text.len()).into_bytes();
    out.extend_from_slice(text.as_bytes());
    out
}
