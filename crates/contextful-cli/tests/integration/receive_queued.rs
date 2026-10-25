//! A served delivery asking for the queued answer: `202` once the batch is in queue custody,
//! then `contextful queue drain` lands it through the receive group, and the outcome route
//! answers what its landing did.
#![cfg(feature = "data-plane")]

use crate::serve::{control, delivered, mint_write, receive_rows, receiving_face_with, run, serve, stdout, AUD};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};

/// `POST /receive/otel` under `token` and `key`, preferring the queued answer when `queued`;
/// the status and the JSON answer.
pub(super) fn deliver(addr: &str, token: &str, key: &str, body: &str, queued: bool) -> (u16, Value) {
    let mut stream = TcpStream::connect(addr).unwrap();
    let prefer = if queued { "Prefer: respond-async\r\n" } else { "" };
    write!(stream, "POST /receive/otel HTTP/1.1\r\nAuthorization: Bearer {token}\r\nContextful-Batch-Key: {key}\r\n{prefer}Content-Type: application/x-ndjson\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (head, answer) = raw.split_once("\r\n\r\n").unwrap_or_else(|| panic!("no answer head: {raw:?}"));
    let status = head.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or_else(|| panic!("no status: {raw:?}"));
    (status, serde_json::from_str(answer).unwrap_or_else(|e| panic!("{e}: {raw}")))
}

/// The outcome route's answer for `key`.
pub(super) fn outcome(addr: &str, token: &str, key: &str) -> (u16, Value) {
    control(addr, "GET", &format!("/receive/otel/{key}"), Some(token), "")
}

/// Drain the project's queue under the face's pins, `now` overriding the clock when given.
fn drain(root: &Path, public: &str, now: Option<&str>) -> String {
    let mut args = vec!["queue", "drain", "--project", "research", "--public-key", public, "--audience", AUD];
    if let Some(now) = now {
        args.extend(["--now", now]);
    }
    stdout(&run(root, &args))
}

fn spool(root: &Path) -> PathBuf {
    root.join(".contextful/queue/research")
}

/// The `receive` batches pending in the local spool; a landing's post-commit wake-ups sit beside them.
fn queued(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(spool(root)) else { return Vec::new() };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter(|p| std::fs::read(p).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).is_some_and(|m| m["message"]["kind"] == "receive"))
        .collect()
}

/// Whether any table of the pipeline landed.
fn landed(root: &Path) -> bool {
    ["otel_spans", "otel_events"].iter().any(|t| root.join(".contextful/context/research/tables").join(t).exists())
}

/// The bodies spilled beside the spool.
fn spilled(root: &Path) -> usize {
    std::fs::read_dir(spool(root).join("bodies")).map(|d| d.flatten().count()).unwrap_or(0)
}

pub(super) fn two_tables() -> String {
    format!("{}{}{}", delivered("events", json!({ "id": "e1" })), delivered("spans", json!({ "id": "s1" })), delivered("spans", json!({ "id": "s2" })))
}

fn face(root: &Path, public: &str) -> (crate::serve::Listener, String) {
    serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "4", "--project", "research", "--public-key", public])
}

/// A delivery preferring the asynchronous answer reads `202` with its batch key once queued, and no table lands
/// until a consumer drains it.
// spec: run.land.receive-queued@4f619177
// spec: run.land.receive-queued-ask@7c0c33ed
#[test]
fn a_delivery_preferring_async_answers_202_with_its_key_and_lands_nothing_until_drained() {
    let (dir, public) = receiving_face_with(2, 4096);
    let root = dir.path();
    let both = mint_write(root, "otel_*");
    let (_listener, addr) = face(root, &public);

    let (status, receipt) = deliver(&addr, &both, "flush-1", &two_tables(), true);
    assert_eq!(status, 202, "{receipt}");
    assert_eq!((receipt["key"].as_str(), receipt["queued"].as_bool()), (Some("flush-1"), Some(true)), "{receipt}");
    assert!(receipt.get("committed").is_none() || receipt["committed"] == false, "a queued receipt never reads as committed: {receipt}");
    assert_eq!(queued(root).len(), 1);
    assert!(!landed(root), "no table lands");
    let (_, pending) = outcome(&addr, &both, "flush-1");
    assert_eq!(pending["outcome"].as_str(), Some("pending"), "{pending}");

    // A delivery without the preference answers after its commits, as before.
    let body = delivered("spans", json!({ "id": "s9" }));
    let (status, ack) = deliver(&addr, &both, "flush-2", &body, false);
    assert_eq!((status, ack["committed"].as_bool()), (200, Some(true)), "{ack}");
    assert_eq!(queued(root).len(), 1, "a committed answer queues nothing");
}

/// A consumer lands queued batches per credential as one group, pushes, then acknowledges; the outcome route reads
/// `pending` before and each table's committed run after.
// spec: run.land.receive-queued-consume@2d8c930c
// spec: run.land.receive-queued-outcome@76191eaa
#[test]
fn a_drain_lands_queued_batches_as_one_group_and_the_outcome_names_each_committed_run() {
    let (dir, public) = receiving_face_with(4, 4096);
    let root = dir.path();
    let both = mint_write(root, "otel_*");
    let (_listener, addr) = face(root, &public);

    assert_eq!(deliver(&addr, &both, "flush-1", &two_tables(), true).0, 202);
    assert_eq!(deliver(&addr, &both, "flush-2", &delivered("spans", json!({ "id": "s3" })), true).0, 202);
    let (status, pending) = outcome(&addr, &both, "flush-1");
    assert_eq!((status, pending["outcome"].as_str()), (200, Some("pending")), "{pending}");

    let said = drain(root, &public, None);
    assert!(said.contains("2 batches"), "{said}");
    assert!(queued(root).is_empty(), "every landed message is acknowledged");
    assert_eq!(receive_rows(root, "SELECT id FROM otel_spans ORDER BY id"), json!([["s1"], ["s2"], ["s3"]]));
    assert_eq!(receive_rows(root, "SELECT id FROM otel_events"), json!([["e1"]]));

    let (status, landed) = outcome(&addr, &both, "flush-1");
    assert_eq!((status, landed["outcome"].as_str()), (200, Some("committed")), "{landed}");
    let tables: Vec<(&str, &str)> = landed["tables"].as_array().unwrap().iter().map(|t| (t["table"].as_str().unwrap(), t["outcome"].as_str().unwrap())).collect();
    assert_eq!(tables, [("otel_spans", "committed"), ("otel_events", "committed")], "{landed}");
    let span_run = landed["tables"][0]["run_id"].as_str().unwrap().to_string();
    let (_, other) = outcome(&addr, &both, "flush-2");
    assert_eq!(other["tables"][0]["run_id"].as_str(), Some(span_run.as_str()), "one group run lands both batches' spans: {other}");
}

/// A delivery refused before its body queues nothing.
// spec: run.land.receive-queued-admission@690be638
#[test]
fn a_queued_delivery_refused_before_its_body_queues_nothing() {
    let (dir, public) = receiving_face_with(2, 64);
    let root = dir.path();
    let spans = mint_write(root, "otel_spans");
    let both = mint_write(root, "otel_*");
    let (_listener, addr) = face(root, &public);

    let (status, refused) = deliver(&addr, &spans, "flush-1", &two_tables(), true);
    assert_eq!((status, refused["error"]["identifier"].as_str()), (403, Some("ReceiveWriteNotGranted")), "{refused}");
    let (status, refused) = deliver(&addr, &both, "flush-1", &two_tables(), true);
    assert_eq!((status, refused["error"]["identifier"].as_str()), (413, Some("ReceiveBatchTooLarge")), "{refused}");
    assert!(queued(root).is_empty());
}

/// The outcome route refuses a credential lacking `write` over one table of the pipeline.
// spec: run.land.receive-queued-outcome-grant@b3ddf02f
#[test]
fn the_outcome_route_refuses_a_credential_without_write_over_every_table() {
    let (dir, public) = receiving_face_with(2, 4096);
    let root = dir.path();
    let spans = mint_write(root, "otel_spans");
    let both = mint_write(root, "otel_*");
    let (_listener, addr) = face(root, &public);
    assert_eq!(deliver(&addr, &both, "flush-1", &two_tables(), true).0, 202);

    let (status, refused) = outcome(&addr, &spans, "flush-1");
    assert_eq!((status, refused["error"]["identifier"].as_str()), (403, Some("ReceiveWriteNotGranted")), "{refused}");
    assert_eq!(outcome(&addr, &both, "flush-1").0, 200);
}

/// A consumer reaching a batch past its key window records `ReceiveBatchKeyExpired` and lands nothing.
// spec: run.land.receive-queued-window@8b297ca6
#[test]
fn a_batch_reached_past_its_key_window_records_the_expiry_and_lands_nothing() {
    let (dir, public) = receiving_face_with(2, 4096);
    let root = dir.path();
    let both = mint_write(root, "otel_*");
    let (_listener, addr) = face(root, &public);
    assert_eq!(deliver(&addr, &both, "flush-1", &two_tables(), true).0, 202);

    let late = Instant::now_plus(3601);
    drain(root, &public, Some(&late));
    assert!(queued(root).is_empty(), "a recorded refusal acknowledges its message");
    let (_, expired) = outcome(&addr, &both, "flush-1");
    assert_eq!((expired["outcome"].as_str(), expired["error"]["identifier"].as_str()), (Some("refused"), Some("ReceiveBatchKeyExpired")), "{expired}");
    assert!(!landed(root), "no table lands");
}

/// A credential revoked while its batch waits lands no row; its refusal is the batch's outcome.
// spec: run.land.receive-queued-credential@e1a0c6e0
#[test]
fn a_credential_revoked_while_queued_records_its_refusal_and_lands_nothing() {
    let (dir, public) = receiving_face_with(2, 4096);
    let root = dir.path();
    let both = mint_write(root, "otel_*");
    let (_listener, addr) = face(root, &public);
    assert_eq!(deliver(&addr, &both, "flush-1", &two_tables(), true).0, 202);
    assert_eq!(stdout(&run(root, &["token", "revoke", "--principal-class", "delegated"])), "epoch 1");

    drain(root, &public, None);
    assert!(queued(root).is_empty());
    let reader = mint_write(root, "otel_*");
    let (_, refused) = outcome(&addr, &reader, "flush-1");
    assert_eq!((refused["outcome"].as_str(), refused["error"]["identifier"].as_str()), (Some("refused"), Some("AuthorityRevoked")), "{refused}");
    assert!(!landed(root), "no table lands");
}

/// A body past the queue's message cap waits beside the queue under its batch key and leaves once its outcome
/// records.
// spec: run.land.receive-queued-spill@57f73f9c
#[test]
fn a_body_past_the_message_cap_spills_and_its_spill_leaves_once_landed() {
    let (dir, public) = receiving_face_with(2, 4 << 20);
    let root = dir.path();
    let both = mint_write(root, "otel_*");
    let (_listener, addr) = face(root, &public);
    let pad = "x".repeat(1024);
    let body: String = (0..1200).map(|i| delivered("spans", json!({ "id": format!("s{i:04}"), "pad": pad }))).collect();

    let (status, receipt) = deliver(&addr, &both, "big-1", &body, true);
    assert_eq!(status, 202, "{receipt}");
    assert_eq!(spilled(root), 1, "the body waits beside the queue");
    let message = std::fs::read_to_string(&queued(root)[0]).unwrap();
    assert!(message.len() < 4096, "the message carries a reference: {} bytes", message.len());

    drain(root, &public, None);
    assert_eq!(receive_rows(root, "SELECT count(*) FROM otel_spans"), json!([["1200"]]));
    assert_eq!(spilled(root), 0, "the spill leaves once its outcome records");
}

/// A message redelivered after its batch landed replays the batch key: every row lands once.
// spec: run.land.receive-queued-redelivery@638fc650
#[test]
fn a_message_redelivered_after_landing_replays_its_key_and_lands_every_row_once() {
    let (dir, public) = receiving_face_with(2, 4096);
    let root = dir.path();
    let both = mint_write(root, "otel_*");
    let (_listener, addr) = face(root, &public);
    assert_eq!(deliver(&addr, &both, "flush-1", &two_tables(), true).0, 202);
    // The consumer dies after landing and before acknowledging: the message stays queued.
    let message = queued(root).remove(0);
    let copy = std::fs::read(&message).unwrap();

    drain(root, &public, None);
    std::fs::write(&message, &copy).unwrap();
    drain(root, &public, None);
    assert!(queued(root).is_empty());
    assert_eq!(receive_rows(root, "SELECT id FROM otel_spans ORDER BY id"), json!([["s1"], ["s2"]]));
    let (_, landed) = outcome(&addr, &both, "flush-1");
    assert_eq!(landed["outcome"].as_str(), Some("committed"), "{landed}");
}

/// The system clock `secs` from now, as `--now` reads it.
struct Instant;

impl Instant {
    fn now_plus(secs: i64) -> String {
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 + secs;
        contextful_core::time::Instant::from_unix_secs(at).unwrap().to_rfc3339()
    }
}
