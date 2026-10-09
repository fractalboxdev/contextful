//! A loopback HTTP/1.1 server a test scripts, a settable clock and a counting provider.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, PoisonError, RwLockReadGuard, RwLockWriteGuard};

/// The variables a ureq agent reads its proxy and bypass list from, at the moment the agent is built.
const PROXY_VARS: [&str; 8] = ["ALL_PROXY", "all_proxy", "HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "NO_PROXY", "no_proxy"];

/// A share of the proxy environment. Every test in a module that sends HTTP holds one for
/// its whole body, so no client it builds sees a proxy another test configured.
pub fn proxy_env() -> RwLockReadGuard<'static, ()> {
    crate::PROXY_ENV.read().unwrap_or_else(PoisonError::into_inner)
}

/// The process environment routing every scheme through one proxy with no bypass list,
/// held while no [`proxy_env`] share is out. Dropping it restores each variable as it was.
pub struct ProxyConfigured {
    saved: Vec<(&'static str, Option<OsString>)>,
    _exclusive: RwLockWriteGuard<'static, ()>,
}

pub fn configure_proxy(url: &str) -> ProxyConfigured {
    let exclusive = crate::PROXY_ENV.write().unwrap_or_else(PoisonError::into_inner);
    let saved = PROXY_VARS.iter().map(|k| (*k, std::env::var_os(k))).collect();
    for k in PROXY_VARS {
        if k.eq_ignore_ascii_case("no_proxy") {
            std::env::remove_var(k);
        } else {
            std::env::set_var(k, url);
        }
    }
    ProxyConfigured { saved, _exclusive: exclusive }
}

impl Drop for ProxyConfigured {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }
}

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

use contextful_core::connector::reference::{Hydrated, SecretName};
use contextful_core::connector::resolve::{Answer, Provider};
use contextful_core::ports::Clock;
use contextful_core::run::Failure;
use contextful_core::time::Instant;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

pub fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

pub const T0: &str = "2030-01-01T00:00:00Z";

/// A clock a test moves by hand.
#[derive(Clone)]
pub struct SetClock(pub Arc<AtomicI64>);

impl SetClock {
    pub fn new() -> SetClock {
        SetClock(Arc::new(AtomicI64::new(at(T0).unix_secs())))
    }

    pub fn advance(&self, secs: i64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }
}

impl Clock for SetClock {
    fn now(&self) -> Instant {
        Instant::from_unix_secs(self.0.load(Ordering::SeqCst)).unwrap()
    }
}

/// A provider answering a fixed map of names, counting every call.
pub struct Fixed {
    pub label: &'static str,
    pub values: Mutex<Vec<(String, String)>>,
    pub calls: AtomicUsize,
    pub delay_ms: u64,
}

impl Fixed {
    pub fn new(label: &'static str, values: &[(&str, &str)]) -> Arc<Fixed> {
        Arc::new(Fixed {
            label,
            values: Mutex::new(values.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()),
            calls: AtomicUsize::new(0),
            delay_ms: 0,
        })
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub fn set(&self, name: &str, value: &str) {
        self.values.lock().unwrap().push((name.to_string(), value.to_string()));
    }
}

impl Provider for Fixed {
    fn name(&self) -> &str {
        self.label
    }

    fn answer(&self, name: &SecretName) -> Result<Option<Answer>, Failure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(self.delay_ms));
        Ok(self.values.lock().unwrap().iter().find(|(k, _)| k == name.as_str()).map(|(_, v)| Answer { value: Hydrated::new(v.clone()), expires_at: None }))
    }
}

pub fn name(s: &str) -> SecretName {
    SecretName::parse(s).unwrap()
}
