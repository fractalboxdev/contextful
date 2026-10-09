//! `contextful export run` through the built binary, against a loopback OTLP/HTTP collector.
#![cfg(feature = "data-plane")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const AUD: &str = "contextful://acme-research";

/// One request the collector received.
#[derive(Debug, Clone)]
struct Received {
    method: String,
    path: String,
    headers: Vec<String>,
    body: serde_json::Value,
}

/// A loopback collector answering every request with the status it currently holds.
struct Collector {
    port: u16,
    status: Arc<AtomicU16>,
    /// While set, a received request waits for its answer.
    held: Arc<AtomicBool>,
    received: Arc<Mutex<Vec<Received>>>,
}

impl Collector {
    fn start() -> Collector {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let status = Arc::new(AtomicU16::new(200));
        let held = Arc::new(AtomicBool::new(false));
        let received: Arc<Mutex<Vec<Received>>> = Arc::default();
        let (answer, seen, hold) = (status.clone(), received.clone(), held.clone());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut parts = line.split_whitespace();
                let (method, path) = (parts.next().unwrap_or_default().to_string(), parts.next().unwrap_or_default().to_string());
                let mut headers = Vec::new();
                let mut length = 0usize;
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap() == 0 || h.trim().is_empty() {
                        break;
                    }
                    let h = h.trim().to_ascii_lowercase();
                    if let Some(v) = h.strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                    headers.push(h);
                }
                let mut body = vec![0u8; length];
                reader.read_exact(&mut body).unwrap();
                let body = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
                let status = answer.load(Ordering::SeqCst);
                seen.lock().unwrap().push(Received { method, path, headers, body });
                while hold.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(10));
                }
                let _ = write!(stream, "HTTP/1.1 {status} X\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}");
            }
        });
        Collector { port, status, held, received }
    }

    fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}/v1/logs", self.port)
    }

    fn answer(&self, status: u16) {
        self.status.store(status, Ordering::SeqCst);
    }

    fn hold(&self, on: bool) {
        self.held.store(on, Ordering::SeqCst);
    }

    /// Wait until the collector has received `n` requests.
    fn await_requests(&self, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while self.received().len() < n {
            assert!(Instant::now() < deadline, "the collector received {} of {n} requests", self.received().len());
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn received(&self) -> Vec<Received> {
        self.received.lock().unwrap().clone()
    }
}

/// The span ids each request carried, in order.
fn span_ids(r: &Received) -> Vec<String> {
    records(r).iter().map(|rec| attribute(rec, "span_id").unwrap()).collect()
}

fn records(r: &Received) -> Vec<serde_json::Value> {
    r.body["resourceLogs"][0]["scopeLogs"][0]["logRecords"].as_array().cloned().unwrap_or_default()
}

fn attribute(record: &serde_json::Value, key: &str) -> Option<String> {
    let a = record["attributes"].as_array()?.iter().find(|a| a["key"] == key)?;
    let v = &a["value"];
    v["stringValue"].as_str().or(v["intValue"].as_str()).map(str::to_string)
}

fn cf(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful")).args(args).envs(env.iter().copied()).current_dir(dir).env_remove("CONTEXTFUL_NODE_ID").output().unwrap()
}

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn err(out: &Output) -> String {
    assert!(!out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn block(endpoint: &str, extra: &str) -> String {
    format!("[[export]]\nname = \"spans-mirror\"\ntable = \"spans\"\nendpoint = \"{endpoint}\"\nsignal = \"logs\"\n{extra}")
}

fn typed_block(endpoint: &str) -> String {
    format!("[[export]]\nname = \"spans-mirror\"\ntable = \"spans\"\nendpoint = \"{endpoint}\"\nformat = \"changes-v1\"\nkey = [\"span_id\"]\nschedule = \"every 30s\"\n")
}

fn typed_events(request: &Received) -> Vec<serde_json::Value> {
    request.body["events"].as_array().cloned().unwrap_or_default()
}

#[test]
fn typed_export_refuses_a_truncated_initial_state() {
    for table_cap in [false, true] {
        let collector = Collector::start();
        let limited = "[[pipeline.tables]]\nname = \"spans\"\n[pipeline.tables.policy.limits]\nmax_rows = 2\n\n";
        let declaration = format!("{}{}", if table_cap { limited } else { "" }, typed_block(&collector.endpoint()));
        let (dir, public, token) = project(&declaration, "spans");
        let p = dir.path();
        let token = if table_cap { token } else {
            ok(&cf(p, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "svc://mirror", "--zone", "on-prem:hq", "--table", "spans", "--max-rows", "2", "--ttl", "600"], &[]))
        };
        land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 3));
        let refused = err(&export(p, &public, &token, &[]));
        assert!(refused.contains("truncated"), "{refused}");
        assert!(collector.received().is_empty());
        let machine = p.join(".contextful/context/research/machine.sqlite");
        let ledger = contextful_sqlite::ExportLedger::open(&machine).unwrap();
        let position = ledger.position("spans-mirror").unwrap();
        assert!(position.pending_publication.is_none());
        assert!(position.ack_sequence.is_none());
        assert!(ledger.state("spans-mirror").unwrap().is_empty());
    }
}

#[test]
fn typed_export_refuses_a_truncated_refresh_without_false_deletions() {
    let collector = Collector::start();
    let (dir, public, token) = project(&typed_block(&collector.endpoint()), "spans");
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 3));
    ok(&export(p, &public, &token, &[]));
    let machine = p.join(".contextful/context/research/machine.sqlite");
    let ledger = contextful_sqlite::ExportLedger::open(&machine).unwrap();
    let before = ledger.position("spans-mirror").unwrap();
    let state = ledger.state("spans-mirror").unwrap();
    drop(ledger);
    let limited = format!("authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"spans\"\n[pipeline.tables.policy.limits]\nmax_rows = 2\n\n{}", typed_block(&collector.endpoint()));
    std::fs::write(p.join("contextful.toml"), limited).unwrap();
    let refused = err(&export(p, &public, &token, &[]));
    assert!(refused.contains("truncated"), "{refused}");
    assert_eq!(collector.received().len(), 1, "a partial state emits no false deletions or completion marker");
    let ledger = contextful_sqlite::ExportLedger::open(&machine).unwrap();
    assert_eq!(ledger.position("spans-mirror").unwrap(), before);
    assert_eq!(ledger.state("spans-mirror").unwrap(), state);
}

#[test]
fn typed_export_delivers_versioned_changes_and_retries_from_machine_cursor() {
    let collector = Collector::start();
    let (dir, public, token) = project(&typed_block(&collector.endpoint()), "spans");
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 2));
    ok(&export(p, &public, &token, &[]));
    let first = typed_events(&collector.received()[0]);
    assert_eq!(first.len(), 3);
    assert_eq!(first[0]["version"], 1);
    assert_eq!(first[0]["kind"], "upsert");
    assert_eq!(first[0]["key"], serde_json::json!({"span_id":"a000"}));
    assert_eq!(first[0]["row"]["duration_ms"], 10);
    assert_eq!(first[2]["kind"], "publication_complete");
    assert_eq!(first[2]["changes"], 2);
    let machine = p.join(".contextful/context/research/machine.sqlite");
    let position = contextful_sqlite::ExportLedger::open(&machine).unwrap().position("spans-mirror").unwrap();
    assert_eq!(position.ack_sequence, Some(2));
    assert!(position.pending_publication.is_none());
    ok(&export(p, &public, &token, &[]));
    assert_eq!(collector.received().len(), 1);

    land(p, "load-2", "2030-01-01T00:01:00Z", &ids("b", 1));
    collector.answer(503);
    let refused = err(&export(p, &public, &token, &[]));
    assert!(refused.contains("ExportDeliveryRefused") && refused.contains("503"), "{refused}");
    let failed = typed_events(&collector.received()[1]);
    let mut pending = contextful_sqlite::ExportLedger::open(&machine).unwrap();
    assert_eq!(pending.position("spans-mirror").unwrap().ack_sequence, Some(2));
    assert_eq!(pending.pending("spans-mirror", 500).unwrap().len(), failed.len());
    drop(pending);
    collector.answer(200);
    ok(&export(p, &public, &token, &[]));
    assert_eq!(typed_events(&collector.received()[2]), failed);
    assert_eq!(contextful_sqlite::ExportLedger::open(&machine).unwrap().position("spans-mirror").unwrap().ack_sequence, Some(4));
}

#[test]
fn encrypted_typed_exports_replay_pending_events_without_cleartext_rows() {
    const CANARY: &str = "sealed-export-row-74-631e";
    let key_var = "CONTEXTFUL_TEST_KEY_74_TYPED_EXPORT";
    unsafe { std::env::set_var(key_var, "0123456789abcdef0123456789abcdef") };
    let collector = Collector::start();
    let (dir, public, token) = project(&typed_block(&collector.endpoint()), "spans");
    let p = dir.path();
    let store_root = p.join(".contextful/context/research");
    std::fs::write(store_root.join("config.toml"), format!("[node]\nid = \"ingest-a\"\n[encryption]\nkey_source = \"env:{key_var}\"\n")).unwrap();
    land(p, "sealed-export-load", "2030-01-01T00:00:00Z", &[CANARY.to_string()]);
    let store = contextful_context::Store::open(p, "research").unwrap();
    let decl = contextful_core::store::declare::TableDecl::named("spans");
    assert_eq!(contextful_context::rows::table_rows(&store, &decl, &["span_id"]).unwrap()[0]["span_id"], CANARY);
    collector.answer(503);
    let refusal = err(&export(p, &public, &token, &[]));
    assert!(refusal.contains("ExportDeliveryRefused"), "{refusal}");
    let refused = typed_events(&collector.received()[0]);
    assert_eq!(refused[0]["row"]["span_id"], CANARY);
    let store = contextful_context::Store::open(p, "research").unwrap();
    let machine = store_root.join("machine.sqlite");
    let mut pending = contextful_sqlite::ExportLedger::open_sealed(&machine, store.file_cipher().unwrap()).unwrap();
    assert_eq!(pending.pending("spans-mirror", 500).unwrap().len(), refused.len());
    drop(pending);
    collector.answer(200);
    ok(&export(p, &public, &token, &[]));
    assert_eq!(typed_events(&collector.received()[1]), refused);
    let position = contextful_sqlite::ExportLedger::open_sealed(&machine, store.file_cipher().unwrap()).unwrap().position("spans-mirror").unwrap();
    assert!(position.pending_publication.is_none());
    assert!(contextful_sqlite::ExportLedger::open(&machine).is_err());
    fn scan(path: &Path) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                assert!(!bytes.windows(CANARY.len()).any(|w| w == CANARY.as_bytes()), "cleartext exported row in {}", path.display());
            }
        }
    }
    scan(&p.join(".contextful"));
}

#[test]
fn typed_export_sends_deletion_after_a_replace_publication() {
    let collector = Collector::start();
    let declaration = format!("[[pipeline.tables]]\nname = \"spans\"\nprimary_key = [\"span_id\"]\nwrite_mode = \"replace\"\n\n{}", typed_block(&collector.endpoint()));
    let (dir, public, token) = project(&declaration, "spans");
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 2));
    ok(&export(p, &public, &token, &[]));
    land(p, "load-2", "2030-01-01T00:01:00Z", &ids("b", 1));
    land(p, "load-3", "2030-01-01T00:02:00Z", &ids("c", 1));
    ok(&export(p, &public, &token, &[]));
    let second = typed_events(&collector.received()[1]);
    assert_eq!(second.iter().filter(|e| e["kind"] == "delete").count(), 2, "{second:?}");
    assert_eq!(second.iter().find(|e| e["kind"] == "upsert").unwrap()["key"], serde_json::json!({"span_id":"c000"}));
    assert!(!second.iter().any(|e| e["key"]["span_id"] == "b000"));
    assert_eq!(second.last().unwrap()["kind"], "publication_complete");
}

#[test]
fn typed_export_republishes_a_changed_read_policy_without_a_new_commit() {
    let collector = Collector::start();
    let (dir, public, token) = project(&typed_block(&collector.endpoint()), "spans");
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 1));
    ok(&export(p, &public, &token, &[]));
    let machine = p.join(".contextful/context/research/machine.sqlite");
    let before = contextful_sqlite::ExportLedger::open(&machine).unwrap().position("spans-mirror").unwrap();

    let declaration = format!("[[pipeline.tables]]\nname = \"spans\"\n[pipeline.tables.policy.columns]\nduration_ms = {{ strategy = \"drop\" }}\n\n{}", typed_block(&collector.endpoint()));
    std::fs::write(p.join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{declaration}")).unwrap();
    ok(&export(p, &public, &token, &[]));

    let received = collector.received();
    assert_eq!(received.len(), 2, "a policy change must produce a new publication");
    let changed = typed_events(&received[1]);
    assert_eq!(changed[0]["kind"], "upsert");
    assert_ne!(changed[0]["publication"], typed_events(&received[0])[0]["publication"]);
    assert!(changed[0]["row"]["duration_ms"].is_null());
    assert_eq!(changed[1]["kind"], "publication_complete");
    assert_eq!(contextful_sqlite::ExportLedger::open(&machine).unwrap().position("spans-mirror").unwrap().source_publication, before.source_publication);
}

#[test]
fn typed_export_rekeys_without_a_new_commit() {
    let collector = Collector::start();
    let (dir, public, token) = project(&typed_block(&collector.endpoint()), "spans");
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 1));
    ok(&export(p, &public, &token, &[]));
    let changed = typed_block(&collector.endpoint()).replace("key = [\"span_id\"]", "key = [\"duration_ms\"]");
    std::fs::write(p.join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{changed}")).unwrap();
    ok(&export(p, &public, &token, &[]));

    let received = collector.received();
    assert_eq!(received.len(), 2, "a key change must produce a new publication");
    let events = typed_events(&received[1]);
    assert_eq!(events.iter().filter(|e| e["kind"] == "delete").count(), 1);
    assert_ne!(events[0]["publication"], typed_events(&received[0])[0]["publication"]);
    assert_eq!(events.iter().filter(|e| e["kind"] == "upsert").count(), 1);
    assert_eq!(events.last().unwrap()["kind"], "publication_complete");
}

#[test]
fn typed_export_marks_a_schema_change_without_a_new_commit() {
    let collector = Collector::start();
    let (dir, public, token) = project(&typed_block(&collector.endpoint()), "spans");
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 1));
    ok(&export(p, &public, &token, &[]));

    let schema_path = p.join(".contextful/context/research/tables/spans/schema.json");
    let mut schema: serde_json::Value = serde_json::from_slice(&std::fs::read(&schema_path).unwrap()).unwrap();
    let fields = schema["fields"].as_array_mut().unwrap();
    let field = fields.iter_mut().find(|f| f["name"] == "duration_ms").unwrap();
    field["nullable"] = serde_json::json!(!field["nullable"].as_bool().unwrap());
    std::fs::write(&schema_path, serde_json::to_vec(&schema).unwrap()).unwrap();

    ok(&export(p, &public, &token, &[]));
    let received = collector.received();
    assert_eq!(received.len(), 2);
    let events = typed_events(&received[1]);
    assert_eq!(events.len(), 1);
    assert_ne!(events[0]["publication"], typed_events(&received[0])[0]["publication"]);
    assert_eq!(events[0]["kind"], "publication_complete");
    assert_eq!(events[0]["changes"], 0);
}

#[test]
fn typed_export_refuses_a_changed_target_under_the_same_name() {
    let first = Collector::start();
    let second = Collector::start();
    let (dir, public, token) = project(&typed_block(&first.endpoint()), "spans");
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 1));
    ok(&export(p, &public, &token, &[]));
    std::fs::write(p.join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{}", typed_block(&second.endpoint()))).unwrap();
    let refused = err(&export(p, &public, &token, &[]));
    assert!(refused.contains("ExportIdentityChanged"), "{refused}");
    assert!(second.received().is_empty());
}

#[test]
fn typed_export_refuses_pending_replay_under_an_ungranted_credential() {
    let collector = Collector::start();
    collector.answer(503);
    let (dir, public, token) = project(&typed_block(&collector.endpoint()), "spans");
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 1));
    assert!(err(&export(p, &public, &token, &[])).contains("ExportDeliveryRefused"));
    let other = ok(&cf(p, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "svc://mirror", "--zone", "on-prem:hq", "--table", "documents", "--ttl", "600"], &[]));
    collector.answer(200);
    let refused = err(&export(p, &public, &other, &[]));
    assert!(refused.contains("ExportIdentityChanged") || refused.contains("EnforceUnknownRelation"), "{refused}");
    assert_eq!(collector.received().len(), 1, "a pending row must not leave under another read grant");
    ok(&export(p, &public, &token, &[]));
    assert_eq!(collector.received().len(), 2, "the original credential can replay the outbox");
    assert!(err(&export(p, &public, &other, &[])).contains("EnforceUnknownRelation"));
    assert_eq!(collector.received().len(), 2, "an ungranted credential cannot silently reuse the acknowledged view");
}

#[test]
fn typed_export_watch_retries_a_refused_publication() {
    let collector = Collector::start();
    collector.answer(503);
    let (dir, public, token) = project(&typed_block(&collector.endpoint()).replace("every 30s", "every 1s"), "spans");
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 1));
    let mut watcher = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["export", "watch", "spans-mirror", "--project", "research", "--public-key", &public, "--audience", AUD])
        .env("CONTEXTFUL_TOKEN", &token).current_dir(p).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
    collector.await_requests(1);
    let machine = p.join(".contextful/context/research/machine.sqlite");
    assert_eq!(contextful_sqlite::ExportLedger::open(&machine).unwrap().position("spans-mirror").unwrap().ack_sequence, None);
    collector.answer(200);
    collector.await_requests(2);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if contextful_sqlite::ExportLedger::open(&machine).unwrap().position("spans-mirror").unwrap().ack_sequence == Some(1) { break; }
        assert!(Instant::now() < deadline, "watch did not acknowledge the retry");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(typed_events(&collector.received()[0]), typed_events(&collector.received()[1]));
    watcher.kill().unwrap();
    watcher.wait().unwrap();
}

#[test]
fn typed_export_splits_requests_by_bytes_and_refuses_an_oversized_event() {
    let collector = Collector::start();
    let (dir, public, token) = project(&typed_block(&collector.endpoint()), "spans");
    let p = dir.path();
    let lines = ["a", "b"].iter().map(|id| serde_json::json!({"span_id":id,"body":"x".repeat(40_000)}).to_string()).collect::<Vec<_>>().join("\n");
    std::fs::write(p.join("rows.jsonl"), lines).unwrap();
    ok(&cf(p, &["context", "land", "spans", "--project", "research", "--rows", "rows.jsonl", "--run-id", "load-1", "--site-id", "site"], &[]));
    ok(&export(p, &public, &token, &[]));
    let received = collector.received();
    assert_eq!(received.len(), 2);
    for request in &received {
        assert!(serde_json::to_vec(&request.body).unwrap().len() <= 64 * 1024);
    }
    assert_eq!(typed_events(&received[0]).len(), 1);
    assert_eq!(typed_events(&received[1]).len(), 2);

    let (too_large, large_public, large_token) = project(&typed_block(&collector.endpoint()), "spans");
    let p = too_large.path();
    std::fs::write(p.join("rows.jsonl"), serde_json::json!({"span_id":"huge","body":"x".repeat(70_000)}).to_string()).unwrap();
    ok(&cf(p, &["context", "land", "spans", "--project", "research", "--rows", "rows.jsonl", "--run-id", "load-1", "--site-id", "site"], &[]));
    let refused = err(&export(p, &large_public, &large_token, &[]));
    assert!(refused.contains("ExportEventTooLarge"), "{refused}");
    assert_eq!(collector.received().len(), 2);
    let mut watcher = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["export", "watch", "spans-mirror", "--project", "research", "--public-key", &large_public, "--audience", AUD])
        .env("CONTEXTFUL_TOKEN", &large_token).current_dir(p).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = watcher.try_wait().unwrap() {
            assert!(!status.success());
            break;
        }
        if Instant::now() >= deadline {
            watcher.kill().unwrap();
            watcher.wait().unwrap();
            panic!("watch retried an event that cannot fit a request");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(collector.received().len(), 2);
}

const BEARER: &str = "headers = { Authorization = \"Bearer ${secret://otel-token}\" }\n";

/// A project with an issuer key and a credential reading `table`.
fn project(manifest: &str, table: &str) -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let store = p.join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::write(p.join(".contextful/issuance.toml"), format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n")).unwrap();
    std::fs::write(p.join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{}", manifest)).unwrap();
    let public = ok(&cf(p, &["token", "keygen", "--out", ".contextful/issuer.seed"], &[]));
    let token = ok(&cf(
        p,
        &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "svc://mirror", "--zone", "on-prem:hq", "--table", table, "--ttl", "600"],
        &[],
    ));
    (dir, public, token)
}

/// Land `ids` into `spans` as run `run`, stamped `now`.
fn land(dir: &Path, run: &str, now: &str, ids: &[String]) {
    let rows: String = ids.iter().enumerate().map(|(i, id)| format!("{{\"span_id\":\"{id}\",\"duration_ms\":{}}}\n", 10 + i)).collect();
    let file = format!("{run}.jsonl");
    std::fs::write(dir.join(&file), rows).unwrap();
    ok(&cf(dir, &["context", "land", "spans", "--project", "research", "--rows", &file, "--run-id", run, "--site-id", "site", "--now", now], &[]));
}

fn ids(prefix: &str, n: usize) -> Vec<String> {
    (0..n).map(|i| format!("{prefix}{i:03}")).collect()
}

fn export(dir: &Path, public: &str, token: &str, env: &[(&str, &str)]) -> Output {
    let mut env = env.to_vec();
    env.push(("CONTEXTFUL_TOKEN", token));
    cf(dir, &["export", "run", "spans-mirror", "--project", "research", "--public-key", public, "--audience", AUD], &env)
}

const SECRET: [(&str, &str); 2] = [("OTEL_TOKEN", "otel-token-value"), ("CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES", "1")];

fn cursor(dir: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(dir.join(".contextful/exports/research/spans-mirror.json")).unwrap()).unwrap()
}

/// A manifest `[[export]]` block declares `name`, one landed `table`, an OTLP/HTTP `endpoint`, `signal` and optional
/// `headers`; `contextful export run <name>` delivers every row its cursor has not passed.
// spec: run.export.export-block@011fb3be
#[test]
fn an_export_run_delivers_the_rows_its_cursor_has_not_passed() {
    let collector = Collector::start();
    let (dir, public, token) = project(&block(&collector.endpoint(), BEARER), "spans");
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 3));

    let out = ok(&export(p, &public, &token, &SECRET));
    assert!(out.contains("spans-mirror: delivered 3 rows in 1 batches"), "{out}");
    let got = collector.received();
    assert_eq!(got.len(), 1);
    assert_eq!((got[0].method.as_str(), got[0].path.as_str()), ("POST", "/v1/logs"));
    assert!(got[0].headers.contains(&"content-type: application/json".to_string()), "{:?}", got[0].headers);
    assert_eq!(span_ids(&got[0]), ids("a", 3));

    // Nothing new: no request.
    let again = ok(&export(p, &public, &token, &SECRET));
    assert!(again.contains("delivered 0 rows in 0 batches"), "{again}");
    assert_eq!(collector.received().len(), 1);

    land(p, "load-2", "2030-01-01T00:01:00Z", &ids("b", 2));
    ok(&export(p, &public, &token, &SECRET));
    let got = collector.received();
    assert_eq!(got.len(), 2);
    assert_eq!(span_ids(&got[1]), ids("b", 2));
}

/// Export reads the rows past its cursor in `_commit_seq`, then `_row_seq`, order and never by `_ingested_at`, so
/// {{store.reserve.commit-seq}} keeps a run committing late from being skipped.
// spec: run.export.commit-order@0cfa5bc8
#[test]
fn a_run_committing_after_the_cursor_under_an_earlier_stamp_is_delivered() {
    let collector = Collector::start();
    let (dir, public, token) = project(&block(&collector.endpoint(), ""), "spans");
    let p = dir.path();
    land(p, "load-late-stamp", "2030-01-01T00:10:00Z", &ids("a", 2));
    ok(&export(p, &public, &token, &[]));

    // A second writer commits after the export under an earlier transaction-time stamp.
    land(p, "load-early-stamp", "2030-01-01T00:00:00Z", &ids("b", 2));
    ok(&export(p, &public, &token, &[]));
    let got = collector.received();
    assert_eq!(got.len(), 2);
    assert_eq!(span_ids(&got[1]), ids("b", 2));
    let seqs: Vec<i64> = got.iter().flat_map(records).map(|r| attribute(&r, "contextful.commit_seq").unwrap().parse().unwrap()).collect();
    assert!(!seqs.is_empty(), "the exclusion below ranges over no element");
    assert!(seqs.windows(2).all(|w| w[0] <= w[1]), "{seqs:?}");
}

/// One delivery batch holds at most 500 rows.
// spec: run.export.batch-rows@6e49b571
#[test]
fn a_delivery_batch_holds_at_most_five_hundred_rows() {
    let collector = Collector::start();
    let (dir, public, token) = project(&block(&collector.endpoint(), ""), "spans");
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 700));
    land(p, "load-2", "2030-01-01T00:01:00Z", &ids("b", 301));
    let out = ok(&export(p, &public, &token, &[]));
    assert!(out.contains("delivered 1001 rows in 3 batches"), "{out}");
    let sizes: Vec<usize> = collector.received().iter().map(|r| records(r).len()).collect();
    assert_eq!(sizes, [500, 500, 1]);
    let all: Vec<String> = collector.received().iter().flat_map(span_ids).collect();
    let mut expected = ids("a", 700);
    expected.extend(ids("b", 301));
    assert_eq!(all, expected, "every row once, in commit order");
}

/// Export reads committed runs only, through the read face under the admitted credential, so its grants, row policy
/// and masks hold; no landing waits on an export.
// spec: run.export.post-commit-read@e659ab94
#[test]
fn export_reads_through_the_face_under_the_admitted_credential() {
    let collector = Collector::start();
    let manifest = format!(
        "[[pipeline.tables]]\nname = \"spans\"\n[pipeline.tables.policy.columns]\nspan_id = {{ strategy = \"hash\" }}\n\n{}",
        block(&collector.endpoint(), "")
    );
    let (dir, public, token) = project(&manifest, "spans");
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 2));
    ok(&export(p, &public, &token, &[]));
    let got = collector.received();
    assert_eq!(got.len(), 1);
    for id in span_ids(&got[0]) {
        assert!(!ids("a", 2).contains(&id), "the mask did not hold: `{id}` left unmasked");
    }

    // A credential granting another table sees no relation `spans`, and nothing leaves.
    let (other, other_public, other_token) = project(&block(&collector.endpoint(), ""), "documents");
    land(other.path(), "load-1", "2030-01-01T00:00:00Z", &ids("a", 2));
    let refused = err(&export(other.path(), &other_public, &other_token, &[]));
    assert!(refused.contains("EnforceUnknownRelation: `spans`"), "{refused}");
    assert_eq!(collector.received().len(), 1);
    assert!(!other.path().join(".contextful/exports/research/spans-mirror.json").exists());

    // A landing commits while an export holds a batch in flight: no landing waits on an export.
    land(p, "load-2", "2030-01-01T00:01:00Z", &ids("b", 2));
    collector.hold(true);
    let (dir_in_flight, public_in_flight, token_in_flight) = (p.to_path_buf(), public.clone(), token.clone());
    let in_flight = std::thread::spawn(move || export(&dir_in_flight, &public_in_flight, &token_in_flight, &[]));
    collector.await_requests(2);
    land(p, "load-3", "2030-01-01T00:02:00Z", &ids("c", 2));
    assert!(!in_flight.is_finished(), "the export answered before its target did");
    collector.hold(false);
    ok(&in_flight.join().unwrap());
    ok(&export(p, &public, &token, &[]));
    assert_eq!(collector.received().len(), 3, "the run landed during the export leaves in the next run");
}

/// A table holding rows whose `_commit_seq` is null, from parts landed without the column, raises
/// `ExportCommitSeqMissing` naming the export and the row count, before any batch leaves.
// spec: run.export.commit-seq-missing@fc1e7556
#[test]
fn rows_without_a_commit_sequence_refuse_the_export() {
    let collector = Collector::start();
    let (dir, public, token) = project(&block(&collector.endpoint(), ""), "spans");
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 3));
    land(p, "load-2", "2030-01-01T00:01:00Z", &ids("b", 2));

    // Rewrite load-1's part as a landing without the column writes it.
    let node = p.join(".contextful/context/research/tables/spans/data/runs/load-1/ingest-a");
    let part = std::fs::read_dir(&node).unwrap().flatten().map(|e| e.path()).find(|f| f.extension().is_some_and(|x| x == "parquet")).unwrap();
    let mut batches = contextful_context::parquet_io::read(&part).unwrap();
    let mut batch = batches.remove(0);
    let at = batch.schema().index_of("_commit_seq").unwrap();
    batch.remove_column(at);
    contextful_context::parquet_io::write(&part, &batch).unwrap();

    let refused = err(&export(p, &public, &token, &[]));
    assert!(refused.contains("ExportCommitSeqMissing") && refused.contains("spans-mirror") && refused.contains("3 rows"), "{refused}");
    assert!(collector.received().is_empty());
    assert!(!p.join(".contextful/exports/research/spans-mirror.json").exists());
}

/// A batch leaves as one OTLP/HTTP JSON `POST` through the mediated client, its allowlist the endpoint's host alone,
/// each header hydrated as {{connector.resolve.hydration-is-just-in-time}} states.
// spec: run.export.delivery@51449bb8
#[test]
fn a_batch_leaves_through_the_mediated_client_with_its_header_hydrated() {
    let collector = Collector::start();
    let (dir, public, token) = project(&block(&collector.endpoint(), BEARER), "spans");
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 1));
    let out = export(p, &public, &token, &SECRET);
    ok(&out);
    assert!(!String::from_utf8_lossy(&out.stderr).contains("otel-token-value") && !String::from_utf8_lossy(&out.stdout).contains("otel-token-value"));
    let got = collector.received();
    assert!(got[0].headers.contains(&"authorization: bearer otel-token-value".to_string()), "{:?}", got[0].headers);
    assert!(!std::fs::read_to_string(p.join(".contextful/exports/research/spans-mirror.json")).unwrap().contains("otel-token-value"));

    // A credential bound for cleartext off loopback never leaves.
    let (dir, public, token) = project(&block("http://otel.example.com/v1/logs", BEARER), "spans");
    land(dir.path(), "load-1", "2030-01-01T00:00:00Z", &ids("a", 1));
    let refused = err(&export(dir.path(), &public, &token, &SECRET));
    assert!(refused.contains("SecretCleartextEndpoint"), "{refused}");
}

/// A header reference no adapter answers refuses at preflight as {{connector.resolve.unresolved-name}}, before any
/// row is read.
// spec: run.export.secret-preflight@bc303782
#[test]
fn an_unresolved_header_reference_refuses_before_any_request() {
    let collector = Collector::start();
    let (dir, public, token) = project(&block(&collector.endpoint(), BEARER), "spans");
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 1));
    let refused = err(&export(p, &public, &token, &[]));
    assert!(refused.contains("SecretUnresolvedReference") && refused.contains("otel-token"), "{refused}");
    assert!(collector.received().is_empty());
    assert!(!p.join(".contextful/exports/research/spans-mirror.json").exists());
}

/// The cursor, the last delivered `_commit_seq` and `_row_seq`, commits to `.contextful/exports/<project>/<name>.json`,
/// outside the synced store root, only after the target answers 2xx for the batch.
// spec: run.export.cursor-after-ack@ac14910a
#[test]
fn the_cursor_commits_after_the_target_acknowledges() {
    let collector = Collector::start();
    let (dir, public, token) = project(&block(&collector.endpoint(), ""), "spans");
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 3));
    ok(&export(p, &public, &token, &[]));
    let delivered = records(&collector.received()[0]);
    let last = delivered.last().unwrap();
    assert_eq!(
        cursor(p),
        serde_json::json!({
            "commit_seq": attribute(last, "contextful.commit_seq").unwrap().parse::<i64>().unwrap(),
            "row_seq": attribute(last, "contextful.row_seq").unwrap().parse::<i64>().unwrap(),
        })
    );
}

/// A run ending between a target's acknowledgement and the cursor commit resends that batch on the next run; a target
/// deduplicates on `contextful.commit_seq` and `contextful.row_seq`.
// spec: run.export.at-least-once@3f337cb5
#[test]
fn a_batch_acknowledged_without_its_cursor_commit_is_resent() {
    let collector = Collector::start();
    let (dir, public, token) = project(&block(&collector.endpoint(), ""), "spans");
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 2));
    ok(&export(p, &public, &token, &[]));
    let first = collector.received()[0].clone();

    // The acknowledgement landed and the cursor commit did not: the cursor stands where it stood before.
    std::fs::remove_file(p.join(".contextful/exports/research/spans-mirror.json")).unwrap();
    ok(&export(p, &public, &token, &[]));
    let got = collector.received();
    assert_eq!(got.len(), 2);
    let key = |r: &Received| -> Vec<(String, String)> {
        records(r).iter().map(|rec| (attribute(rec, "contextful.commit_seq").unwrap(), attribute(rec, "contextful.row_seq").unwrap())).collect()
    };
    assert_eq!(key(&got[1]), key(&first), "the resent batch carries the keys a target deduplicates on");
}

/// A target answering other than 2xx, or unreachable, raises `ExportDeliveryRefused` naming the export and the answer,
/// and the cursor stays where it stood.
// spec: run.export.delivery-refused@705e5a43
#[test]
fn a_refused_delivery_leaves_the_cursor_where_it_stood() {
    let collector = Collector::start();
    let (dir, public, token) = project(&block(&collector.endpoint(), ""), "spans");
    let p = dir.path();
    land(p, "load-1", "2030-01-01T00:00:00Z", &ids("a", 2));
    ok(&export(p, &public, &token, &[]));
    let before = cursor(p);

    land(p, "load-2", "2030-01-01T00:01:00Z", &ids("b", 2));
    collector.answer(503);
    let refused = err(&export(p, &public, &token, &[]));
    assert!(refused.contains("ExportDeliveryRefused") && refused.contains("spans-mirror") && refused.contains("503"), "{refused}");
    assert_eq!(cursor(p), before);

    // Recovery resends the refused batch.
    collector.answer(200);
    ok(&export(p, &public, &token, &[]));
    let got = collector.received();
    assert_eq!(span_ids(got.last().unwrap()), ids("b", 2));

    // An unreachable target refuses the same way.
    let dead = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let (dir, public, token) = project(&block(&format!("http://127.0.0.1:{dead}/v1/logs"), ""), "spans");
    land(dir.path(), "load-1", "2030-01-01T00:00:00Z", &ids("a", 1));
    let unreachable = err(&export(dir.path(), &public, &token, &[]));
    assert!(unreachable.contains("ExportDeliveryRefused"), "{unreachable}");
}
