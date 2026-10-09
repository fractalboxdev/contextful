//! The secret guard at the pull path.

use crate::support::{plan, Pages, Rig, Sink};
use contextful_core::pipeline::guard::MARKER;
use contextful_engine::guard::Guarded;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Mutex;

const KEY: &str = "AKIAIOSFODNN7EXAMPLE";

static REPORTED: Mutex<Vec<(String, BTreeMap<String, usize>)>> = Mutex::new(Vec::new());

fn capture(step: &str, counts: &BTreeMap<String, usize>) {
    REPORTED.lock().unwrap().push((step.to_string(), counts.clone()));
}

fn files_holding(dir: &std::path::Path, needle: &str) -> usize {
    let mut n = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            if e.path().is_dir() {
                stack.push(e.path());
            } else if String::from_utf8_lossy(&std::fs::read(e.path()).unwrap()).contains(needle) {
                n += 1;
            }
        }
    }
    n
}

/// The secret guard runs at the one pull path streaming and backfill share, ahead of the recorded pull and the
/// land path, so a replay reintroduces no credential.
// spec: run.guard-secrets.placement@b56c4dba
#[test]
fn the_journal_records_the_masked_pull_and_a_replay_lands_it_masked() {
    let rig = Rig::new();
    let pages = vec![vec![json!({"id": "d1", "note": format!("key {KEY}")})], vec![json!({"id": "d2", "note": "fine"})]];
    let mut source = Pages::new(pages.clone());
    source.die_after = Some(1);
    let mut sink = Sink::default();
    let p = plan("kind = \"opaque-token\"", "");
    rig.crash(&p, "run-1", &mut source, &mut sink);
    // The first pull is recorded, masked; the credential rests nowhere in the engine's state.
    assert_eq!(rig.engine.journal.recorded(&rig.row("run-1").execution_id).unwrap(), 1);
    assert!(files_holding(rig.dir.path(), MARKER) >= 1);
    assert_eq!(files_holding(rig.dir.path(), KEY), 0);
    // The replay lands the recorded batch, still masked.
    rig.clock.advance(60);
    rig.run(&p, "1.0.0", "run-2", &mut source, &mut sink).unwrap();
    let notes: Vec<&str> = sink.commits[0].batches.iter().flatten().map(|r| r["note"].as_str().unwrap()).collect();
    assert_eq!(notes, [format!("key {MARKER}").as_str(), "fine"]);
}

/// The guard is on by default and blocks no run; each pull logs the count of masked cells per column.
// spec: run.guard-secrets.mask-only@ab244676
#[test]
fn each_pull_reports_masked_cells_and_the_run_proceeds() {
    let rig = Rig::new();
    let pages = vec![vec![json!({"id": "d1", "note": KEY, "memo": "password=hunter2hunter2"}), json!({"id": "d2", "note": KEY})]];
    let mut source = Guarded { inner: Pages::new(pages), report: capture };
    let mut sink = Sink::default();
    let row = rig.run(&plan("kind = \"opaque-token\"", ""), "1.0.0", "run-guard", &mut source, &mut sink).unwrap();
    assert_eq!(row.status, contextful_core::run::record::RunStatus::Success);
    assert_eq!(row.rows, 2, "every row lands");
    let reported = REPORTED.lock().unwrap();
    let (_, counts) = reported.iter().find(|(step, c)| step == "pull-0" && c.contains_key("memo")).expect("the pull reported its counts");
    assert_eq!(counts.iter().map(|(k, v)| (k.as_str(), *v)).collect::<Vec<_>>(), [("memo", 1), ("note", 2)]);
}

#[test]
fn natural_identifiers_and_masked_provider_keys_survive_recursive_journal_replay() {
    let slugs = [
        "sk-public-company-orders-rise-above-average-before-next-quarter",
        "sk-public-company-orders-fall-below-average-before-next-quarter",
    ];
    let keys = [format!("sk-proj-{}", "aB9_-".repeat(24)), format!("sk-{}", "Ab9".repeat(16))];
    let rig = Rig::new();
    let rows = slugs.iter().zip(&keys).enumerate().map(|(n, (slug, key))| json!({
        "id": format!("public-{n}"), "slug": slug,
        "nested": {"items": [{"slug":slug,"description":key}]}, "description":key,
    })).collect();
    let mut source = Pages::new(vec![rows, Vec::new()]);
    source.die_after = Some(1);
    let mut sink = Sink::default();
    let p = plan("kind = \"opaque-token\"", "");
    rig.crash(&p, "before-restart", &mut source, &mut sink);
    let execution = rig.row("before-restart").execution_id;
    assert_eq!(rig.engine.journal.recorded(&execution).unwrap(), 1);
    for slug in slugs { assert!(files_holding(rig.dir.path(), slug) > 0, "the journal preserves each public identity"); }
    for key in &keys { assert_eq!(files_holding(rig.dir.path(), key), 0, "provider keys never reach the journal"); }
    // Change upstream bytes: the next attempt must use the already guarded journal entry.
    source.pages[0] = vec![json!({"id":"replacement","slug":"changed-after-recording"})];
    rig.clock.advance(60);
    let resumed = rig.run(&p, "1.0.0", "after-restart", &mut source, &mut sink).unwrap();
    assert_eq!(resumed.status, contextful_core::run::record::RunStatus::Success);
    assert_eq!(source.calls().iter().filter(|(position, _)| position.is_none()).count(), 1);
    let rows = &sink.commits[0].batches[0];
    assert_eq!(rows.len(), 2);
    for (row, slug) in rows.iter().zip(slugs) {
        assert_eq!(row["slug"], slug);
        assert_eq!(row["nested"]["items"][0]["slug"], slug);
        assert_eq!(row["description"], MARKER);
        assert_eq!(row["nested"]["items"][0]["description"], MARKER);
    }
}
