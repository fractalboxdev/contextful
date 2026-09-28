//! A loopback HTTP/1.1 server a test scripts, and the resolver and request helpers the
//! source tests share.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

/// One request the server received.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    /// Path and query, as sent.
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// The first header of `name`, compared case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or_default()
    }

    /// The value of query parameter `name`.
    pub fn query(&self, name: &str) -> Option<String> {
        let q = self.target.split_once('?')?.1;
        q.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| v.replace("%3A", ":").replace("%2B", "+"))
    }
}

/// A response: status, headers and body.
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn json(status: u16, body: &str) -> Response {
        Response { status, headers: vec![("Content-Type".into(), "application/json".into())], body: body.as_bytes().to_vec() }
    }
}

type Handler = dyn Fn(&Request) -> Response + Send + Sync;

/// A running server; requests are served on a background thread for the test's life.
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

    /// Every request received at `path`.
    pub fn received(&self, path: &str) -> Vec<Request> {
        self.requests.lock().unwrap().iter().filter(|r| r.path() == path).cloned().collect()
    }
}

fn serve(stream: TcpStream, seen: &Mutex<Vec<Request>>, handler: &Handler) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or_default().to_string(), parts.next().unwrap_or_default().to_string());
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
    let request = Request { method, target, headers, body };
    let response = handler(&request);
    seen.lock().unwrap().push(request);
    let mut out = stream;
    write!(out, "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n", response.status, response.body.len())?;
    for (k, v) in &response.headers {
        write!(out, "{k}: {v}\r\n")?;
    }
    out.write_all(b"\r\n")?;
    out.write_all(&response.body)?;
    out.flush()
}

use contextful_connectors::http::{HttpConfig, HttpSource};
use contextful_core::connector::reference::{Hydrated, SecretName};
use contextful_core::connector::resolve::{Answer, Provider};
use contextful_core::ports::FixedClock;
use contextful_core::run::ports::{Cancellation, PullRequest};
use contextful_core::run::Failure;
use contextful_core::time::Instant;
use contextful_outbound::Resolver;

/// A provider answering every name from a fixed map.
pub struct Fixed(pub Vec<(&'static str, &'static str)>);

impl Provider for Fixed {
    fn name(&self) -> &str {
        "fixed"
    }

    fn answer(&self, name: &SecretName) -> Result<Option<Answer>, Failure> {
        Ok(self.0.iter().find(|(n, _)| *n == name.as_str()).map(|(_, v)| Answer { value: Hydrated::new(*v), expires_at: None }))
    }
}

pub struct Never;

impl Cancellation for Never {
    fn requested(&self) -> bool {
        false
    }
}

pub fn resolver(secrets: Vec<(&'static str, &'static str)>) -> Arc<Resolver> {
    let clock = Arc::new(FixedClock(Instant::parse("2030-01-01T00:00:00Z").unwrap()));
    Arc::new(Resolver::new(vec![Arc::new(Fixed(secrets))], false, clock))
}

/// A source over `config` for table `t`.
pub fn source(config: serde_json::Value, secrets: Vec<(&'static str, &'static str)>) -> HttpSource {
    HttpSource::new(HttpConfig::parse(&config).unwrap(), "t", resolver(secrets)).unwrap()
}

pub fn request(position: Option<serde_json::Value>) -> PullRequest {
    PullRequest { step_label: "pull-0".into(), position, idempotency_key: "idem-1".into() }
}
