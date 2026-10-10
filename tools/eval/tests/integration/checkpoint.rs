use std::collections::BTreeMap;

use contextful_eval::baseline::{RunStamp, Tier};
use contextful_eval::checkpoint::{CaseResult, Checkpoint, CheckpointError, Header};
use contextful_eval::metrics::{Returned, RowRef};

fn header(seed: u64) -> Header {
    Header { run: RunStamp { k: 5, tier: Tier::Deterministic, model: None, samples: 1 }, seed }
}

fn result(id: &str, key: &str) -> CaseResult {
    let legs = ["lexical", "vector", "hybrid"]
        .iter()
        .map(|l| (l.to_string(), vec![Returned { row: RowRef::new("t", key), in_window: true }]))
        .collect::<BTreeMap<_, _>>();
    CaseResult { id: id.into(), legs, edges: None, systems: None }
}

/// A run appends each case's result as JSONL, and a crash re-runs the in-flight case.
// spec: assurance.evaluate.checkpoint@2d3c57a1
#[test]
fn a_killed_run_resumes_from_its_jsonl_and_reruns_only_the_in_flight_case() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.jsonl");
    let (mut cp, done) = Checkpoint::open(&path, &header(7)).unwrap();
    assert!(done.is_empty());
    cp.append(&result("c1", "a")).unwrap();
    cp.append(&result("c2", "b")).unwrap();
    drop(cp);
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.lines().count(), 3, "a header and one line per finished case: {text}");
    assert!(text.ends_with('\n'));

    // The kill lands mid-write: the third case's line is cut short.
    let torn = serde_json::to_string(&result("c3", "c")).unwrap();
    std::fs::write(&path, format!("{text}{}", &torn[..torn.len() / 2])).unwrap();
    let (mut cp, done) = Checkpoint::open(&path, &header(7)).unwrap();
    assert_eq!(done.keys().collect::<Vec<_>>(), ["c1", "c2"], "the in-flight case alone runs again");
    assert_eq!(done["c2"], result("c2", "b"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "the torn line is truncated away");
    cp.append(&result("c3", "c")).unwrap();
    drop(cp);
    let (_, done) = Checkpoint::open(&path, &header(7)).unwrap();
    assert_eq!(done.len(), 3);
    assert_eq!(done["c3"], result("c3", "c"));

    // A whole line that does not parse is a crash only when it is the last one.
    let unterminated = format!("{text}{{\"id\":\"c3\"\n");
    std::fs::write(&path, &unterminated).unwrap();
    assert_eq!(Checkpoint::open(&path, &header(7)).unwrap().1.len(), 2);
    std::fs::write(&path, format!("{text}{{\"id\":\"c3\"\n{}\n", serde_json::to_string(&result("c4", "d")).unwrap())).unwrap();
    assert!(matches!(Checkpoint::open(&path, &header(7)), Err(CheckpointError::Corrupt { line: 4, .. })));

    // A checkpoint written under another seed resumes nothing.
    std::fs::write(&path, &text).unwrap();
    match Checkpoint::open(&path, &header(8)) {
        Err(CheckpointError::Mismatch { recorded, run, .. }) => assert!(recorded.ends_with("seed=7") && run.ends_with("seed=8")),
        other => panic!("expected a mismatch, got {other:?}"),
    }
}
