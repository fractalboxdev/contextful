//! `run.journal` for the store-driven kind: the input set a read answers, its pins and the
//! labels a row records under.

use contextful_core::run::drive::{row_label, InputSet, StoreInput, Woken};
use contextful_core::run::RunError;
use serde_json::json;
use std::collections::BTreeMap;

fn snapshots() -> BTreeMap<String, String> {
    BTreeMap::from([("documents".to_string(), "5f1c".to_string())])
}

/// An input statement whose response the face truncates at its row ceiling raises `RunInputTruncated` before any
/// row runs.
// spec: run.journal.input-truncated@f1ffa5fc
#[test]
fn a_truncated_input_response_refuses_and_a_whole_one_keys_each_row() {
    let columns = vec!["doc_id".to_string(), "body".to_string()];
    let rows = vec![vec![json!("d1"), json!("alpha")], vec![json!("d2"), json!("beta")]];
    match InputSet::from_response("2030-01-01T00:00:00Z", snapshots(), &columns, rows.clone(), true) {
        Err(RunError::RunInputTruncated(m)) => assert!(m.contains("2 rows") && m.contains("2030-01-01T00:00:00Z"), "{m}"),
        other => panic!("expected RunInputTruncated, got {other:?}"),
    }
    let set = InputSet::from_response("2030-01-01T00:00:00Z", snapshots(), &columns, rows, false).unwrap();
    let keyed = set.keyed();
    assert_eq!(keyed.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), ["0", "1"]);
    assert_eq!(keyed[1].row["body"], json!("beta"));
    assert_eq!(set.bounds().rows, 2);
    assert_eq!(InputSet::decode(&set.encode()).unwrap(), set);
}

#[test]
fn the_plan_reference_moves_with_the_body_statement_and_as_of_and_labels_scope_to_the_row() {
    let base = StoreInput { body: "score".into(), statement: "SELECT 1".into(), as_of: None };
    let moved = [
        StoreInput { body: "rank".into(), ..base.clone() },
        StoreInput { statement: "SELECT 2".into(), ..base.clone() },
        StoreInput { as_of: Some("2030-01-01T00:00:00Z".into()), ..base.clone() },
    ];
    for m in &moved {
        assert_ne!(m.plan_ref(), base.plan_ref());
        assert_ne!(m.pins(), base.pins());
    }
    assert_eq!(base.plan_ref(), base.clone().plan_ref());
    assert_eq!(row_label("7", "model"), "row/7/model");
    assert_ne!(row_label("7", "model"), row_label("17", "model"));
    for w in [Woken::TimedOut, Woken::Resumed(Vec::new()), Woken::Resumed(b"yes".to_vec())] {
        assert_eq!(Woken::decode(&w.encode()).unwrap(), w);
    }
}
