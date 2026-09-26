//! The probe guest, one host for the process, and a loopback HTTP/1.1 server a test scripts.

use contextful_core::connector::attach::Allowlist;
use contextful_core::run::Failure;
use contextful_wasm::{ComponentHost, Connector, Grant, Limits, Reservation, Reserve, Session};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// The probe guest exporting `config` and `attribution` beside the base world.
pub const PROBE: &[u8] = include_bytes!("../fixtures/probe.wasm");
/// The probe guest exporting the base world alone.
pub const PROBE_BASE: &[u8] = include_bytes!("../fixtures/probe-base.wasm");
/// A component instantiating three linear memories, 281.25 MiB together (`memories.wat`).
pub const MEMORIES: &[u8] = include_bytes!("../fixtures/memories.wasm");

/// One host and both compiled guests for the process: compiling is per engine, and a
/// session carries its own store, memory cap and deadline.
pub fn host() -> &'static (ComponentHost, Connector, Connector) {
    static HOST: OnceLock<(ComponentHost, Connector, Connector)> = OnceLock::new();
    HOST.get_or_init(|| {
        let host = ComponentHost::new().expect("host");
        let probe = host.load(PROBE).expect("probe loads");
        let base = host.load(PROBE_BASE).expect("base probe loads");
        (host, probe, base)
    })
}

pub fn loopback() -> Grant {
    Grant { allow: Allowlist::parse(&["127.0.0.1"]).unwrap(), attach: Vec::new(), gate: None }
}

pub fn open_with(grant: Grant, limits: &Limits, config: Option<&serde_json::Value>) -> Result<Session, Failure> {
    let (host, probe, _) = host();
    host.open(probe, grant, limits, config)
}

pub fn open() -> Session {
    open_with(loopback(), &Limits::default(), None).expect("session opens")
}

/// The single text cell of a one-row batch.
pub fn text(ipc: &[u8]) -> String {
    let rows = contextful_wasm::batch::rows(ipc).expect("rows");
    assert_eq!(rows.len(), 1, "{rows:?}");
    rows[0].values().next().and_then(|v| v.as_str()).expect("a text cell").to_string()
}

/// A reservation point answering one fixed reservation and counting the asks.
pub struct Gate {
    pub answer: Reservation,
    pub asked: AtomicUsize,
}

impl Gate {
    pub fn new(answer: Reservation) -> Arc<Gate> {
        Arc::new(Gate { answer, asked: AtomicUsize::new(0) })
    }
}

impl Reserve for Gate {
    fn reserve(&self) -> Reservation {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.answer.clone()
    }
}

/// One request the server received.
#[derive(Debug, Clone)]
pub struct Request {
    pub headers: Vec<(String, String)>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    pub fn text(status: u16, body: &str) -> Response {
        Response { status, headers: Vec::new(), body: body.to_string() }
    }
}

type Handler = dyn Fn(&Request) -> Response + Send + Sync;

/// A server answering each connection on its own thread.
pub struct Server {
    pub port: u16,
    pub requests: Arc<Mutex<Vec<Request>>>,
}

impl Server {
    pub fn start(handler: impl Fn(&Request) -> Response + Send + Sync + 'static) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests: Arc<Mutex<Vec<Request>>> = Arc::default();
        let (seen, handler): (_, Arc<Handler>) = (requests.clone(), Arc::new(handler));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (seen, handler) = (seen.clone(), handler.clone());
                std::thread::spawn(move || {
                    let _ = serve(stream, &seen, handler.as_ref());
                });
            }
        });
        Server { port, requests }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    pub fn received(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

fn serve(stream: TcpStream, seen: &Mutex<Vec<Request>>, handler: &Handler) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        reader.read_line(&mut h)?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let len = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    let mut body = vec![0; len];
    reader.read_exact(&mut body)?;
    let request = Request { headers };
    seen.lock().unwrap().push(request.clone());
    let response = handler(&request);
    let mut out = stream;
    write!(out, "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n", response.status, response.body.len())?;
    for (k, v) in &response.headers {
        write!(out, "{k}: {v}\r\n")?;
    }
    out.write_all(b"\r\n")?;
    out.write_all(response.body.as_bytes())?;
    out.flush()
}
