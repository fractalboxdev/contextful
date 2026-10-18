//! `run.land.dead-letter` through the runner: a pull's dead letters stage beside its table and commit with the run.

use crate::support::{plan, Rig, Sink};
use contextful_core::pipeline::guard::MARKER;
use contextful_core::run::ports::{Cancellation, DeadLetter, PullRequest, Source};
use contextful_core::run::record::RunStatus;
use contextful_core::run::Failure;
use serde_json::{json, Value};

const KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// A source answering one pull of `rows` beside one input it could not read, whose reason quotes a credential.
struct OneUnreadable {
    rows: Vec<Value>,
}

impl Source for OneUnreadable {
    fn pull(&mut self, request: &PullRequest, _: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        let letter = json!({"key": "d/2.jsonl", "reason": format!("PipelineUnreadableInput: `d/2.jsonl` at line 2: found {KEY}"), "sha256": "ab".repeat(32)});
        let cursor = request.position.clone().unwrap_or(json!("c1"));
        Ok(serde_json::to_vec(&json!({"rows": self.rows, "cursor": cursor, "more": false, "snapshot_complete": false, "skipped": 1, "dead_letters": [letter]})).unwrap())
    }
}

/// A pull's dead letters land in the table `<table>_dead_letter`, one row per input carrying its key, reason and
/// SHA-256, committed in the same run as the table's rows; its key, reason and SHA-256 pass the secret guard.
// spec: run.land.dead-letter@57a7105e
// spec: run.guard-secrets.dead-letter-reason@8970d04f
#[test]
fn a_pulls_dead_letters_stage_beside_its_table_and_commit_with_the_run() {
    let rig = Rig::new();
    let mut sink = Sink::default();
    let p = plan("kind = \"opaque-token\"", "");
    let row = rig.run(&p, "1.0.0", "run-1", &mut OneUnreadable { rows: vec![json!({"id": "d1"})] }, &mut sink).unwrap();
    assert_eq!((row.status, row.skipped, row.rows), (RunStatus::Success, 1, 1), "the dead letter counts as skipped beside the landed row");
    assert_eq!(sink.commits.len(), 1, "the dead letters commit with the run's one commit");
    assert_eq!(sink.dead_letters.len(), 1);
    let (table, run, letters) = &sink.dead_letters[0];
    assert_eq!((table.as_str(), run.as_str()), ("filings", "run-1"));
    let expected = DeadLetter { key: "d/2.jsonl".into(), reason: format!("PipelineUnreadableInput: `d/2.jsonl` at line 2: found {MARKER}"), sha256: "ab".repeat(32) };
    assert_eq!(letters, &[expected], "the secret guard masks a dead letter's reason as it masks a row");
}

/// A run whose pull lands no row and holds the position still commits its dead letters.
#[test]
fn a_dead_letter_commits_without_rows() {
    let rig = Rig::new();
    let p = plan("kind = \"opaque-token\"", "");
    let mut sink = Sink::default();
    rig.run(&p, "1.0.0", "run-1", &mut OneUnreadable { rows: Vec::new() }, &mut sink).unwrap();
    assert_eq!(sink.commits.len(), 1, "the dead letter alone commits the run");
    assert!(sink.commits[0].batches.is_empty());
}

/// A replacing table whose read dead-letters any input fails the pull under {{run.land.unreadable-input}}, naming each
/// input, and commits neither its rows nor its dead letters.
// spec: run.land.dead-letter-replace@f4a9125b
#[test]
fn a_replacing_table_whose_read_dead_letters_an_input_fails_the_pull_naming_it() {
    let rig = Rig::new();
    let p = plan("kind = \"opaque-token\"", "");
    for (fire, run) in ["run-1", "run-2"].into_iter().enumerate() {
        let mut replacing = Sink { replace: true, ..Sink::default() };
        rig.clock.advance(60);
        let row = rig.run(&p, "1.0.0", run, &mut OneUnreadable { rows: vec![json!({"id": "d1"})] }, &mut replacing).unwrap();
        let e = row.error_message.clone().unwrap_or_default();
        assert_eq!((row.status, row.rows), (RunStatus::Failed, 0), "fire {fire}: {e}");
        assert!(e.contains("PipelineUnreadableInput") && e.contains("d/2.jsonl"), "fire {fire}: {e}");
        assert!(replacing.discarded.iter().all(|(t, r)| (t.as_str(), r.as_str()) == ("filings", run)) && !replacing.discarded.is_empty(), "the staged rows go");
        assert!(replacing.commits.is_empty() && replacing.dead_letters.is_empty(), "fire {fire} commits nothing, so no dead letter repeats");
    }
}
