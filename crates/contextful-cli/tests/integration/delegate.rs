//! Delegated bodies (`surface.delegate`, `run.items.delegated-item`): a store-driven job whose
//! isolated items run on a loopback `call` delegate through the stock binary.
#![cfg(feature = "data-plane")]

use contextful_acceptance::http::{Request, Response, Server};
use contextful_core::surface::delegate::{sign_answer, sign_request, Answer};
use contextful_core::surface::worker::{SIGNATURE_HEADER, TIMESTAMP_HEADER};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

const AUD: &str = "contextful://acme-research";
const KEY: &[u8] = b"a-deployment-worker-key";
const ROUTE: &str = "/stage/screen";

fn cf(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful")).args(args).envs(env.iter().copied()).current_dir(dir).env_remove("CONTEXTFUL_NODE_ID").output().unwrap()
}

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Standard output and error together, whatever the exit status.
fn text(out: &Output) -> String {
    format!("{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn unix_now() -> i64 {
    i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()).unwrap()
}

/// `answer`, signed under the deployment key over `answer\n<timestamp>\n` and its body.
fn signed(answer: &Answer) -> Response {
    let body = answer.encode();
    let timestamp = unix_now();
    Response {
        status: 200,
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            (TIMESTAMP_HEADER.into(), timestamp.to_string()),
            (SIGNATURE_HEADER.into(), sign_answer(KEY, timestamp, &body)),
        ],
        body,
    }
}

/// The request's envelope, after its signature under the deployment key verifies.
fn verified(request: &Request) -> Value {
    let timestamp: i64 = request.header(TIMESTAMP_HEADER).expect("timestamp header").parse().unwrap();
    assert_eq!(request.header(SIGNATURE_HEADER), Some(sign_request(KEY, timestamp, &request.body).as_str()), "signed under the deployment key");
    serde_json::from_slice(&request.body).unwrap()
}

fn entity(request: &Request) -> String {
    serde_json::from_slice::<Value>(&request.body).unwrap()["row"]["entity_id"].as_str().unwrap().to_string()
}

/// `ok` rows for the envelope's row.
fn rows_for(envelope: &Value) -> Answer {
    let row = json!({ "entity_id": envelope["row"]["entity_id"], "pass_id": envelope["row"]["pass_id"], "proceed": true });
    Answer::Ok { rows: [("screen_result".to_string(), vec![row.as_object().unwrap().clone()])].into() }
}

fn manifest(url: &str, timeout: &str, max_attempts: u32) -> String {
    format!(
        "authoring_posture = \"per_request\"\n\n[control.delegates.stages]\nurl = \"{url}\"\nmode = \"call\"\ntimeout = \"{timeout}\"\n\n\
         [[job]]\nname = \"stage-screen\"\nkind = \"store-driven\"\nbody = {{ delegate = \"stages\" }}\n\
         statement = \"SELECT entity_id, pass_id FROM due_items ORDER BY pass_id\"\nas_of = \"2030-01-01T00:00:10Z\"\n\
         max_in_flight = 2\ntables = [\"screen_result\"]\n\
         [job.isolated_items]\nlogical_key = \"pass_id\"\nentity_key = \"entity_id\"\nentity_namespace = \"entity\"\nmax_attempts = {max_attempts}\n"
    )
}

/// A project with two due items, an issuer key and a credential reading `due_items`.
fn project(manifest: &str) -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let store = p.join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::write(p.join(".contextful/issuance.toml"), format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n")).unwrap();
    std::fs::write(p.join("contextful.toml"), manifest).unwrap();
    std::fs::write(p.join("due.jsonl"), "{\"entity_id\":\"e-1\",\"pass_id\":\"p-1\"}\n{\"entity_id\":\"e-2\",\"pass_id\":\"p-2\"}\n").unwrap();
    ok(&cf(p, &["context", "land", "due_items", "--project", "research", "--rows", "due.jsonl", "--run-id", "load-1", "--site-id", "site", "--now", "2030-01-01T00:00:00Z"], &[]));
    let public = ok(&cf(p, &["token", "keygen", "--out", ".contextful/issuer.seed"], &[]));
    let token = ok(&cf(
        p,
        &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "svc://stages", "--zone", "on-prem:hq", "--table", "due_items", "--ttl", "600"],
        &[],
    ));
    (dir, public, token)
}

fn fire(p: &Path, public: &str, token: &str, run: &str, now: &str) -> Output {
    cf(
        p,
        &["job", "fire", "stage-screen", "--project", "research", "--run-id", run, "--site-id", "site", "--now", now, "--public-key", public, "--audience", AUD],
        &[("CONTEXTFUL_TOKEN", token), ("CONTEXTFUL_WORKER_KEY", std::str::from_utf8(KEY).unwrap())],
    )
}

/// The rows `sql` answers over the project, each as its cells' text.
fn select(p: &Path, sql: &str) -> Vec<Vec<String>> {
    let v: Value = serde_json::from_str(&ok(&cf(p, &["query", "--json", "--project", "research", sql], &[]))).unwrap();
    v["rows"].as_array().unwrap().iter().map(|r| r.as_array().unwrap().iter().map(|c| c.as_str().map(str::to_string).unwrap_or_else(|| c.to_string())).collect()).collect()
}

/// Under a delegated body, an item's attempt is one request under the envelope, and the answer is recorded as
/// the body output under the item's entry key, so a resume replays it and sends nothing.
// spec: run.items.delegated-item@00026c2f
#[test]
fn a_call_delegate_takes_one_signed_envelope_per_item_and_a_later_fire_sends_nothing() {
    let delegate = Server::start(|request| signed(&rows_for(&verified(request))));
    let (dir, public, token) = project(&manifest(&delegate.url(ROUTE), "30s", 3));
    let p = dir.path();
    ok(&fire(p, &public, &token, "fire-1", "2030-01-01T00:01:00Z"));

    let requests = delegate.received(ROUTE);
    assert_eq!(requests.len(), 2, "one request per item");
    for request in &requests {
        let envelope = verified(request);
        assert_eq!(envelope["job"], "stage-screen");
        assert_eq!(envelope["tables"], json!(["screen_result"]));
        assert_eq!(envelope["as_of"], "2030-01-01T00:00:10Z");
        assert_eq!(envelope["attempt"], 1);
        assert_eq!(envelope["entity"]["namespace"], "entity");
        assert_eq!(envelope["entity"]["key"], envelope["row"]["entity_id"]);
        assert!(envelope["entity"]["fence"].is_u64() && envelope["pins"].is_object() && envelope["version"].is_u64(), "{envelope}");
        assert!(envelope["step"].as_str().unwrap().starts_with("row/") && envelope["step"].as_str().unwrap().ends_with("/delegate/1"), "{envelope}");
        assert_eq!(envelope["input_hash"].as_str().unwrap().len(), 64);
        assert!(!envelope["execution_id"].as_str().unwrap().is_empty() && !envelope["idempotency_key"].as_str().unwrap().is_empty(), "{envelope}");
        assert_eq!(envelope["deadline"], "2030-01-01T00:01:30Z", "the engine's instant plus the timeout");
    }
    assert_eq!(select(p, "SELECT entity_id, pass_id FROM screen_result ORDER BY pass_id"), [["e-1", "p-1"], ["e-2", "p-2"]]);

    // Both items hold completion receipts: a later fire sends nothing and lands nothing new.
    ok(&fire(p, &public, &token, "fire-2", "2030-01-01T00:02:00Z"));
    assert_eq!(delegate.received(ROUTE).len(), 2, "a completed item is never submitted again");
    assert_eq!(select(p, "SELECT entity_id FROM screen_result").len(), 2);
}

/// A `defer` parks the item until its `retry_at`, consuming no failed attempt; the first fire after that
/// instant re-admits it under the item's retained journal, and no run sleeps on it.
// spec: surface.delegate.defer-parks@7b5e9381
#[test]
fn a_defer_answer_parks_the_item_without_spending_an_attempt_until_its_retry_at() {
    let seen: Arc<Mutex<BTreeMap<String, u32>>> = Arc::default();
    let delegate = Server::start({
        let seen = seen.clone();
        move |request| {
            let envelope = verified(request);
            let key = envelope["row"]["entity_id"].as_str().unwrap().to_string();
            let n = {
                let mut seen = seen.lock().unwrap();
                *seen.entry(key.clone()).or_default() += 1;
                seen[&key]
            };
            if n == 1 {
                signed(&Answer::Defer { retry_at: "2030-01-01T00:01:03Z".into(), reason: Some("budget".into()) })
            } else {
                signed(&rows_for(&envelope))
            }
        }
    });
    // One failed attempt is the whole budget: a defer that spent it would end the item.
    let (dir, public, token) = project(&manifest(&delegate.url(ROUTE), "30s", 1));
    let p = dir.path();
    let first = text(&fire(p, &public, &token, "fire-1", "2030-01-01T00:01:00Z"));
    assert!(first.contains("2 parked"), "{first}");
    assert_eq!(delegate.received(ROUTE).len(), 2);

    // Before `retry_at`, the parked items stay parked and nothing is sent.
    let second = text(&fire(p, &public, &token, "fire-2", "2030-01-01T00:01:02Z"));
    assert!(second.contains("2 parked"), "{second}");
    assert_eq!(delegate.received(ROUTE).len(), 2);

    // After it, each item asks again under the next ordinal and lands.
    ok(&fire(p, &public, &token, "fire-3", "2030-01-01T00:01:05Z"));
    let requests = delegate.received(ROUTE);
    assert_eq!(requests.len(), 4);
    assert_eq!(verified(&requests[3])["attempt"], 2);
    assert_eq!(select(p, "SELECT entity_id FROM screen_result ORDER BY entity_id"), [["e-1"], ["e-2"]]);
}

/// A `call` response with a status other than `200` closes the attempt `Transient`, whatever its body; only a
/// signed `permanent` answer writes a terminal receipt.
// spec: surface.delegate.status@689099b4
#[test]
fn a_non_200_answer_spends_one_attempt_and_a_permanent_answer_writes_a_terminal_receipt() {
    let delegate = Server::start(|request| {
        let envelope = verified(request);
        if envelope["row"]["entity_id"] == "e-1" {
            // A `permanent` body under a 503 is still transient.
            Response::json(503, "{\"outcome\":\"permanent\",\"reason\":\"down\"}")
        } else {
            signed(&Answer::Permanent { reason: Some("subject withdrawn".into()) })
        }
    });
    let (dir, public, token) = project(&manifest(&delegate.url(ROUTE), "30s", 2));
    let p = dir.path();
    let count = |id: &str| delegate.received(ROUTE).iter().filter(|r| entity(r) == id).count();

    let first = text(&fire(p, &public, &token, "fire-1", "2030-01-01T00:01:00Z"));
    assert!(first.contains("2 failed"), "{first}");
    let (e1, e2) = (count("e-1"), count("e-2"));
    assert!(e1 >= 1 && e2 == 1, "{e1} {e2}");

    // The transient item is asked again on its second attempt; the terminal one is not.
    text(&fire(p, &public, &token, "fire-2", "2030-01-01T00:02:00Z"));
    assert!(count("e-1") > e1, "a transient failure spends one attempt, not the budget");
    assert_eq!(count("e-2"), 1, "a permanent answer writes a terminal receipt");

    // Two failed attempts spend the budget; a third fire asks nothing.
    let e1 = count("e-1");
    text(&fire(p, &public, &token, "fire-3", "2030-01-01T00:03:00Z"));
    assert_eq!((count("e-1"), count("e-2")), (e1, 1));
}

/// Under `call`, one request per attempt holds its connection open and the response body is the attempt's
/// answer; a response arriving after the delegate's `timeout` closes the attempt `Transient`, and its late body
/// records nothing.
// spec: surface.delegate.call-mode@dcc54901
#[test]
fn a_response_after_the_timeout_closes_the_attempt_transient_and_lands_nothing() {
    let delegate = Server::start(|request| {
        let envelope = verified(request);
        std::thread::sleep(std::time::Duration::from_secs(3));
        signed(&rows_for(&envelope))
    });
    let (dir, public, token) = project(&manifest(&delegate.url(ROUTE), "1s", 1));
    let p = dir.path();
    let first = text(&fire(p, &public, &token, "fire-1", "2030-01-01T00:01:00Z"));
    assert!(first.contains("2 failed"), "{first}");
    assert!(delegate.received(ROUTE).len() >= 2);
    assert!(text(&cf(p, &["query", "--json", "--project", "research", "SELECT entity_id FROM screen_result"], &[])).contains("screen_result"), "no row landed from the late body");

    // The one-attempt budget is spent: a later fire sends nothing.
    std::thread::sleep(std::time::Duration::from_secs(4));
    let sent = delegate.received(ROUTE).len();
    let second = text(&fire(p, &public, &token, "fire-2", "2030-01-01T00:02:00Z"));
    assert_eq!(delegate.received(ROUTE).len(), sent, "first fire:\n{first}\nsecond fire:\n{second}");
}

#[test]
fn job_validate_refuses_a_public_host_an_over_ceiling_timeout_and_an_unbound_delegate() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let validate = |manifest: &str| {
        std::fs::write(p.join("contextful.toml"), manifest).unwrap();
        text(&cf(p, &["job", "validate"], &[]))
    };
    assert!(validate(&manifest("http://127.0.0.1:1/stage", "30s", 1)).contains("stage-screen: valid (store-driven, body delegate:stages"));
    assert!(validate(&manifest("http://delegates.example.org/stage", "30s", 1)).contains("DelegateHostRefused"));
    assert!(validate(&manifest("https://127.0.0.1:1/stage", "30s", 1)).contains("DelegateHostRefused"));
    assert!(validate(&manifest("http://127.0.0.1:1/stage", "2h", 1)).contains("DelegateTimeoutRefused"));
    let unbound = manifest("http://127.0.0.1:1/stage", "30s", 1).replace("delegate = \"stages\"", "delegate = \"other\"");
    assert!(validate(&unbound).contains("JobDelegateUnbound"));
}
