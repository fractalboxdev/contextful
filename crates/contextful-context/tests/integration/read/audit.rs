//! `disclosure.record.projection`: `audit_reads` computed from a chain's entries on each
//! call, and a lookup over a rolling 24 h window within 1 s.

use contextful_context::read::{audit_reads, ReadOptions};
use contextful_policy::audit::{attr, entries, AuditLog};
use contextful_policy::issue::SeedSigner;
use serde_json::{json, Value};
use std::time::{Duration, Instant as Clock, SystemTime, UNIX_EPOCH};

/// `secs` since the epoch as RFC 3339 UTC.
fn rfc3339(secs: u64) -> String {
    contextful_core::time::Instant::from_unix_nanos(i128::from(secs) * 1_000_000_000).unwrap().to_rfc3339()
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn read_entry(at: u64, who: &str, table: &str, rows: u64) -> Value {
    json!({
        attr::READ_AT: rfc3339(at),
        attr::ON_BEHALF_OF: who,
        attr::AGENT: "agent://research-loop",
        attr::TABLES: [table],
        attr::POLICY: "rev-1",
        attr::OUTCOME: attr::SERVED,
        attr::ROWS: rows,
    })
}

fn signer() -> SeedSigner {
    SeedSigner::from_seed(&format!("ed25519-private/{}", "07".repeat(32))).unwrap()
}

/// Every entry in the window projects one row per table it names; an entry carrying no
/// read instant projects none; the relation holds the named columns.
#[test]
fn audit_reads_projects_one_row_per_table_an_entry_names() {
    let dir = tempfile::tempdir().unwrap();
    let now = now_secs();
    let log = AuditLog::open(dir.path(), signer()).unwrap();
    let mut two = read_entry(now - 10, "user://ada@acme.example", "research/notes", 4);
    two[attr::TABLES] = json!(["research/notes", "research/links"]);
    log.append_all(vec![two, json!({ "contextful.erasure.subject": "sha256:00" })]).unwrap();
    drop(log);
    let chain = entries(dir.path()).unwrap();
    assert_eq!(chain.len(), 2);
    let out = audit_reads(&chain, "SELECT * FROM audit_reads ORDER BY table_name", ReadOptions::default()).unwrap().to_json();
    assert_eq!(out["columns"], json!(["read_at", "on_behalf_of", "agent", "table_name", "policy", "outcome", "row_count"]));
    assert_eq!(out["rows"].as_array().unwrap().len(), 2, "{out}");
    assert_eq!(out["rows"][0][3], "research/links");
    assert_eq!(out["rows"][1][3], "research/notes");
}

/// A lookup over a full 24 h window answers within 1 s, the chain's segments parsed on
/// the call. Under `contextful-ci measure` the window holds 86 400 reads, one per second;
/// elsewhere 8 640, one per 10 s.
// spec: disclosure.record.projection@ee7e8e76
#[test]
fn a_lookup_over_a_24_hour_window_answers_within_one_second() {
    let measuring = std::env::var_os(contextful_eval::record::MEASURE_DIR_VAR).is_some();
    let step: u64 = if measuring { 1 } else { 10 };
    let dir = tempfile::tempdir().unwrap();
    let now = now_secs();
    let window = 24 * 3600;
    let log = AuditLog::open(dir.path(), signer()).unwrap();
    let batch: Vec<Value> = (0..window / step)
        .map(|i| {
            let who = if i % 7 == 0 { "user://ada@acme.example" } else { "user://bo@acme.example" };
            read_entry(now - window + i * step + 1, who, if i % 2 == 0 { "research/notes" } else { "hr/salaries" }, i % 50)
        })
        .collect();
    let reads = batch.len() as u64;
    log.append_all(batch).unwrap();
    drop(log);

    let sql = "SELECT table_name, count(*) AS n FROM audit_reads WHERE on_behalf_of = 'user://ada@acme.example' \
               AND read_at >= now() - INTERVAL 24 HOUR GROUP BY table_name ORDER BY table_name";
    let started = Clock::now();
    let chain = entries(dir.path()).unwrap();
    let out = audit_reads(&chain, sql, ReadOptions::default()).unwrap().to_json();
    let elapsed = started.elapsed();
    eprintln!("audit_reads over {reads} reads in 24 h: {:.1} ms", elapsed.as_secs_f64() * 1e3);
    assert_eq!(out["rows"].as_array().unwrap().len(), 2, "{out}");
    contextful_eval::record::emit("audit-projection-latency", elapsed.as_secs_f64() * 1e3, reads, 0);
    assert!(elapsed < Duration::from_secs(1), "the lookup took {elapsed:?}");
}
