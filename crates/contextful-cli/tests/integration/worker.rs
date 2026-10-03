//! `pipeline serve` dispatching onto remote workers through the relay, and `pipeline worker`.

use crate::pipeline::{cf, ok, project, Vendor};
use contextful_core::surface::worker::{sign_submission, Signed, StepOutcome, StepResult, HEARTBEAT_LAPSE_SECS, SIGNATURE_HEADER, TIMESTAMP_HEADER, WORKER_KEY_VAR};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const KEY: &str = "a-deployment-worker-key";

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A child of the binary in `dir`, its stderr collected line by line.
struct Proc {
    child: Child,
    lines: Arc<Mutex<Vec<String>>>,
}

impl Proc {
    fn start(dir: &Path, args: &[&str]) -> Proc {
        let mut child = Command::new(env!("CARGO_BIN_EXE_contextful"))
            .args(args)
            .current_dir(dir)
            .env(WORKER_KEY_VAR, KEY)
            .env_remove("CONTEXTFUL_NODE_ID")
            .env_remove("CONTEXTFUL_SECRETS_BACKEND")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let lines: Arc<Mutex<Vec<String>>> = Arc::default();
        let (sink, err) = (lines.clone(), child.stderr.take().unwrap());
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                sink.lock().unwrap().push(line);
            }
        });
        Proc { child, lines }
    }

    fn log(&self) -> String {
        self.lines.lock().unwrap().join("\n")
    }

    fn wait_for(&self, needle: &str, within: Duration) {
        let deadline = Instant::now() + within;
        while !self.lines.lock().unwrap().iter().any(|l| l.contains(needle)) {
            assert!(Instant::now() < deadline, "no `{needle}`:\n{}", self.log());
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn count(&self, needle: &str) -> usize {
        self.lines.lock().unwrap().iter().filter(|l| l.contains(needle)).count()
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One request a fake worker took: the submission body.
fn take_submission(listener: &TcpListener) -> serde_json::Value {
    let (stream, _) = listener.accept().unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut length = 0;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).unwrap() == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                length = v.trim().parse().unwrap();
            }
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let mut stream = stream;
    write!(stream, "HTTP/1.1 202 Accepted\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").unwrap();
    serde_json::from_slice(&body).unwrap()
}

/// Send `method` to the step's callback URL signed for `attempt`, answering status and body.
fn signed(method: &str, callback: &str, s: &serde_json::Value, attempt: u32, body: &[u8]) -> (u16, String) {
    let url = callback.strip_prefix("http://").unwrap();
    let (host, path) = url.split_once('/').unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let fields = Signed { run: s["run"].as_str().unwrap().into(), step: s["step"].as_str().unwrap().into(), attempt, timestamp: now };
    let mut stream = TcpStream::connect(host).unwrap();
    let mut head = format!("{method} /{path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    for (k, v) in fields.headers(KEY.as_bytes()) {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    let status = answer.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, answer.split("\r\n\r\n").nth(1).unwrap_or_default().to_string())
}

/// `[control] workers` lists worker URLs and `[control] relay` the URL whose `/awake/:token` route `serve`
/// binds; with workers listed, each landing step posts to a worker's `POST /submit`, keyed by run, step and
/// attempt.
// The relay times the lapse on the system clock, so this test waits out one real lapse.
// spec: surface.dispatch.worker-target@9daa7575
#[test]
fn a_killed_worker_s_step_moves_once_and_its_late_callback_is_rejected() {
    // The vendor holds attempt 2's pull until the late callback has been answered.
    let (released, hit) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    let (gate, entered) = (released.clone(), hit.clone());
    let vendor = Vendor::start(move |_| {
        entered.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(60);
        while !gate.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        (200, "[{\"id\":\"a\"}]".into())
    });
    let doomed = TcpListener::bind("127.0.0.1:0").unwrap();
    let (live, relay) = (free_port(), free_port());
    let doomed_url = format!("http://127.0.0.1:{}", doomed.local_addr().unwrap().port());
    let dir = project(&format!(
        "site_id = \"site-a\"\n\n[control]\nworkers = [\"{doomed_url}\", \"http://127.0.0.1:{live}\"]\nrelay = \"http://127.0.0.1:{relay}\"\n\n[[pipeline]]\nid = \"orders\"\nschedule = \"every 1h\"\ntables = [\"items\"]\n[pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"{}\" }}\n",
        vendor.url("/v1/orders")
    ));
    ok(&cf(dir.path(), &["pipeline", "import", "--project", "research"]));
    let worker = Proc::start(dir.path(), &["pipeline", "worker", "--project", "research", "--listen", &format!("127.0.0.1:{live}")]);
    worker.wait_for("worker on http://", Duration::from_secs(30));
    let mut serve = Proc::start(dir.path(), &["pipeline", "serve", "--cycle", "--project", "research"]);

    // Attempt 1 lands on the doomed worker, which beats once and dies.
    let first = take_submission(&doomed);
    assert_eq!((first["step"].as_str(), first["attempt"].as_u64()), (Some("orders"), Some(1)));
    let callback = first["callback"].as_str().unwrap().to_string();
    assert_eq!(signed("GET", &callback, &first, 1, b"").0, 200, "a heartbeat from the current attempt");
    drop(doomed);
    let killed = Instant::now();

    // After the lapse the step moves to the live worker under attempt 2.
    serve.wait_for(&format!("attempt 2 on http://127.0.0.1:{live}"), Duration::from_secs(HEARTBEAT_LAPSE_SECS + 30));
    assert!(killed.elapsed() >= Duration::from_secs(HEARTBEAT_LAPSE_SECS - 1), "moved only after the lapse");
    worker.wait_for("attempt 2 started", Duration::from_secs(30));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !hit.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "attempt 2 pulls:\n{}", worker.log());
        std::thread::sleep(Duration::from_millis(50));
    }

    // The killed worker's late attempt-1 callback is rejected and changes no step.
    let stale = StepOutcome::Done(StepResult::CatalogRow("stale".into())).encode();
    let (status, body) = signed("POST", &callback, &first, 1, &stale);
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("DispatchCallbackRejected") && body.contains("superseded by attempt 2"), "{body}");
    released.store(true, Ordering::SeqCst);

    let out = serve.child.wait_with_output_ref();
    assert!(out.0.success(), "{}", serve.log());
    let answer: serde_json::Value = serde_json::from_str(&out.1).unwrap();
    assert_eq!(answer["fired"], serde_json::json!(["orders"]), "{}", serve.log());
    assert_eq!(serve.count("attempt 2 on"), 1, "rescheduled once:\n{}", serve.log());
    assert_eq!(serve.count("attempt 3 on"), 0, "{}", serve.log());
    assert!(vendor.targets().iter().all(|t| t == "/v1/orders"), "{:?}", vendor.targets());
    assert!(worker.log().contains("attempt 2 callback `200`"), "{}", worker.log());
}

trait WaitRef {
    fn wait_with_output_ref(&mut self) -> (std::process::ExitStatus, String);
}

impl WaitRef for Child {
    fn wait_with_output_ref(&mut self) -> (std::process::ExitStatus, String) {
        let mut stdout = String::new();
        self.stdout.take().unwrap().read_to_string(&mut stdout).unwrap();
        (self.wait().unwrap(), stdout)
    }
}

/// Heartbeats and callbacks sign under the key `CONTEXTFUL_WORKER_KEY` holds; `serve` listing workers without
/// a relay or without that key refuses at startup and binds nothing.
// spec: surface.dispatch.worker-key@806a0be1
#[test]
fn workers_need_a_relay_and_a_key() {
    let dir = project("site_id = \"site-a\"\n\n[control]\nworkers = [\"http://127.0.0.1:9\"]\n");
    let out = cf(dir.path(), &["pipeline", "serve", "--cycle", "--project", "research"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("[control] relay"), "{}", String::from_utf8_lossy(&out.stderr));
    let dir = project(&format!("site_id = \"site-a\"\n\n[control]\nworkers = [\"http://127.0.0.1:9\"]\nrelay = \"http://127.0.0.1:{}\"\n", free_port()));
    let out = cf(dir.path(), &["pipeline", "serve", "--cycle", "--project", "research"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains(WORKER_KEY_VAR), "{}", String::from_utf8_lossy(&out.stderr));
}

/// Post `body` to a worker's `POST /submit` with `headers`, answering status and body.
fn submit(worker: &str, headers: &[(&str, String)], body: &[u8]) -> (u16, String) {
    let mut stream = TcpStream::connect(worker).unwrap();
    let mut head = format!("POST /submit HTTP/1.1\r\nHost: {worker}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    let status = answer.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, answer.split("\r\n\r\n").nth(1).unwrap_or_default().to_string())
}

/// A worker admits a `POST /submit` only when it carries an HMAC under the worker key over its timestamp and
/// body, within {{surface.dispatch.callback-skew}}, naming a callback under its own `[control] relay`; any
/// other submission raises `DispatchSubmitRejected` and starts nothing.
// spec: surface.dispatch.submit-signed@9fd7b101
#[test]
fn a_worker_runs_only_signed_submissions_calling_back_to_its_relay() {
    // A worker with no relay to call back to refuses to start.
    let dir = project("site_id = \"site-a\"\n");
    let out = cf(dir.path(), &["pipeline", "worker", "--project", "research", "--listen", &format!("127.0.0.1:{}", free_port())]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("[control] relay"), "{}", String::from_utf8_lossy(&out.stderr));

    let relay = format!("http://127.0.0.1:{}", free_port());
    let dir = project(&format!("site_id = \"site-a\"\n\n[control]\nworkers = [\"http://127.0.0.1:9\"]\nrelay = \"{relay}\"\n"));
    let listen = format!("127.0.0.1:{}", free_port());
    let worker = Proc::start(dir.path(), &["pipeline", "worker", "--project", "research", "--listen", &listen]);
    worker.wait_for("worker on http://", Duration::from_secs(30));
    let body = |callback: &str| {
        serde_json::to_vec(&serde_json::json!({
            "run": "orders-1", "step": "orders", "attempt": 1, "version": 1, "callback": callback, "idempotency_key": "orders-1/orders/1"
        }))
        .unwrap()
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let sign = |key: &[u8], ts: i64, body: &[u8]| vec![(TIMESTAMP_HEADER, ts.to_string()), (SIGNATURE_HEADER, sign_submission(key, ts, body))];
    let ours = body(&format!("{relay}/awake/0a1b2c"));
    let theirs = body("http://127.0.0.1:9/awake/0a1b2c");
    for (why, headers, body) in [
        ("unsigned", vec![], ours.clone()),
        ("another key", sign(b"not-the-key", now, &ours), ours.clone()),
        ("stale", sign(KEY.as_bytes(), now - 301, &ours), ours.clone()),
        ("a foreign callback", sign(KEY.as_bytes(), now, &theirs), theirs.clone()),
    ] {
        let (status, answer) = submit(&listen, &headers, &body);
        assert_eq!(status, 401, "{why}: {answer}");
        assert!(answer.contains("DispatchSubmitRejected"), "{why}: {answer}");
    }
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(worker.count("started"), 0, "{}", worker.log());
}
