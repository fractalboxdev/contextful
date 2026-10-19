//! `contextful pipeline receive`: a pushed batch lands through the pull land path.
#![cfg(feature = "data-plane")]

use crate::pipeline::{cf, ok, project, stderr};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};

const NOW: &str = "2030-01-01T00:00:00Z";

fn receiving(bytes: u64, depth: u32) -> tempfile::TempDir {
    project(&format!(
        "[[pipeline]]\nid = \"otel\"\ntables = [\"spans\", {{ name = \"events\", columns = {{ id = \"Utf8\", n = \"Int64\" }} }}]\n[pipeline.source]\nname = \"receive\"\nconfig = {{ format = \"jsonl\", max_batch_bytes = {bytes}, max_queue_depth = {depth}, key_retention_secs = 3600 }}\n"
    ))
}

fn receiver(dir: &Path, key: &str) -> Command {
    receiver_at(dir, key, NOW)
}

fn receiver_at(dir: &Path, key: &str, now: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_contextful"));
    command
        .args(["pipeline", "receive", "otel", "--key", key, "--project", "research", "--site-id", "site-a", "--now", now])
        .current_dir(dir)
        .env_remove("CONTEXTFUL_NODE_ID")
        .env_remove("CONTEXTFUL_SECRETS_BACKEND");
    command
}

fn receive(dir: &Path, key: &str, body: &str) -> Output {
    send(receiver(dir, key), body)
}

fn send(mut command: Command, body: &str) -> Output {
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    // A refused sender may close its stdin before reading every byte.
    let _ = child.stdin.take().unwrap().write_all(body.as_bytes());
    child.wait_with_output().unwrap()
}

fn answer(out: &Output) -> serde_json::Value {
    serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap_or_else(|e| panic!("{e}: {}{}", String::from_utf8_lossy(&out.stdout), stderr(out)))
}

fn rows(dir: &Path, sql: &str) -> serde_json::Value {
    let out: serde_json::Value = serde_json::from_str(&ok(&cf(dir, &["query", "--json", "--project", "research", sql]))).unwrap();
    out["rows"].clone()
}

fn line(table: &str, row: serde_json::Value) -> String {
    format!("{}\n", serde_json::json!({ "table": table, "row": row }))
}

/// A delivered batch answers after every touched table commits, in declaration order, naming the batch run id.
// spec: run.land.receive-ack@dc8272d4
#[test]
fn a_batch_over_two_tables_answers_after_both_commit_naming_its_run_id() {
    let dir = receiving(4096, 2);
    ok(&cf(dir.path(), &["pipeline", "validate"]));
    let body = format!("{}{}{}", line("events", serde_json::json!({ "id": "e1" })), line("spans", serde_json::json!({ "id": "s1" })), line("spans", serde_json::json!({ "id": "s2" })));
    let out = receive(dir.path(), "flush-1", &body);
    assert!(out.status.success(), "{}", stderr(&out));
    let ack = answer(&out);
    let run_id = ack["run_id"].as_str().unwrap().to_string();
    assert!(run_id.starts_with("receive-"), "{ack}");
    assert_eq!(ack["committed"], true);
    let tables: Vec<&str> = ack["tables"].as_array().unwrap().iter().map(|t| t["table"].as_str().unwrap()).collect();
    assert_eq!(tables, ["otel_spans", "otel_events"], "declaration order");
    for t in ack["tables"].as_array().unwrap() {
        assert_eq!(t["outcome"], "committed", "{ack}");
        assert!(t["run_id"].as_str().unwrap().starts_with(&run_id), "{ack}");
    }
    assert_eq!(rows(dir.path(), "SELECT id FROM otel_spans ORDER BY id"), serde_json::json!([["s1"], ["s2"]]));
    assert_eq!(rows(dir.path(), "SELECT id FROM otel_events"), serde_json::json!([["e1"]]));
}

/// A resend under a key whose tables all committed names the original run id and lands nothing; a key whose run
/// committed one of two tables lands the other on resend.
// spec: run.land.receive-batch-key@313f16ce
#[test]
fn a_resent_key_replays_committed_tables_and_lands_the_rest() {
    let dir = receiving(4096, 2);
    let body = format!("{}{}", line("spans", serde_json::json!({ "id": "s1" })), line("events", serde_json::json!({ "id": "e1" })));
    let first = answer(&receive(dir.path(), "flush-1", &body));
    let again = receive(dir.path(), "flush-1", &body);
    assert!(again.status.success(), "{}", stderr(&again));
    let again = answer(&again);
    assert_eq!(again["run_id"], first["run_id"]);
    let outcomes: Vec<(&str, &str)> = again["tables"].as_array().unwrap().iter().map(|t| (t["table"].as_str().unwrap(), t["outcome"].as_str().unwrap())).collect();
    assert_eq!(outcomes, [("otel_spans", "replayed"), ("otel_events", "replayed")]);
    assert_eq!(rows(dir.path(), "SELECT id FROM otel_spans"), serde_json::json!([["s1"]]));
    assert_eq!(rows(dir.path(), "SELECT id FROM otel_events"), serde_json::json!([["e1"]]));

    // `events.n` is declared `Int64`, so a text `n` is irreconcilable: `events` fails while `spans` commits.
    let broken = format!("{}{}", line("spans", serde_json::json!({ "id": "s2" })), line("events", serde_json::json!({ "id": "e2", "n": "many" })));
    let partial = receive(dir.path(), "flush-2", &broken);
    assert!(!partial.status.success(), "{}", String::from_utf8_lossy(&partial.stdout));
    let partial = answer(&partial);
    // A schema-incompatible table is no retryable class, so the answer tells the sender not to resend the bytes.
    assert_eq!((partial["committed"].clone(), partial["retryable"].clone()), (serde_json::json!(false), serde_json::json!(false)), "{partial}");
    let mended = format!("{}{}", line("spans", serde_json::json!({ "id": "s2" })), line("events", serde_json::json!({ "id": "e2" })));
    let resent = receive(dir.path(), "flush-2", &mended);
    assert!(resent.status.success(), "{}", stderr(&resent));
    let resent = answer(&resent);
    assert_eq!(resent["run_id"], partial["run_id"]);
    let outcomes: Vec<(&str, &str)> = resent["tables"].as_array().unwrap().iter().map(|t| (t["table"].as_str().unwrap(), t["outcome"].as_str().unwrap())).collect();
    assert_eq!(outcomes, [("otel_spans", "replayed"), ("otel_events", "committed")]);
    assert_eq!(rows(dir.path(), "SELECT id FROM otel_spans ORDER BY id"), serde_json::json!([["s1"], ["s2"]]));
    assert_eq!(rows(dir.path(), "SELECT id FROM otel_events ORDER BY id"), serde_json::json!([["e1"], ["e2"]]));
}

/// A delivered batch enters at the pull position, so a credential-shaped cell passes the secret guard before the
/// recorded pull and lands masked.
// spec: run.land.receive-entry@2e83385c
#[test]
fn a_delivered_credential_lands_masked_and_no_stored_byte_holds_it() {
    const KEY: &str = "zK9s8d7f6g5h";
    let dir = receiving(4096, 2);
    let out = receive(dir.path(), "flush-1", &line("spans", serde_json::json!({ "id": "s1", "note": format!("x-api-key: {KEY}") })));
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(rows(dir.path(), "SELECT note FROM otel_spans"), serde_json::json!([["x-api-key: [REDACTED:secret]"]]));
    let mut pending = vec![dir.path().join(".contextful")];
    let (mut files, mut holding) = (0, Vec::new());
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            pending.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
        } else {
            files += 1;
            if std::fs::read(&path).unwrap().windows(KEY.len()).any(|w| w == KEY.as_bytes()) {
                holding.push(path);
            }
        }
    }
    assert!(files > 0, "the batch landed files");
    assert_eq!(holding, Vec::<std::path::PathBuf>::new(), "no stored byte holds the credential");
}

/// A body past `max_batch_bytes` raises `ReceiveBatchTooLarge`, non-retryable, and no table lands.
// spec: run.land.receive-batch-too-large@6de6256c
#[test]
fn a_body_one_byte_past_the_bound_lands_nothing() {
    let body = line("spans", serde_json::json!({ "id": "s1" }));
    let dir = receiving(body.len() as u64 - 1, 2);
    let out = receive(dir.path(), "flush-1", &body);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("ReceiveBatchTooLarge"), "{}", stderr(&out));
    assert_eq!(answer(&out)["retryable"], false);
    assert!(!dir.path().join(".contextful/context/research/tables/otel_spans").exists(), "no table lands");
    // At the bound the same body lands.
    let dir = receiving(body.len() as u64, 2);
    assert!(receive(dir.path(), "flush-1", &body).status.success());
}

/// A batch arriving while every queue slot of its pipeline is held raises `ReceiveBackpressure`, retryable, before
/// reading its body, and its answer carries a 1 s retry interval.
// spec: run.land.receive-backpressure@92c0c916
// spec: run.land.receive-retry-after@7811fd03
#[test]
fn a_third_batch_past_two_held_slots_is_told_to_retry_after_one_second() {
    let dir = receiving(4096, 2);
    let mut held = Vec::new();
    for key in ["flush-1", "flush-2"] {
        let mut child = receiver(dir.path(), key).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let mut err = BufReader::new(child.stderr.take().unwrap());
        let mut queued = String::new();
        err.read_line(&mut queued).unwrap();
        assert!(queued.contains("queue slot"), "{queued}");
        held.push((child, err));
    }
    let refused = receive(dir.path(), "flush-3", &line("spans", serde_json::json!({ "id": "s3" })));
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("ReceiveBackpressure"), "{}", stderr(&refused));
    let ack = answer(&refused);
    assert_eq!((ack["retryable"].clone(), ack["retry_after_ms"].clone()), (serde_json::json!(true), serde_json::json!(1000)), "{ack}");
    // The held batches land once their bodies arrive.
    for (i, (mut child, _err)) in held.into_iter().enumerate() {
        child.stdin.take().unwrap().write_all(line("spans", serde_json::json!({ "id": format!("s{i}") })).as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
    }
    assert_eq!(rows(dir.path(), "SELECT id FROM otel_spans ORDER BY id"), serde_json::json!([["s0"], ["s1"]]));
    assert!(receive(dir.path(), "flush-3", &line("spans", serde_json::json!({ "id": "s3" }))).status.success(), "a freed slot admits the retry");
}

/// One group of a pipeline's batches lands at a time: a batch whose body arrived waits while another holds the land
/// lock, and commits once it is released.
// spec: run.land.receive-queue@b983066a
#[test]
fn a_batch_waits_for_the_land_lock_another_batch_holds() {
    let dir = receiving(4096, 2);
    let queue = dir.path().join(".contextful/run/research/receive/otel");
    std::fs::create_dir_all(&queue).unwrap();
    let lock = std::fs::File::options().read(true).write(true).create(true).truncate(false).open(queue.join("land.lock")).unwrap();
    lock.lock().unwrap();
    let mut child = receiver(dir.path(), "flush-1").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let mut queued = String::new();
    err.read_line(&mut queued).unwrap();
    assert!(queued.contains("queue slot"), "{queued}");
    child.stdin.take().unwrap().write_all(line("spans", serde_json::json!({ "id": "s1" })).as_bytes()).unwrap();
    // An unlocked batch lands well inside this wait.
    std::thread::sleep(std::time::Duration::from_secs(5));
    assert!(child.try_wait().unwrap().is_none(), "the batch waits while the land lock is held");
    assert!(!dir.path().join(".contextful/context/research/tables/otel_spans").exists(), "no row lands while the land lock is held");
    lock.unlock().unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(answer(&out)["committed"], true);
    assert_eq!(rows(dir.path(), "SELECT id FROM otel_spans"), serde_json::json!([["s1"]]));
}

/// The bound a delivered batch waits for the push guard; `push_wait_secs` declares three times it.
const RECEIVE_PUSH_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// A project syncing to a fresh `file://` bucket with `push_wait_secs = 90`, its `[sync]` block carrying `extra`.
fn syncing(bucket: &Path, extra: &str) -> tempfile::TempDir {
    queued_project(Some(&format!(
        "endpoint = \"file://{}\"\nbucket = \"context-team\"\nprefix = \"team\"\ncoordination = \"single-writer\"\npush_wait_secs = 90\n{extra}",
        bucket.display()
    )))
}

/// Hold the store's push guard, as a push of the store on the machine does.
fn hold_push_guard(dir: &Path) -> std::fs::File {
    let guard = std::fs::File::options().read(true).write(true).create(true).truncate(false).open(dir.join(".contextful/context/research/.push.lock")).unwrap();
    guard.lock().unwrap();
    guard
}

/// Deliver one batch under `key` while the push guard is held: the answer and the time it took.
fn receive_timed(dir: &Path, key: &str, id: &str) -> (Output, std::time::Duration) {
    let started = std::time::Instant::now();
    let out = receive(dir, key, &line("spans", serde_json::json!({ "id": id })));
    (out, started.elapsed())
}

/// A delivered batch whose `push_after_run` meets a held push guard answers retryable after 30 s, not after the
/// declared `push_wait_secs`; a resend under its key once the guard frees pushes the committed table.
// spec: run.land.receive-push-wait@746d4566
#[test]
fn a_held_push_guard_answers_a_delivered_batch_retryable_within_thirty_seconds() {
    let bucket = tempfile::tempdir().unwrap();
    let dir = syncing(bucket.path(), "push_after_run = true\n");
    let guard = hold_push_guard(dir.path());
    let (out, waited) = receive_timed(dir.path(), "flush-1", "s1");
    assert!(!out.status.success(), "{}", stderr(&out));
    let ack = answer(&out);
    assert_eq!(ack["retryable"], true, "{ack}");
    assert!(stderr(&out).contains("SyncPushInFlight"), "{}", stderr(&out));
    assert!(waited >= RECEIVE_PUSH_WAIT && waited < RECEIVE_PUSH_WAIT * 2, "answered after {waited:?}");
    assert_eq!(files_under(bucket.path()), Vec::<std::path::PathBuf>::new(), "no object reaches the bucket");
    guard.unlock().unwrap();
    let resent = receive(dir.path(), "flush-1", &line("spans", serde_json::json!({ "id": "s1" })));
    assert!(resent.status.success(), "{}", stderr(&resent));
    let resent = answer(&resent);
    assert_eq!(resent["tables"][0]["outcome"], "replayed", "{resent}");
    assert!(files_under(bucket.path()).iter().any(|p| p.to_string_lossy().contains("otel_spans")), "the resend pushes the committed table");
    assert_eq!(rows(dir.path(), "SELECT id FROM otel_spans"), serde_json::json!([["s1"]]));
}

/// A delivered batch whose `pull_before_run` meets a held push guard answers retryable after 30 s and lands no table.
#[test]
fn a_held_push_guard_refuses_a_pulling_batch_retryable_within_thirty_seconds() {
    let bucket = tempfile::tempdir().unwrap();
    let dir = syncing(bucket.path(), "pull_before_run = true\n");
    let guard = hold_push_guard(dir.path());
    let (out, waited) = receive_timed(dir.path(), "flush-1", "s1");
    assert!(!out.status.success(), "{}", stderr(&out));
    let ack = answer(&out);
    assert_eq!((ack["retryable"].clone(), ack["committed"].clone()), (serde_json::json!(true), serde_json::json!(false)), "{ack}");
    assert!(waited >= RECEIVE_PUSH_WAIT && waited < RECEIVE_PUSH_WAIT * 2, "answered after {waited:?}");
    assert!(!dir.path().join(".contextful/context/research/tables/otel_spans").exists(), "no table lands");
    guard.unlock().unwrap();
    assert!(receive(dir.path(), "flush-1", &line("spans", serde_json::json!({ "id": "s1" }))).status.success(), "a freed guard admits the resend");
}

/// A `receive` pipeline declaring a `schedule` fails validation: a sender, not the cadence, starts its runs.
// spec: connector.source.receive-unscheduled@0e261316
#[test]
fn a_scheduled_receive_pipeline_fails_validation() {
    let dir = project("[[pipeline]]\nid = \"otel\"\nschedule = \"every 1h\"\ntables = [\"spans\"]\n[pipeline.source]\nname = \"receive\"\nconfig = { format = \"jsonl\", max_batch_bytes = 4096, max_queue_depth = 2, key_retention_secs = 3600 }\n");
    let out = cf(dir.path(), &["pipeline", "validate"]);
    assert!(!out.status.success(), "a scheduled receive pipeline validates");
    assert!(stderr(&out).contains("schedule"), "{}", stderr(&out));
}

/// A batch key replays only within the source's `key_retention_secs` of its first attempt; a later batch under it
/// raises `ReceiveBatchKeyExpired`, non-retryable, and no row lands.
// spec: run.land.receive-key-expired@89a7069a
#[test]
fn a_resend_past_the_key_retention_window_is_refused_and_lands_nothing() {
    let dir = receiving(4096, 2);
    let body = line("spans", serde_json::json!({ "id": "s1" }));
    let first = receive(dir.path(), "flush-1", &body);
    assert!(first.status.success(), "{}", stderr(&first));
    let within = send(receiver_at(dir.path(), "flush-1", "2030-01-01T01:00:00Z"), &body);
    assert!(within.status.success(), "{}", stderr(&within));
    assert_eq!(answer(&within)["tables"][0]["outcome"], "replayed", "a resend at the window's edge replays");
    let late = send(receiver_at(dir.path(), "flush-1", "2030-01-01T01:00:01Z"), &line("spans", serde_json::json!({ "id": "s2" })));
    assert!(!late.status.success(), "{}", String::from_utf8_lossy(&late.stdout));
    assert!(stderr(&late).contains("ReceiveBatchKeyExpired"), "{}", stderr(&late));
    let refused = answer(&late);
    assert_eq!((refused["committed"].clone(), refused["retryable"].clone()), (serde_json::json!(false), serde_json::json!(false)), "{refused}");
    assert_eq!(rows(dir.path(), "SELECT id FROM otel_spans"), serde_json::json!([["s1"]]));
    let fresh = send(receiver_at(dir.path(), "flush-2", "2030-01-01T01:00:01Z"), &line("spans", serde_json::json!({ "id": "s2" })));
    assert!(fresh.status.success(), "a new key lands: {}", stderr(&fresh));
    assert_eq!(rows(dir.path(), "SELECT id FROM otel_spans ORDER BY id"), serde_json::json!([["s1"], ["s2"]]));
}

/// A credential lacking `write` over one declared table of the pipeline raises `ReceiveWriteNotGranted` before the
/// body is read: the receiver exits while its stdin stays open, and no table lands.
// spec: run.land.receive-write-not-granted@51a4ea13
#[test]
fn a_token_without_write_on_one_table_is_refused_before_the_body_is_read() {
    let dir = receiving(4096, 2);
    std::fs::write(dir.path().join(".contextful/issuance.toml"), "default_audience = \"contextful://research\"\nmax_lifetime_secs = 3600\n").unwrap();
    let public = ok(&cf(dir.path(), &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let mint = |table: &str| {
        ok(&cf(dir.path(), &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@example.test", "--ttl", "600", "--action", "write", "--table", table]))
    };
    let admitted = |token: &str| {
        let mut command = receiver(dir.path(), "flush-1");
        command.args(["--public-key", &public, "--audience", "contextful://research"]).env("CONTEXTFUL_TOKEN", token);
        command
    };
    let mut child = admitted(&mint("otel_spans")).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let open = child.stdin.take().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while child.try_wait().unwrap().is_none() {
        assert!(std::time::Instant::now() < deadline, "the receiver waits on a body it should refuse before reading");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let out = child.wait_with_output().unwrap();
    drop(open);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("ReceiveWriteNotGranted") && stderr(&out).contains("otel_events"), "{}", stderr(&out));
    assert_eq!(answer(&out)["retryable"], false);
    assert!(!dir.path().join(".contextful/context/research/tables/otel_spans").exists(), "no table lands");
    let granted = send(admitted(&mint("otel_*")), &line("spans", serde_json::json!({ "id": "s1" })));
    assert!(granted.status.success(), "{}", stderr(&granted));
    assert_eq!(rows(dir.path(), "SELECT id FROM otel_spans"), serde_json::json!([["s1"]]));
}

/// A receiver killed after recording a delivered batch leaves it pending; the resend under the key resumes the
/// recorded pull, and every landed cell but the run id matches the uninterrupted run's, the credential masked.
// spec: run.land.receive-resume@94ee920e
#[test]
fn a_resend_after_the_receiver_dies_lands_what_the_uninterrupted_run_lands() {
    let body = format!("{}{}", line("spans", serde_json::json!({ "id": "s1", "note": "x-api-key: zK9s8d7f6g5h" })), line("spans", serde_json::json!({ "id": "s2", "note": "plain" })));
    // Both runs land at one instant, so the ingest time agrees and only the attempt's run id differs.
    const LATER: &str = "2030-01-01T00:01:00Z";
    let whole = receiving(4096, 2);
    let out = send(receiver_at(whole.path(), "flush-1", LATER), &body);
    assert!(out.status.success(), "{}", stderr(&out));

    let resumed = receiving(4096, 2);
    let table = resumed.path().join(".contextful/context/research/tables/otel_spans");
    std::fs::create_dir_all(&table).unwrap();
    let commit = std::fs::File::options().read(true).write(true).create(true).truncate(false).open(table.join("commit.lock")).unwrap();
    commit.lock().unwrap();
    let mut child = receiver(resumed.path(), "flush-1").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(body.as_bytes()).unwrap();
    // The run stages its parts after the recorded pull and then waits on the held commit lock.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
    let staged = || {
        let mut pending = vec![table.clone()];
        while let Some(path) = pending.pop() {
            if path.is_dir() {
                pending.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
            } else if path.extension().is_some_and(|e| e == "parquet") {
                return true;
            }
        }
        false
    };
    while !staged() {
        assert!(std::time::Instant::now() < deadline, "the run never staged");
        assert!(child.try_wait().unwrap().is_none(), "the receiver ended before its commit");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    commit.unlock().unwrap();
    drop(commit);

    // The resend arrives once the dead receiver's 30 s owner lease has lapsed.
    // The resend carries other bytes: the recorded pull, not the resent body, is what lands.
    let resent = body.replace("plain", "resent");
    let out = send(receiver_at(resumed.path(), "flush-1", LATER), &resent);
    assert!(out.status.success(), "{}", stderr(&out));
    let ack = answer(&out);
    assert_eq!(ack["tables"][0]["outcome"], "committed", "{ack}");
    assert_eq!(ack["run_id"], answer(&send(receiver_at(whole.path(), "flush-1", LATER), &body))["run_id"]);
    let sql = "SELECT * EXCLUDE (_run_id) FROM otel_spans ORDER BY id";
    let landed = rows(resumed.path(), sql);
    assert_eq!(landed.as_array().map(Vec::len), Some(2), "{landed}");
    assert_eq!(landed, rows(whole.path(), sql), "the resumed run lands the uninterrupted run's cells");
    assert_eq!(rows(resumed.path(), "SELECT note FROM otel_spans ORDER BY id"), serde_json::json!([["x-api-key: [REDACTED:secret]"], ["plain"]]));
}

/// The seed of the queued-receive schedule: rounds of sixteen one-row batches.
pub(crate) const QUEUED_SEED: u64 = 0x5eed_0101;

/// The batches each round of the queued-receive schedule holds behind the land lock.
pub(crate) const QUEUED_WRITERS: usize = 16;

/// The rounds of the queued-receive schedule: two under a measure run, one otherwise.
pub(crate) fn queued_rounds() -> usize {
    if std::env::var_os(contextful_eval::record::MEASURE_DIR_VAR).is_some() { 2 } else { 1 }
}

/// A project receiving into `otel` through a queue 64 deep, its store config carrying the `sync` block when given.
pub(crate) fn queued_project(sync: Option<&str>) -> tempfile::TempDir {
    let dir = receiving(4096, 64);
    if let Some(sync) = sync {
        std::fs::write(dir.path().join(".contextful/context/research/config.toml"), format!("[node]\nid = \"ingest-a\"\n\n[sync]\n{sync}")).unwrap();
    }
    dir
}

/// Deliver `rounds` rounds of `writers` one-row `spans` batches, each round queued behind a held land lock and
/// released together, every receiver carrying `env`; the batches delivered. Row ids derive from `seed`.
pub(crate) fn queue_behind_the_land_lock(dir: &Path, rounds: usize, writers: usize, seed: u64, env: &[(&str, &str)]) -> u64 {
    let queue = dir.join(".contextful/run/research/receive/otel");
    std::fs::create_dir_all(&queue).unwrap();
    let lock = std::fs::File::options().read(true).write(true).create(true).truncate(false).open(queue.join("land.lock")).unwrap();
    for round in 0..rounds {
        lock.lock().unwrap();
        let held: Vec<_> = (0..writers)
            .map(|w| {
                let n = (round * writers + w) as u64;
                let mut child = receiver(dir, &format!("flush-{n}")).envs(env.iter().copied()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
                let mut err = BufReader::new(child.stderr.take().unwrap());
                let mut queued = String::new();
                err.read_line(&mut queued).unwrap();
                assert!(queued.contains("queue slot"), "{queued}");
                let id = format!("{:016x}", seed.wrapping_mul(n + 1));
                child.stdin.take().unwrap().write_all(line("spans", serde_json::json!({ "id": id })).as_bytes()).unwrap();
                (child, err)
            })
            .collect();
        // Every batch of the round spools before the lock releases, so one group holds the round.
        let spool = queue.join("spool");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let spooled = || std::fs::read_dir(&spool).map(|d| d.filter(|e| e.as_ref().is_ok_and(|e| e.path().extension().is_some_and(|x| x == "batch"))).count()).unwrap_or(0);
        while spooled() < writers {
            assert!(std::time::Instant::now() < deadline, "{} of {writers} batches spooled behind the land lock", spooled());
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        lock.unlock().unwrap();
        for (child, mut err) in held {
            let out = child.wait_with_output().unwrap();
            let mut rest = String::new();
            let _ = std::io::Read::read_to_string(&mut err, &mut rest);
            assert!(out.status.success(), "{rest}");
            assert_eq!(answer(&out)["committed"], true, "{rest}");
        }
    }
    (rounds * writers) as u64
}

/// The batches spooled in the pipeline `otel`'s queue and not yet claimed by a leader.
fn spooled(dir: &Path) -> usize {
    let spool = dir.join(".contextful/run/research/receive/otel/spool");
    std::fs::read_dir(spool).map(|d| d.filter(|e| e.as_ref().is_ok_and(|e| e.path().extension().is_some_and(|x| x == "batch"))).count()).unwrap_or(0)
}

/// Hold the pipeline `otel`'s land lock, start a receiver of one `spans` row under `key`, and wait until its batch
/// spools behind the lock.
fn spooled_behind_the_land_lock(dir: &Path, key: &str, id: &str) -> (std::fs::File, std::process::Child) {
    let queue = dir.join(".contextful/run/research/receive/otel");
    std::fs::create_dir_all(&queue).unwrap();
    let lock = std::fs::File::options().read(true).write(true).create(true).truncate(false).open(queue.join("land.lock")).unwrap();
    lock.lock().unwrap();
    let mut child = receiver(dir, key).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(line("spans", serde_json::json!({ "id": id })).as_bytes()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while spooled(dir) < 1 {
        assert!(std::time::Instant::now() < deadline, "the batch never spooled behind the land lock");
        assert!(child.try_wait().unwrap().is_none(), "the receiver ended before spooling");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    (lock, child)
}

/// A receiver killed after spooling its batch behind the land lock leaves the batch spooled: the next batch's
/// leader lands it in the same group, and a resend under the dead receiver's key replays that group's run.
// spec: run.land.receive-spool@8c374dc9
#[test]
fn a_batch_spooled_by_a_dead_receiver_lands_in_the_next_group() {
    let dir = receiving(4096, 4);
    let (lock, mut child) = spooled_behind_the_land_lock(dir.path(), "flush-1", "s1");
    child.kill().unwrap();
    child.wait().unwrap();
    lock.unlock().unwrap();
    drop(lock);

    let out = receive(dir.path(), "flush-2", &line("spans", serde_json::json!({ "id": "s2" })));
    assert!(out.status.success(), "{}", stderr(&out));
    let ack = answer(&out);
    assert_eq!(ack["tables"][0]["outcome"], "committed", "{ack}");
    assert_eq!(rows(dir.path(), "SELECT id FROM otel_spans ORDER BY id"), serde_json::json!([["s1"], ["s2"]]), "the leader lands the orphaned batch");
    assert_eq!(rows(dir.path(), "SELECT count(DISTINCT _run_id) FROM otel_spans"), serde_json::json!([["1"]]), "one group run holds both batches");
    assert_eq!(spooled(dir.path()), 0, "the group leaves no batch spooled");

    let again = receive(dir.path(), "flush-1", &line("spans", serde_json::json!({ "id": "s1" })));
    assert!(again.status.success(), "{}", stderr(&again));
    let again = answer(&again);
    assert_eq!((again["tables"][0]["outcome"].clone(), again["tables"][0]["run_id"].clone()), (serde_json::json!("replayed"), ack["tables"][0]["run_id"].clone()), "{again}");
    assert_eq!(rows(dir.path(), "SELECT id FROM otel_spans ORDER BY id"), serde_json::json!([["s1"], ["s2"]]), "the resend lands nothing");
}

/// A group whose run fails with a class that does not retry lands each member alone: the well-formed batch commits
/// and answers committed, and only the batch whose row sets a reserved column answers non-retryable.
// spec: run.land.receive-group-isolation@1931267b
#[test]
fn a_bad_row_in_a_group_fails_only_its_own_batch() {
    let dir = receiving(4096, 4);
    let queue = dir.path().join(".contextful/run/research/receive/otel");
    std::fs::create_dir_all(&queue).unwrap();
    let lock = std::fs::File::options().read(true).write(true).create(true).truncate(false).open(queue.join("land.lock")).unwrap();
    lock.lock().unwrap();
    let spawn = |key: &str, row: serde_json::Value| {
        let mut child = receiver(dir.path(), key).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(line("spans", row).as_bytes()).unwrap();
        child
    };
    let good = spawn("flush-1", serde_json::json!({ "id": "s1" }));
    let bad = spawn("flush-2", serde_json::json!({ "id": "s2", "_modality": "7" }));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while spooled(dir.path()) < 2 {
        assert!(std::time::Instant::now() < deadline, "both batches spool behind the land lock");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    lock.unlock().unwrap();
    drop(lock);

    let good = good.wait_with_output().unwrap();
    assert!(good.status.success(), "{}", stderr(&good));
    let ack = answer(&good);
    assert_eq!((ack["committed"].clone(), ack["tables"][0]["outcome"].clone()), (serde_json::json!(true), serde_json::json!("committed")), "{ack}");
    let bad = bad.wait_with_output().unwrap();
    assert!(!bad.status.success(), "{}", String::from_utf8_lossy(&bad.stdout));
    let refused = answer(&bad);
    assert_eq!((refused["committed"].clone(), refused["retryable"].clone()), (serde_json::json!(false), serde_json::json!(false)), "{refused}");
    assert_eq!(rows(dir.path(), "SELECT id FROM otel_spans"), serde_json::json!([["s1"]]), "only the well-formed batch lands");
}

/// A spooled batch that neither leads nor joins a group within the 30 s commit wait withdraws its spool and answers
/// `ReceiveBackpressure`, retryable; the resend lands once the land lock frees.
// spec: run.land.receive-wait@0252fe6a
#[test]
fn a_batch_no_group_claims_within_the_wait_withdraws_and_answers_retryable() {
    let dir = receiving(4096, 2);
    let (lock, child) = spooled_behind_the_land_lock(dir.path(), "flush-1", "s1");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut child = child;
    while child.try_wait().unwrap().is_none() {
        assert!(std::time::Instant::now() < deadline, "the batch outwaits the commit wait behind the held land lock");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(stderr(&out).contains("ReceiveBackpressure"), "{}", stderr(&out));
    let ack = answer(&out);
    assert_eq!((ack["committed"].clone(), ack["retryable"].clone(), ack["retry_after_ms"].clone()), (serde_json::json!(false), serde_json::json!(true), serde_json::json!(1000)), "{ack}");
    assert_eq!(spooled(dir.path()), 0, "the withdrawn batch leaves the spool");
    lock.unlock().unwrap();
    drop(lock);
    let resent = receive(dir.path(), "flush-1", &line("spans", serde_json::json!({ "id": "s1" })));
    assert!(resent.status.success(), "{}", stderr(&resent));
    assert_eq!(rows(dir.path(), "SELECT id FROM otel_spans"), serde_json::json!([["s1"]]));
}

/// A commit outwaiting its table's commit lock past the 30 s commit wait raises `StoreCommitLockTimeout`, which the
/// batch answers retryable; the resend under its key lands once the lock frees.
// spec: store.reserve.commit-wait@f56001f5
#[test]
fn a_commit_outwaiting_the_commit_lock_answers_retryable() {
    let dir = receiving(4096, 2);
    let table = dir.path().join(".contextful/context/research/tables/otel_spans");
    std::fs::create_dir_all(&table).unwrap();
    let commit = std::fs::File::options().read(true).write(true).create(true).truncate(false).open(table.join("commit.lock")).unwrap();
    commit.lock().unwrap();
    let started = std::time::Instant::now();
    let out = receive(dir.path(), "flush-1", &line("spans", serde_json::json!({ "id": "s1" })));
    assert!(started.elapsed() >= std::time::Duration::from_secs(30), "the commit waits out the 30 s commit wait");
    assert!(!out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(stderr(&out).contains("StoreCommitLockTimeout"), "{}", stderr(&out));
    let ack = answer(&out);
    assert_eq!((ack["committed"].clone(), ack["retryable"].clone()), (serde_json::json!(false), serde_json::json!(true)), "{ack}");
    commit.unlock().unwrap();
    drop(commit);
    let resent = receive(dir.path(), "flush-1", &line("spans", serde_json::json!({ "id": "s1" })));
    assert!(resent.status.success(), "{}", stderr(&resent));
    assert_eq!(rows(dir.path(), "SELECT id FROM otel_spans"), serde_json::json!([["s1"]]));
}

/// Every file under `dir`, recursively.
fn files_under(dir: &Path) -> Vec<std::path::PathBuf> {
    let (mut pending, mut found) = (vec![dir.to_path_buf()], Vec::new());
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            pending.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
        } else {
            found.push(path);
        }
    }
    found
}

/// Batches queued behind the land lock: the commits, and the part files plus run manifests, landed per 1000
/// one-row updates.
// spec: run.land.receive-group@bf98bdda
#[test]
fn batches_queued_behind_the_land_lock_commit_per_group() {
    let dir = queued_project(None);
    let updates = queue_behind_the_land_lock(dir.path(), queued_rounds(), QUEUED_WRITERS, QUEUED_SEED, &[]);
    let landed = rows(dir.path(), "SELECT count(*), count(DISTINCT _run_id) FROM otel_spans");
    let count = |cell: &serde_json::Value| cell.as_str().and_then(|n| n.parse::<u64>().ok());
    assert_eq!(count(&landed[0][0]), Some(updates), "every queued row lands once: {landed}");
    let commits = count(&landed[0][1]).unwrap();
    assert!((1..=updates).contains(&commits), "{landed}");
    let parts = files_under(&dir.path().join(".contextful/context/research/tables/otel_spans"))
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "parquet") || p.file_name().is_some_and(|n| n == "_manifest.json"))
        .count() as u64;
    assert!(parts >= commits, "each commit lands at least one part: {parts} for {commits}");
    let per_1000 = |n: u64| n as f64 * 1000.0 / updates as f64;
    eprintln!("{updates} queued updates: {commits} commits, {parts} part files and run manifests");
    assert!(per_1000(commits) <= 63.0, "a round queued behind the land lock commits as one group: {commits} commits for {updates} updates");
    contextful_eval::record::emit("receive-group-commits", per_1000(commits), updates, QUEUED_SEED);
    contextful_eval::record::emit("receive-group-parts", per_1000(parts), updates, QUEUED_SEED);
}

/// The point a group's leader dies at.
#[derive(Debug, Clone, Copy)]
enum Crash {
    /// The group run opened: the members' scopes name the group and its run row is being recorded.
    Opened,
    /// The group run staged its parts and waits to reserve its commit.
    Staged,
    /// The group run's manifest committed and its run closes before any member is answered.
    Committed,
}

/// Whether a run record waits on `_runs`' commit lock: a run directory holding no manifest.
fn recording(tables: &Path) -> bool {
    let runs = tables.join("_runs/data/runs");
    let Ok(entries) = std::fs::read_dir(runs) else { return false };
    entries.flatten().flat_map(|run| std::fs::read_dir(run.path()).into_iter().flatten().flatten()).any(|node| !node.path().join("_manifest.json").exists())
}

/// Whether a parquet file under `dir` sits inside a run's staging directory.
fn staging(dir: &Path) -> bool {
    files_under(dir).iter().any(|p| p.extension().is_some_and(|e| e == "parquet") && p.components().any(|c| c.as_os_str() == "stage.staging"))
}

fn held(path: &Path) -> std::fs::File {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = std::fs::File::options().read(true).write(true).create(true).truncate(false).open(path).unwrap();
    file.lock().unwrap();
    file
}

/// Spool `flush-1` and `flush-2` behind the land lock, release it, and kill both receivers once the group's leader
/// reaches `crash`.
fn crash_a_group(dir: &Path, crash: Crash) {
    let tables = dir.join(".contextful/context/research/tables");
    let land = held(&dir.join(".contextful/run/research/receive/otel/land.lock"));
    let mut children: Vec<_> = [("flush-1", "s1"), ("flush-2", "s2")]
        .into_iter()
        .map(|(key, id)| {
            let mut child = receiver(dir, key).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
            child.stdin.take().unwrap().write_all(line("spans", serde_json::json!({ "id": id })).as_bytes()).unwrap();
            child
        })
        .collect();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
    let wait = |what: &str, done: &dyn Fn() -> bool, children: &mut Vec<std::process::Child>| {
        while !done() {
            assert!(std::time::Instant::now() < deadline, "the group never reached {what} before {crash:?}");
            for child in children.iter_mut() {
                assert!(child.try_wait().unwrap().is_none(), "a receiver ended before {crash:?}");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    wait("the spool", &|| spooled(dir) == 2, &mut children);
    let data = held(&tables.join("otel_spans/commit.lock"));
    let record = matches!(crash, Crash::Opened).then(|| held(&tables.join("_runs/commit.lock")));
    land.unlock().unwrap();
    drop(land);
    match crash {
        Crash::Opened => wait("its run record", &|| recording(&tables), &mut children),
        Crash::Staged => wait("its staged parts", &|| staging(&tables.join("otel_spans")), &mut children),
        Crash::Committed => {
            wait("its staged parts", &|| staging(&tables.join("otel_spans")), &mut children);
            let record = held(&tables.join("_runs/commit.lock"));
            data.unlock().unwrap();
            let manifest = || files_under(&tables.join("otel_spans")).iter().any(|p| p.file_name().is_some_and(|n| n == "_manifest.json"));
            wait("its manifest", &|| manifest() && recording(&tables), &mut children);
            for mut child in children.drain(..) {
                child.kill().unwrap();
                child.wait().unwrap();
            }
            drop(record);
        }
    }
    for mut child in children {
        child.kill().unwrap();
        child.wait().unwrap();
    }
    drop(record);
    drop(data);
}

/// A leader killed with its group's run open, with its parts staged, or with its commit landed and no member answered
/// leaves the group to the next leader: a later batch lands apart from it, each member's resend replays the group's
/// run, and every row lands once.
// spec: run.land.receive-adopt@56163e99
#[test]
fn a_group_whose_leader_dies_lands_each_member_once() {
    // The resends arrive once the dead leader's 30 s cursor lease has lapsed.
    const LATER: &str = "2030-01-01T00:01:00Z";
    for crash in [Crash::Opened, Crash::Staged, Crash::Committed] {
        let dir = receiving(4096, 4);
        crash_a_group(dir.path(), crash);
        let later = send(receiver_at(dir.path(), "flush-3", LATER), &line("spans", serde_json::json!({ "id": "s3" })));
        assert!(later.status.success(), "{crash:?}: {}", stderr(&later));
        assert_eq!(answer(&later)["tables"][0]["outcome"], "committed", "{crash:?}");
        let mut replayed = Vec::new();
        for (key, id) in [("flush-1", "s1"), ("flush-2", "s2")] {
            let out = send(receiver_at(dir.path(), key, LATER), &line("spans", serde_json::json!({ "id": id })));
            assert!(out.status.success(), "{crash:?}: {}", stderr(&out));
            let ack = answer(&out);
            assert_eq!(ack["tables"][0]["outcome"], "replayed", "{crash:?}: {ack}");
            replayed.push(ack["tables"][0]["run_id"].as_str().unwrap().to_string());
        }
        assert_eq!(replayed[0], replayed[1], "{crash:?}: both members replay one group run");
        let landed = rows(dir.path(), "SELECT id, _run_id FROM otel_spans ORDER BY id");
        let run_of = |id: &str| landed.as_array().unwrap().iter().filter(|r| r[0] == id).map(|r| r[1].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!((run_of("s1"), run_of("s2")), (vec![replayed[0].clone()], vec![replayed[0].clone()]), "{crash:?}: each member lands once, in its group's run: {landed}");
        assert_eq!(run_of("s3").len(), 1, "{crash:?}: {landed}");
        assert_ne!(run_of("s3")[0], replayed[0], "{crash:?}: the later batch lands apart from the dead leader's group: {landed}");
        assert_eq!(spooled(dir.path()), 0, "{crash:?}: the group leaves no batch spooled");
    }
}
