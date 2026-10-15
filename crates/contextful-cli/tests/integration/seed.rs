//! `run.seed` through the built binary: `pipeline run` drives a declared seed before the live
//! pull, probing its HTTP source with a HEAD, and `seed status` and `seed reset` read and clear it.
#![cfg(feature = "data-plane")]

use crate::pipeline::{cf, ok, project, stderr};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// A loopback vendor serving a bulk export under `/exports/` with a settable `ETag`, and the
/// live feed under `/v1/`, recording each request as `<method> <target>`.
struct Exports {
    port: u16,
    etag: Arc<Mutex<String>>,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Exports {
    fn start() -> Exports {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let etag = Arc::new(Mutex::new("\"v1\"".to_string()));
        let requests: Arc<Mutex<Vec<String>>> = Arc::default();
        let (tag, seen) = (etag.clone(), requests.clone());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
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
                let body = if target.starts_with("/exports/") {
                    r#"[{"id":"a","updated_at":10},{"id":"b","updated_at":20}]"#
                } else {
                    r#"[{"id":"c","updated_at":150}]"#
                };
                seen.lock().unwrap().push(format!("{method} {target}"));
                let etag = tag.lock().unwrap().clone();
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: {etag}\r\nConnection: close\r\n\r\n", body.len());
                if method != "HEAD" {
                    let _ = stream.write_all(body.as_bytes());
                }
            }
        });
        Exports { port, etag, requests }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// Every request since the last call.
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.requests.lock().unwrap())
    }
}

fn seeded(vendor: &Exports) -> tempfile::TempDir {
    project(&format!(
        "[[pipeline]]\nid = \"orders\"\n[[pipeline.tables]]\nname = \"items\"\nprimary_key = [\"id\"]\norder_by = \"updated_at\"\n\
         [pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"{}\" }}\n\
         [pipeline.seed]\nbelow = 100\n[pipeline.seed.source]\nname = \"http\"\nconfig = {{ endpoint = \"{}\" }}\n\
         \n[[job]]\nname = \"fold-items\"\nschedule = \"0 3 * * *\"\nkind = \"fold\"\ntarget = \"orders_items\"\n",
        vendor.url("/v1/{table}"),
        vendor.url("/exports/{table}"),
    ))
}

fn fire(dir: &Path, run: &str) -> std::process::Output {
    cf(dir, &["pipeline", "run", "orders", "--project", "research", "--run-id", run, "--site-id", "site-a", "--now", "2030-01-01T00:00:00Z"])
}

fn status(dir: &Path) -> String {
    ok(&cf(dir, &["seed", "status", "--project", "research"]))
}

/// `pipeline run` loads an uncommitted seed through the seeding loader before the live pull, probing the HTTP seed
/// source with a HEAD; a committed seed whose HEAD answers its stamped fingerprint is never fetched again.
#[test]
fn a_fire_loads_the_seed_once_then_probes_it_with_a_head() {
    let vendor = Exports::start();
    let dir = seeded(&vendor);
    let out = ok(&fire(dir.path(), "r1"));
    assert!(out.contains("orders_items: r1.seed-0 seeded · 2 rows"), "{out}");
    assert!(out.contains("orders_items: r1 success"), "{out}");
    assert_eq!(vendor.take(), ["HEAD /exports/items", "GET /exports/items", "GET /v1/items"], "the seed loads before the live pull");
    let rows = ok(&cf(dir.path(), &["query", "--json", "--project", "research", "SELECT id FROM orders_items ORDER BY id"]));
    let rows: serde_json::Value = serde_json::from_str(&rows).unwrap();
    assert_eq!(rows["rows"], serde_json::json!([["a"], ["b"], ["c"]]), "seeded and live rows share the live table");

    ok(&fire(dir.path(), "r2"));
    assert_eq!(vendor.take(), ["HEAD /exports/items", "GET /v1/items"], "an unchanged seed source is probed, never downloaded");
}

/// `seed status` prints each seeded table's ceiling, commit state and fingerprint match; `seed reset <pipeline>
/// [--table T]` clears the seed's chunk plan and commit stamp, so the next run loads it again.
// spec: run.seed.commands@fa204de3
#[test]
fn seed_status_reads_the_seed_and_seed_reset_clears_it_for_a_reload() {
    let vendor = Exports::start();
    let dir = seeded(&vendor);
    assert_eq!(status(dir.path()), "orders orders_items: ceiling 100 · uncommitted (0 of 0 chunks done) · fingerprint unstamped");
    ok(&fire(dir.path(), "r1"));
    assert_eq!(status(dir.path()), "orders orders_items: ceiling 100 · committed · fingerprint match");

    *vendor.etag.lock().unwrap() = "\"v2\"".to_string();
    assert_eq!(status(dir.path()), "orders orders_items: ceiling 100 · committed · fingerprint moved");
    vendor.take();
    let refused = fire(dir.path(), "r2");
    assert!(!refused.status.success());
    let err = stderr(&refused);
    assert!(err.contains("PipelineSeedSourceChanged") && err.contains("\"v1\"") && err.contains("\"v2\""), "{err}");
    assert_eq!(vendor.take(), ["HEAD /exports/items"], "a moved seed source refuses the table before any pull");

    let unknown = cf(dir.path(), &["seed", "reset", "orders", "--table", "nope", "--project", "research"]);
    assert!(!unknown.status.success() && stderr(&unknown).contains("`nope`"), "{}", stderr(&unknown));
    assert_eq!(ok(&cf(dir.path(), &["seed", "reset", "orders", "--table", "items", "--project", "research"])), "orders_items: seed reset · 1 chunk(s) cleared");
    assert_eq!(status(dir.path()), "orders orders_items: ceiling 100 · uncommitted (0 of 0 chunks done) · fingerprint unstamped");

    let out = ok(&fire(dir.path(), "r3"));
    assert!(out.contains("orders_items: r3.seed-0 seeded · 2 rows"), "{out}");
    assert_eq!(vendor.take(), ["HEAD /exports/items", "GET /exports/items", "GET /v1/items"], "a reset seed reloads");
    assert_eq!(status(dir.path()), "orders orders_items: ceiling 100 · committed · fingerprint match");
}
