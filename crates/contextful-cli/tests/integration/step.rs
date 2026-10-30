//! `contextful pipeline step` through the built binary (`run.orchestrate`): one provider step per
//! table pull or seed chunk, against a loopback vendor that pages, throttles and hangs on demand.
#![cfg(feature = "data-plane")]

use crate::pipeline::{cf, ok, project, stderr};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const T0: &str = "2030-01-01T00:00:00Z";
/// Past the 30 s owner lease of an attempt opened at [`T0`].
const T1: &str = "2030-01-01T00:05:00Z";

/// A loopback vendor. `/exports/items?page=N` serves four pages of two rows, then an empty page;
/// `/v1/items?page=N` serves one row, then an empty page. While `hang_page` names a page, a GET of
/// that export page is recorded and never answered; while `throttles` is positive, a GET answers
/// `429` with `Retry-After: 1` and counts it down. Every request is recorded as `<method> <target>`.
struct Paged {
    port: u16,
    requests: Arc<Mutex<Vec<String>>>,
    hang_page: Arc<AtomicUsize>,
    throttles: Arc<AtomicUsize>,
    refuse: Arc<AtomicBool>,
}

impl Paged {
    fn start() -> Paged {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (requests, hang_page, throttles, refuse): (Arc<Mutex<Vec<String>>>, Arc<AtomicUsize>, Arc<AtomicUsize>, Arc<AtomicBool>) = Default::default();
        let (seen, hang, throttle, refusing) = (requests.clone(), hang_page.clone(), throttles.clone(), refuse.clone());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let (seen, hang, throttle, refusing) = (seen.clone(), hang.clone(), throttle.clone(), refusing.clone());
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let mut parts = line.split_whitespace();
                    let (method, target) = (parts.next().unwrap_or_default().to_string(), parts.next().unwrap_or_default().to_string());
                    loop {
                        let mut h = String::new();
                        if reader.read_line(&mut h).unwrap() == 0 || h.trim().is_empty() {
                            break;
                        }
                    }
                    seen.lock().unwrap().push(format!("{method} {target}"));
                    let page: usize = target.split("page=").nth(1).and_then(|p| p.parse().ok()).unwrap_or(1);
                    if method == "GET" && target.starts_with("/exports/") && hang.load(Ordering::SeqCst) == page {
                        while hang.load(Ordering::SeqCst) == page {
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        return;
                    }
                    if method == "GET" && throttle.load(Ordering::SeqCst) > 0 {
                        throttle.fetch_sub(1, Ordering::SeqCst);
                        let _ = write!(stream, "HTTP/1.1 429 X\r\nRetry-After: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        return;
                    }
                    if method == "GET" && refusing.load(Ordering::SeqCst) {
                        let _ = write!(stream, "HTTP/1.1 400 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        return;
                    }
                    let body = if target.starts_with("/exports/") && page <= 4 {
                        format!(r#"[{{"id":"e{page}a","updated_at":{}}},{{"id":"e{page}b","updated_at":{}}}]"#, page * 10, page * 10 + 1)
                    } else if target.starts_with("/v1/") && page == 1 {
                        r#"[{"id":"live","updated_at":150}]"#.to_string()
                    } else {
                        "[]".to_string()
                    };
                    let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n", body.len());
                    if method != "HEAD" {
                        let _ = stream.write_all(body.as_bytes());
                    }
                });
            }
        });
        Paged { port, requests, hang_page, throttles, refuse }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// Every request since the last call.
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.requests.lock().unwrap())
    }
}

/// A pipeline `orders` landing `orders_items` from a paged live feed, seeded from a paged export below 100.
fn seeded(vendor: &Paged) -> tempfile::TempDir {
    project(&format!(
        "[[pipeline]]\nid = \"orders\"\n[[pipeline.tables]]\nname = \"items\"\nprimary_key = [\"id\"]\norder_by = \"updated_at\"\n\
         [pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"{}\", page_param = \"page\" }}\n\
         [pipeline.seed]\nbelow = 100\n[pipeline.seed.source]\nname = \"http\"\nconfig = {{ endpoint = \"{}\", page_param = \"page\" }}\n\
         \n[[job]]\nname = \"fold-items\"\nschedule = \"0 3 * * *\"\nkind = \"fold\"\ntarget = \"orders_items\"\n",
        vendor.url("/v1/{table}"),
        vendor.url("/exports/{table}"),
    ))
}

/// A pipeline `feed` landing `feed_items` from the paged live feed alone.
fn live(vendor: &Paged) -> tempfile::TempDir {
    project(&format!(
        "[[pipeline]]\nid = \"feed\"\n[[pipeline.tables]]\nname = \"items\"\n\
         [pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"{}\", page_param = \"page\" }}\n",
        vendor.url("/v1/{table}"),
    ))
}

fn step(dir: &Path, pipeline: &str, unit: &[&str], step: &str, now: &str) -> std::process::Output {
    let mut args = vec!["pipeline", "step", pipeline];
    args.extend_from_slice(unit);
    args.extend_from_slice(&["--run-id", step, "--project", "research", "--site-id", "site-a", "--now", now]);
    cf(dir, &args)
}

fn show(dir: &Path, run: &str) -> serde_json::Value {
    serde_json::from_str(&ok(&cf(dir, &["run", "show", run, "--project", "research"]))).unwrap()
}

/// Spawn chunk 0's step of `orders` as `wf-1` at [`T0`] and kill it once the vendor holds export
/// page 3, so pages 1 and 2 stand recorded and page 3 does not.
fn kill_after_half(dir: &Path, vendor: &Paged) {
    vendor.hang_page.store(3, Ordering::SeqCst);
    let mut child = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["pipeline", "step", "orders", "--chunk", "0", "--run-id", "wf-1", "--project", "research", "--site-id", "site-a", "--now", T0])
        .current_dir(dir)
        .env_remove("CONTEXTFUL_NODE_ID")
        .env_remove("CONTEXTFUL_SECRETS_BACKEND")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !vendor.requests.lock().unwrap().iter().any(|r| r == "GET /exports/items?page=3") {
        assert!(Instant::now() < deadline, "the chunk step never reached export page 3");
        std::thread::sleep(Duration::from_millis(20));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    vendor.hang_page.store(0, Ordering::SeqCst);
}

/// The committed Parquet parts of `run`'s commit in `orders_items`, as `(relative path, bytes)`.
fn parts(dir: &Path, run: &str) -> Vec<(String, Vec<u8>)> {
    let listed = ok(&cf(dir, &["context", "files", "orders_items", "--project", "research"]));
    let prefix = format!("tables/orders_items/data/runs/{run}/");
    listed
        .lines()
        .filter(|f| f.starts_with(&prefix))
        .map(|f| (f.to_string(), std::fs::read(dir.join(".contextful/context/research").join(f)).unwrap()))
        .collect()
}

/// A table step runs one table pull and prints its commit pointer; a replay of the completed step prints that
/// pointer and sends no request.
// spec: run.orchestrate.provider-step@82706d91
// spec: run.orchestrate.step-command@64bf0483
#[test]
fn a_table_step_prints_its_commit_pointer_and_its_replay_re_enters_nothing() {
    let vendor = Paged::start();
    let dir = live(&vendor);
    let first = ok(&step(dir.path(), "feed", &["--table", "items"], "wf-1", T0));
    assert_eq!(first, "feed_items: commit wf-1");
    assert_eq!(vendor.take(), ["GET /v1/items?page=1", "GET /v1/items?page=2"]);
    assert_eq!(show(dir.path(), "wf-1")["status"], "success");

    let replayed = ok(&step(dir.path(), "feed", &["--table", "items"], "wf-1", T1));
    assert_eq!(replayed, first, "a replay prints the recorded pointer");
    assert!(vendor.take().is_empty(), "a replay of a completed step re-enters nothing");
    assert!(!cf(dir.path(), &["run", "show", "wf-1.replay-2", "--project", "research"]).status.success(), "a replay opens no attempt");

    let unknown = step(dir.path(), "feed", &["--table", "nope"], "wf-2", T1);
    assert!(!unknown.status.success() && stderr(&unknown).contains("`nope`"), "{}", stderr(&unknown));
    let chunkless = step(dir.path(), "feed", &["--chunk", "0"], "wf-3", T1);
    assert!(!chunkless.status.success() && stderr(&chunkless).contains("no chunk plan"), "{}", stderr(&chunkless));
}

/// A chunk step killed after half its pages leaves its pages recorded; its replay resumes from them, sends no
/// recorded page again, and lands parts byte-identical to an uninterrupted engine-runner fire.
// spec: run.orchestrate.resume@dcf151ff
#[test]
fn a_chunk_step_killed_after_half_its_pages_resumes_and_lands_the_runners_bytes() {
    let vendor = Paged::start();
    let dir = seeded(&vendor);
    kill_after_half(dir.path(), &vendor);
    assert_eq!(vendor.take(), ["HEAD /exports/items", "GET /exports/items?page=1", "GET /exports/items?page=2", "GET /exports/items?page=3"]);

    let resumed = ok(&step(dir.path(), "orders", &["--chunk", "0"], "wf-1", T1));
    assert_eq!(resumed, "orders_items chunk 0: commit wf-1.replay-2.seed-0");
    assert_eq!(
        vendor.take(),
        ["HEAD /exports/items", "GET /exports/items?page=3", "GET /exports/items?page=4", "GET /exports/items?page=5"],
        "the replay sends no recorded page"
    );
    assert_eq!(show(dir.path(), "wf-1.seed-0")["status"], "partial_failure", "the killed attempt reaps once its lease lapses");
    assert_eq!(show(dir.path(), "wf-1.replay-2.seed-0")["rows"], 8);
    let replayed = ok(&step(dir.path(), "orders", &["--chunk", "0"], "wf-1", T1));
    assert_eq!(replayed, resumed, "a replay of the completed chunk prints its pointer");
    assert!(vendor.take().is_empty(), "and sends nothing, the fingerprint probe included");

    // The engine runner's uninterrupted fire names the chunk's run as the resumed attempt does.
    let runner = seeded(&vendor);
    ok(&cf(runner.path(), &["pipeline", "run", "orders", "--project", "research", "--run-id", "wf-1.replay-2", "--site-id", "site-a", "--now", T1]));
    let (stepped, ran) = (parts(dir.path(), "wf-1.replay-2.seed-0"), parts(runner.path(), "wf-1.replay-2.seed-0"));
    assert!(!stepped.is_empty(), "the resumed chunk committed parts");
    assert_eq!(stepped, ran, "the resumed chunk's parts match the uninterrupted fire byte for byte");

    // The table step reads the committed seed and pulls live.
    let table = ok(&step(dir.path(), "orders", &["--table", "items"], "wf-2", T1));
    assert_eq!(table, "orders_items: commit wf-2");
}

/// A step opens its attempt's owner lease and releases it on return; the replay of a killed step refuses while
/// the killed attempt's lease runs and resumes once it lapses.
// spec: run.orchestrate.step-lease@77b12a45
#[test]
fn a_step_holds_its_owner_lease_only_while_it_runs() {
    let vendor = Paged::start();
    let dir = live(&vendor);
    ok(&step(dir.path(), "feed", &["--table", "items"], "wf-1", T0));
    let row = show(dir.path(), "wf-1");
    assert_eq!(row["status"], "success");
    assert!(row["owner"].is_null(), "a returned step holds no owner lease: {row}");

    let seeded_dir = seeded(&vendor);
    kill_after_half(seeded_dir.path(), &vendor);
    assert_eq!(show(seeded_dir.path(), "wf-1.seed-0")["status"], "running", "the killed attempt still holds its lease");
    let early = step(seeded_dir.path(), "orders", &["--chunk", "0"], "wf-1", T0);
    assert!(!early.status.success(), "a replay inside the killed attempt's lease refuses");
    assert!(stderr(&early).contains("wf-1.seed-0"), "naming the attempt: {}", stderr(&early));
    assert!(!cf(seeded_dir.path(), &["run", "show", "wf-1.replay-2.seed-0", "--project", "research"]).status.success(), "and opens no attempt");
    vendor.take();
    let late = ok(&step(seeded_dir.path(), "orders", &["--chunk", "0"], "wf-1", T1));
    assert_eq!(late, "orders_items chunk 0: commit wf-1.replay-2.seed-0");
    assert!(show(seeded_dir.path(), "wf-1.replay-2.seed-0")["owner"].is_null());
}

/// A throttled pull retries on the engine's schedule inside its step, under one attempt; a refused pull closes
/// the step failed and the step exits with the failure.
// spec: run.orchestrate.one-retry-layer@46c3ab82
#[test]
fn a_throttled_pull_retries_inside_its_step_under_one_attempt() {
    let vendor = Paged::start();
    let dir = live(&vendor);
    vendor.throttles.store(1, Ordering::SeqCst);
    let out = ok(&step(dir.path(), "feed", &["--table", "items"], "wf-1", T0));
    assert_eq!(out, "feed_items: commit wf-1");
    assert_eq!(vendor.take(), ["GET /v1/items?page=1", "GET /v1/items?page=1", "GET /v1/items?page=2"], "the engine retries the throttled page");
    assert_eq!(show(dir.path(), "wf-1")["status"], "success");
    assert!(!cf(dir.path(), &["run", "show", "wf-1.replay-2", "--project", "research"]).status.success(), "one attempt holds the step");

    vendor.refuse.store(true, Ordering::SeqCst);
    let refused = step(dir.path(), "feed", &["--table", "items"], "wf-2", T1);
    assert!(!refused.status.success(), "a failed step exits with its failure");
    assert_eq!(show(dir.path(), "wf-2")["status"], "failed", "the engine closes the step's table");
}
