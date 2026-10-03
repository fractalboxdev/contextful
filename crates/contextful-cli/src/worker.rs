//! Remote workers (`surface.dispatch`): the relay route `serve` mounts for its workers, the
//! client that submits a step to a worker, and `pipeline worker`, the worker itself.
//!
//! A worker takes `POST /submit` signed under the worker key and naming a callback on its
//! `[control] relay`, answering `401` otherwise; it runs the step as a child `pipeline run --applied <N>` of
//! this binary, heartbeats with a signed `GET` on the step's awakeable route every 15 s, and
//! posts the outcome there with a signed `POST`. The relay answers `200`, `404` for a token
//! no step holds, `409` for `DispatchCallbackRejected` and `422` for `StepResultRefused`.

use crate::run::ProjectArgs;
use anyhow::{bail, Result};
use contextful_core::surface::worker::{
    admit_submission, sign_submission, Signed, StepOutcome, StepResult, HEARTBEAT_BEAT_SECS, SIGNATURE_HEADER, STEP_RESULT_CAP, TIMESTAMP_HEADER, WORKER_KEY_VAR,
};
use contextful_engine::worker::{Relay, Submission, WorkerClient};
use contextful_outbound::egress::{system, Outbound, Transport};
use contextful_outbound::HeaderValue;
use serde_json::json;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use url::Url;

/// Wall clock one request to a worker or the relay may take.
const HOP_TIMEOUT: Duration = Duration::from_secs(10);

/// Largest request head either server reads.
const HEAD_MAX: usize = 16 * 1024;

/// The deployment's worker key, from [`WORKER_KEY_VAR`].
pub(crate) fn worker_key() -> Result<Vec<u8>> {
    match std::env::var(WORKER_KEY_VAR) {
        Ok(k) if !k.is_empty() => Ok(k.into_bytes()),
        _ => bail!("`[control] workers` signs every heartbeat and callback under a key: set {WORKER_KEY_VAR}"),
    }
}

/// One HTTP/1.1 request.
pub(crate) struct Request {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

/// Read one request from `stream`, its body at most `max_body` bytes.
pub(crate) fn read_request(stream: &TcpStream, max_body: usize) -> Result<Request> {
    stream.set_read_timeout(Some(HOP_TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or_default().to_string(), parts.next().unwrap_or_default().to_string());
    let mut headers = BTreeMap::new();
    let mut read = line.len();
    loop {
        let mut h = String::new();
        let n = reader.read_line(&mut h)?;
        read += n;
        if read > HEAD_MAX {
            bail!("the request head passes {HEAD_MAX} bytes");
        }
        if n == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let length: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut body = Vec::new();
    reader.take(length.min(max_body + 1) as u64).read_to_end(&mut body)?;
    Ok(Request { method, path, headers, body })
}

/// Answer `status` with a JSON body.
pub(crate) fn respond(mut stream: &TcpStream, status: u16, body: &serde_json::Value) -> Result<()> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        422 => "Unprocessable Content",
        _ => "Internal Server Error",
    };
    let body = serde_json::to_vec(body)?;
    write!(stream, "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())?;
    stream.write_all(&body)?;
    Ok(())
}

/// Send one request to `url` over the system transport, answering status and body.
fn send(transport: &dyn Transport, method: &str, url: &Url, headers: Vec<(String, String)>, body: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let host = url.host_str().unwrap_or_default();
    let addrs = transport.resolve(host, url.port_or_known_default().unwrap_or(80))?;
    let headers: Vec<(String, HeaderValue)> = headers.into_iter().map(|(k, v)| (k, HeaderValue::Plain(v))).collect();
    let request = Outbound {
        method,
        url,
        addrs: &addrs,
        headers: &headers,
        body,
        direct: true,
        timeout: HOP_TIMEOUT,
        max_body: 64 * 1024,
        read_body: true,
    };
    let answer = transport.send(&request).map_err(|e| format!("{e:?}"))?;
    Ok((answer.status, answer.body))
}

/// Seconds since the Unix epoch on the system clock.
fn unix_now() -> i64 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default();
    i64::try_from(now).unwrap_or(i64::MAX)
}

/// Submits steps to workers over HTTP, each signed under the worker key
/// (`surface.dispatch.submit-signed`).
pub(crate) struct HttpWorkers {
    pub transport: Arc<dyn Transport>,
    pub key: Vec<u8>,
}

impl WorkerClient for HttpWorkers {
    fn submit(&self, worker: &str, submission: &Submission) -> Result<(), String> {
        let url = Url::parse(&format!("{}/submit", worker.trim_end_matches('/'))).map_err(|e| format!("worker URL `{worker}`: {e}"))?;
        let body = serde_json::to_vec(submission).map_err(|e| e.to_string())?;
        let timestamp = unix_now();
        let headers = vec![
            ("Content-Type".into(), "application/json".into()),
            (TIMESTAMP_HEADER.into(), timestamp.to_string()),
            (SIGNATURE_HEADER.into(), sign_submission(&self.key, timestamp, &body)),
        ];
        match send(self.transport.as_ref(), "POST", &url, headers, &body)? {
            (200 | 202, _) => Ok(()),
            (status, body) => Err(format!("answered `{status}`: {}", String::from_utf8_lossy(&body))),
        }
    }
}

/// Serve `GET` and `POST /awake/:token` for `relay` on `listener`, one thread per request.
pub(crate) fn serve_relay(relay: Arc<Relay>, listener: TcpListener) {
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let relay = relay.clone();
            std::thread::spawn(move || {
                if let Err(e) = answer_relay(&relay, &stream) {
                    eprintln!("relay: {e:#}");
                }
            });
        }
    });
}

fn answer_relay(relay: &Relay, stream: &TcpStream) -> Result<()> {
    let r = read_request(stream, STEP_RESULT_CAP)?;
    let Some(token) = r.path.strip_prefix("/awake/").filter(|t| !t.is_empty() && !t.contains('/')) else {
        return respond(stream, 404, &json!({ "error": format!("no route `{}`; a worker reaches `/awake/:token`", r.path) }));
    };
    let header = |k: &str| r.headers.get(k).cloned();
    let answer = match r.method.as_str() {
        "GET" => relay.heartbeat(token, &header).map(|attempt| json!({ "attempt": attempt })),
        "POST" => relay.callback(token, &header, &r.body).map(|outcome| json!({ "recorded": outcome })),
        _ => return respond(stream, 405, &json!({ "error": "a heartbeat is `GET` and a callback `POST`" })),
    };
    match answer {
        Ok(body) => respond(stream, 200, &body),
        Err(refusal) => {
            eprintln!("relay: {refusal}");
            respond(stream, refusal.status(), &json!({ "error": refusal.to_string() }))
        }
    }
}

/// How a worker runs one step.
struct WorkerConfig {
    exe: PathBuf,
    project: String,
    declaration: Option<PathBuf>,
    key: Vec<u8>,
    /// The `[control] relay` every callback must name.
    relay: String,
    transport: Arc<dyn Transport>,
}

/// `pipeline worker --listen <addr>`: take submitted steps, run each, heartbeat and call back.
pub(crate) fn serve_worker(project: &ProjectArgs, declaration: Option<PathBuf>, listen: &str) -> Result<()> {
    let located = project.locate(declaration.clone())?;
    let text = if located.declaration.exists() { std::fs::read_to_string(&located.declaration)? } else { String::new() };
    let Some(relay) = crate::cadence::declared_relay(&text)? else {
        bail!("a worker calls back only to its relay: set `[control] relay` to the URL `serve` binds");
    };
    let config = Arc::new(WorkerConfig {
        exe: std::env::current_exe()?,
        project: located.project.name.clone(),
        declaration,
        key: worker_key()?,
        relay: relay.as_str().trim_end_matches('/').to_string(),
        transport: system(),
    });
    let listener = TcpListener::bind(listen)?;
    eprintln!("worker on http://{}/submit", listener.local_addr()?);
    let jobs: Arc<Mutex<BTreeMap<String, String>>> = Arc::default();
    for stream in listener.incoming().flatten() {
        let (config, jobs) = (config.clone(), jobs.clone());
        std::thread::spawn(move || {
            if let Err(e) = answer_submit(&config, &jobs, &stream) {
                eprintln!("worker: {e:#}");
            }
        });
    }
    Ok(())
}

fn answer_submit(config: &Arc<WorkerConfig>, jobs: &Mutex<BTreeMap<String, String>>, stream: &TcpStream) -> Result<()> {
    let r = read_request(stream, 64 * 1024)?;
    if r.path != "/submit" {
        return respond(stream, 404, &json!({ "error": format!("no route `{}`; a step is `POST /submit`", r.path) }));
    }
    if r.method != "POST" {
        return respond(stream, 405, &json!({ "error": "a step is `POST /submit`" }));
    }
    let s: Submission = match serde_json::from_slice(&r.body) {
        Ok(s) => s,
        Err(e) => return respond(stream, 422, &json!({ "error": format!("a submission body: {e}") })),
    };
    let header = |k: &str| r.headers.get(k).cloned();
    let now = contextful_core::ports::Clock::now(&crate::clock::SystemClock);
    if let Err(e) = admit_submission(&config.key, &config.relay, &s.callback, header, &r.body, now) {
        eprintln!("worker: {e}");
        return respond(stream, e.status(), &json!({ "error": e.to_string() }));
    }
    let handle = format!("{}-{}-a{}", s.run, s.step, s.attempt);
    // A repeated idempotency key answers the existing job and starts nothing.
    {
        let mut jobs = jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = jobs.get(&s.idempotency_key) {
            return respond(stream, 200, &json!({ "handle": existing }));
        }
        jobs.insert(s.idempotency_key.clone(), handle.clone());
    }
    respond(stream, 202, &json!({ "handle": handle }))?;
    let config = config.clone();
    std::thread::spawn(move || run_step(&config, &s, &handle));
    Ok(())
}

fn signed_headers(key: &[u8], s: &Submission) -> Vec<(String, String)> {
    let signed = Signed { run: s.run.clone(), step: s.step.clone(), attempt: s.attempt, timestamp: unix_now() };
    signed.headers(key).into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn run_step(config: &WorkerConfig, s: &Submission, handle: &str) {
    let Ok(callback) = Url::parse(&s.callback) else {
        eprintln!("worker: step {} of {}: callback `{}` is not a URL", s.step, s.run, s.callback);
        return;
    };
    eprintln!("worker: step {} of {}: attempt {} started", s.step, s.run, s.attempt);
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let beat = {
        let (done, key, transport, callback, s) = (done.clone(), config.key.clone(), config.transport.clone(), callback.clone(), s.clone());
        std::thread::spawn(move || {
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                match send(transport.as_ref(), "GET", &callback, signed_headers(&key, &s), &[]) {
                    Ok((200, _)) => {}
                    Ok((status, body)) => eprintln!("worker: heartbeat `{status}`: {}", String::from_utf8_lossy(&body)),
                    Err(e) => eprintln!("worker: heartbeat: {e}"),
                }
                for _ in 0..HEARTBEAT_BEAT_SECS * 10 {
                    if done.load(std::sync::atomic::Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        })
    };
    let mut cmd = std::process::Command::new(&config.exe);
    cmd.args(["pipeline", "run", &s.step, "--applied", &s.version.to_string(), "--project", &config.project, "--run-id", handle]);
    if let Some(d) = &config.declaration {
        cmd.arg("--declaration").arg(d);
    }
    let outcome = match cmd.output() {
        Ok(out) if out.status.success() => StepOutcome::Done(StepResult::CatalogRow(handle.to_string())),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            StepOutcome::Failed { failed: stderr.lines().last().unwrap_or("the step failed").to_string() }
        }
        Err(e) => StepOutcome::Failed { failed: format!("starting `{}`: {e}", config.exe.display()) },
    };
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = beat.join();
    match send(config.transport.as_ref(), "POST", &callback, signed_headers(&config.key, s), &outcome.encode()) {
        Ok((status, body)) => eprintln!("worker: step {} of {}: attempt {} callback `{status}`: {}", s.step, s.run, s.attempt, String::from_utf8_lossy(&body)),
        Err(e) => eprintln!("worker: step {} of {}: callback: {e}", s.step, s.run),
    }
}
