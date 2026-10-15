//! Milestone 17 — delegated bodies.
//!
//! Reach: a store-driven item runs its paid call on a loopback delegate, its answer lands as
//! the item's rows, and a resume after a restart sends no second request.

use contextful_acceptance::http::{Request, Response, Server};
use contextful_acceptance::{bin, GitRepo};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::process::Output;

const AUD: &str = "contextful://acme-research";
const KEY: &[u8] = b"a-deployment-worker-key";
const STORE: &str = ".contextful/context/research";

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// HMAC-SHA256 over `message` under `key`, as lowercase hex: the signature every delegate
/// request and answer carries.
fn hmac_hex(key: &[u8], message: &[u8]) -> String {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let inner: Vec<u8> = k.iter().map(|b| b ^ 0x36).chain(message.iter().copied()).collect();
    let outer: Vec<u8> = k.iter().map(|b| b ^ 0x5c).chain(Sha256::digest(&inner).iter().copied()).collect();
    Sha256::digest(&outer).iter().map(|b| format!("{b:02x}")).collect()
}

/// The delegate's answer: `ok` rows for the job's one output table, signed under the
/// deployment key over `answer\n<idempotency_key>\n<timestamp>\n` and the body.
fn answer(request: &Request) -> Response {
    let envelope: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
    let row = &envelope["row"];
    let body = json!({
        "outcome": "ok",
        "rows": { "screen_result": [{ "entity_id": row["entity_id"], "pass_id": row["pass_id"], "proceed": true }] }
    })
    .to_string();
    let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs().to_string();
    let key = envelope["idempotency_key"].as_str().unwrap_or_default();
    let signature = hmac_hex(KEY, format!("answer\n{key}\n{timestamp}\n{body}").as_bytes());
    let mut response = Response::json(200, &body);
    response.headers.push(("x-contextful-timestamp".into(), timestamp));
    response.headers.push(("x-contextful-signature".into(), signature));
    response
}

fn config(delegate: &str) -> String {
    format!(
        "site_id = \"site-a\"\nauthoring_posture = \"per_request\"\n\n\
         [control.delegates.stages]\nurl = \"{delegate}\"\nmode = \"call\"\ntimeout = \"30s\"\n\n\
         [[job]]\nname = \"stage-screen\"\nkind = \"store-driven\"\nbody = {{ delegate = \"stages\" }}\n\
         statement = \"SELECT entity_id, pass_id FROM due_items ORDER BY pass_id\"\n\
         as_of = \"2030-01-01T00:00:10Z\"\nmax_in_flight = 2\ntables = [\"screen_result\"]\n\
         isolated_items = {{ logical_key = \"pass_id\", entity_key = \"entity_id\", entity_namespace = \"entity\", max_attempts = 3 }}\n"
    )
}

#[test]
#[ignore]
fn m17_delegated() {
    let cf = bin("contextful");
    let delegate = Server::start(answer);
    let p = GitRepo::init();
    p.write(&format!("{STORE}/config.toml"), "[node]\nid = \"ingest-a\"\n");
    p.write(".contextful/issuance.toml", &format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n"));
    p.write("contextful.toml", &config(&delegate.url("/stage/screen")));
    p.write("due.jsonl", "{\"entity_id\":\"e-1\",\"pass_id\":\"p-1\"}\n{\"entity_id\":\"e-2\",\"pass_id\":\"p-2\"}\n");
    ok(&p.run(&cf, &["context", "land", "due_items", "--project", "research", "--rows", "due.jsonl", "--run-id", "load-1", "--site-id", "site", "--now", "2030-01-01T00:00:00Z"]));

    let public = ok(&p.run(&cf, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let token = ok(&p.run(
        &cf,
        &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "svc://stages", "--zone", "on-prem:hq", "--table", "due_items", "--ttl", "600"],
    ));
    let fire = |run: &str| {
        p.run_env(
            &cf,
            &["job", "fire", "stage-screen", "--project", "research", "--run-id", run, "--site-id", "site", "--now", "2030-01-01T00:01:00Z", "--public-key", &public, "--audience", AUD],
            &[("CONTEXTFUL_TOKEN", &token), ("CONTEXTFUL_WORKER_KEY", std::str::from_utf8(KEY).unwrap())],
        )
    };

    // Each isolated item is one signed request carrying the envelope, and its answer lands.
    ok(&fire("fire-1"));
    let requests = delegate.received("/stage/screen");
    assert_eq!(requests.len(), 2, "one request per item");
    for request in &requests {
        let timestamp = request.header("x-contextful-timestamp").expect("timestamp header");
        let expected = hmac_hex(KEY, &[format!("call\n{timestamp}\n").as_bytes(), &request.body].concat());
        assert_eq!(request.header("x-contextful-signature"), Some(expected.as_str()), "signed under the deployment key");
        let envelope: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(envelope["job"], "stage-screen");
        assert_eq!(envelope["tables"], json!(["screen_result"]));
        assert_eq!(envelope["as_of"], "2030-01-01T00:00:10Z");
        assert_eq!(envelope["entity"]["namespace"], "entity");
        for field in ["execution_id", "step", "input_hash", "attempt", "idempotency_key", "deadline"] {
            assert!(!envelope[field].is_null(), "envelope carries `{field}`: {envelope}");
        }
        assert!(envelope["version"].is_null(), "a fire from the local declaration names no applied version");
        assert!(envelope["entity"]["fence"].is_number() && envelope["pins"].is_object(), "{envelope}");
    }
    let landed: Value = serde_json::from_str(&ok(&p.run(&cf, &["query", "--json", "--project", "research", "SELECT entity_id, pass_id FROM screen_result ORDER BY pass_id"]))).unwrap();
    assert_eq!(landed["rows"], json!([["e-1", "p-1"], ["e-2", "p-2"]]));

    // A later fire under the same execution identity holds both items' completion receipts
    // and sends nothing.
    ok(&fire("fire-2"));
    assert_eq!(delegate.received("/stage/screen").len(), 2, "a completed item is never submitted again");
}
