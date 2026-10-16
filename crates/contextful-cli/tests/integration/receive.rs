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
// spec: run.land.receive-batch-key@439b2557
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

/// Batches of one pipeline land one at a time: a batch whose body arrived waits while another holds the land lock,
/// and commits once it is released.
// spec: run.land.receive-queue@e0e977ce
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
