//! A cold `serve --http` listening while its snapshot set hydrates from the bucket.
#![cfg(unix)]

use super::{file_sync, issue, project, seed, AUD};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

/// A served process killed when the test ends, pass or fail, and its stderr lines.
struct Served {
    child: Child,
    lines: Receiver<String>,
    addr: String,
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Served {
    /// The next stderr line containing `needle`, waiting at most `within`.
    fn line(&self, needle: &str, within: Duration) -> Option<String> {
        let deadline = Instant::now() + within;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.lines.recv_timeout(left) {
                Ok(line) if line.contains(needle) => return Some(line),
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
        None
    }
}

/// Start `contextful serve --http` on `dir` and wait at most `within` for it to listen.
fn serve_within(dir: &Path, public: &str, within: Duration) -> Served {
    let mut child = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["serve", "--http", "127.0.0.1:0", "--project", "research", "--public-key", public, "--audience", AUD, "--max-in-flight", "4"])
        .current_dir(dir)
        .env_remove("CONTEXTFUL_NODE_ID")
        .env_remove("CONTEXTFUL_ISSUER_PUBKEY")
        .env_remove("CONTEXTFUL_AUDIENCE")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let err = BufReader::new(child.stderr.take().unwrap());
    let (send, lines) = channel();
    std::thread::spawn(move || {
        for line in err.lines().map_while(Result::ok) {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    let mut served = Served { child, lines, addr: String::new() };
    let listening = served.line("listening on http://", within);
    let Some(addr) = listening.as_deref().and_then(|l| l.trim().strip_prefix("listening on http://")).and_then(|a| a.strip_suffix("/mcp")) else {
        panic!("`serve` did not listen within {within:?}");
    };
    served.addr = addr.to_string();
    served
}

/// One request's status and body.
fn exchange(addr: &str, method: &str, target: &str, token: Option<&str>, body: &str) -> (u16, String) {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    write!(s, "{method} {target} HTTP/1.1\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    (head.split(' ').nth(1).unwrap().parse().unwrap(), body.to_string())
}

fn hydration(addr: &str) -> Value {
    let (status, body) = exchange(addr, "GET", "/health", None, "");
    assert_eq!(status, 200, "{body}");
    serde_json::from_str::<Value>(&body).unwrap()["hydration"].clone()
}

fn query(addr: &str, token: &str) -> (u16, String) {
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "context.query", "arguments": { "sql": "SELECT id FROM filings" } } }).to_string();
    exchange(addr, "POST", "/mcp", Some(token), &body)
}

/// Every object file under `dir`.
fn objects(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() { stack.push(path) } else { out.push(path) }
        }
    }
    out
}

/// A cold retrieval container meets {{topology.publish-hostname.container-readiness}}; a container holding no snapshot adds a whole-store pull before its first answer.
// spec: read.cache.cold-start@791b106f
#[test]
fn a_cold_http_server_is_ready_within_8_s_and_answers_once_its_pull_lands() {
    let bucket = tempfile::tempdir().unwrap();
    let _a = seed(&file_sync(bucket.path(), ""), &[]);
    // The bucket's data object becomes a pipe, so the pull holds until the test feeds it.
    let part = objects(bucket.path()).into_iter().find(|p| p.extension().is_some_and(|e| e == "parquet")).expect("a pushed data object");
    let bytes = std::fs::read(&part).unwrap();
    std::fs::remove_file(&part).unwrap();
    assert!(Command::new("mkfifo").arg(&part).status().unwrap().success());

    let cold = project("ingest-h", &file_sync(bucket.path(), "pull_before_run = true\n"));
    let (public, token) = issue(cold.path());
    let started = Instant::now();
    let served = serve_within(cold.path(), &public, Duration::from_secs(8));

    // Ready within 8 s while the pull waits on the bucket; reads hold until it lands.
    assert_eq!(hydration(&served.addr), "warming");
    assert!(started.elapsed() < Duration::from_secs(8), "readiness took {:?}", started.elapsed());
    let (status, body) = query(&served.addr, &token);
    assert_eq!(status, 503, "{body}");

    std::fs::OpenOptions::new().write(true).open(&part).unwrap().write_all(&bytes).unwrap();
    let mapped = served.line("hydrated:", Duration::from_secs(60)).expect("the hydrator reports the mapped snapshot set");
    assert!(mapped.contains("files: 1,"), "{mapped}");
    let deadline = Instant::now() + Duration::from_secs(10);
    while hydration(&served.addr) != "complete" {
        assert!(Instant::now() < deadline, "hydration never completed");
        std::thread::sleep(Duration::from_millis(50));
    }

    // The first answer reads the pulled, mapped rows; the bucket is out of the query path.
    drop(bucket);
    let (status, body) = query(&served.addr, &token);
    assert_eq!(status, 200, "{body}");
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["result"]["structuredContent"]["rows"], json!([["1"]]), "{body}");
}

/// A server whose store is already on local disk still binds before it maps: a data file
/// that cannot be read yet holds hydration, not readiness.
#[test]
fn a_local_store_is_ready_within_8_s_while_its_mapping_waits_on_the_disk() {
    let bucket = tempfile::tempdir().unwrap();
    let local = seed(&file_sync(bucket.path(), ""), &[]);
    // The landed data file becomes a pipe, so mapping it holds until the test opens it.
    let part = objects(&local.path().join(super::STORE)).into_iter().find(|p| p.extension().is_some_and(|e| e == "parquet")).expect("a landed data file");
    std::fs::remove_file(&part).unwrap();
    assert!(Command::new("mkfifo").arg(&part).status().unwrap().success());

    let (public, _token) = issue(local.path());
    let started = Instant::now();
    let served = serve_within(local.path(), &public, Duration::from_secs(8));
    assert!(started.elapsed() < Duration::from_secs(8), "readiness took {:?}", started.elapsed());
    assert_eq!(hydration(&served.addr), "warming");

    // Opening the pipe's write end lets the mapping finish.
    drop(std::fs::OpenOptions::new().write(true).open(&part).unwrap());
    served.line("hydrated:", Duration::from_secs(60)).expect("the hydrator reports the mapped snapshot set");
    assert_eq!(hydration(&served.addr), "complete");
}

/// A pull whose endpoint resolves but whose transfer fails stops the listening server.
#[test]
fn a_failed_transfer_after_listening_stops_the_server() {
    let bucket = tempfile::tempdir().unwrap();
    let _a = seed(&file_sync(bucket.path(), ""), &[]);
    // The bucket's data object becomes a directory, which no transfer can read.
    let part = objects(bucket.path()).into_iter().find(|p| p.extension().is_some_and(|e| e == "parquet")).expect("a pushed data object");
    std::fs::remove_file(&part).unwrap();
    std::fs::create_dir(&part).unwrap();

    let cold = project("ingest-h", &file_sync(bucket.path(), "pull_before_run = true\n"));
    let (public, _token) = issue(cold.path());
    let mut served = serve_within(cold.path(), &public, Duration::from_secs(8));

    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = served.child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "the server kept running after its transfer failed");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(!status.success(), "a failed transfer exits nonzero: {status}");
    let mut rest = Vec::new();
    while let Ok(line) = served.lines.recv_timeout(Duration::from_secs(1)) {
        rest.push(line);
    }
    assert!(!rest.iter().any(|l| l.starts_with("hydrated:")), "a failed transfer never completes hydration: {rest:?}");
    assert!(rest.iter().any(|l| l.contains("pull_before_run")), "the failure names the pull: {rest:?}");
}
