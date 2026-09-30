use std::path::Path;

use contextful_eval::case::*;
use contextful_eval::metrics::RowRef;

const LINE: &str = r#"{"id":"c1","corpus":"../corpora/native","question":"solar battery storage","tags":["temporal","regression"],"prefix":"news/","expected":{"answer":"Cells got cheaper.","artifacts":["news/wire#n1","news/wire#n2"],"edges":["e1"],"entities":["acme"],"must_cite":["news/wire#n1"],"must_abstain":false,"must_not_retrieve":["crm/deals#d9"],"time_anchor":"2030-06-01T00:00:00Z","since":"2030-03-01T00:00:00Z"}}"#;

/// A case carries an id, a corpus path relative to its case file, a question, tags, and an expected block of answer, artifacts, edges, entities, must-cite, must-abstain, must-not-retrieve, time anchor and recency bound.
// spec: assurance.evaluate.case-format@f2e030e3
#[test]
fn a_case_line_carries_its_corpus_question_tags_and_expected_block() {
    let cases = load(&format!("{LINE}\n\n")).unwrap();
    assert_eq!(cases.len(), 1, "a blank line holds no case");
    let c = &cases[0];
    assert_eq!(c.corpus_dir(Path::new("evals/cases/native.jsonl")), Path::new("evals/cases/../corpora/native"));
    assert_eq!(c.question, "solar battery storage");
    assert_eq!(c.tags, ["temporal", "regression"]);
    assert_eq!(c.prefix.as_deref(), Some("news/"));
    let e = &c.expected;
    assert_eq!(e.answer.as_deref(), Some("Cells got cheaper."));
    assert_eq!((e.edges.len(), e.entities.len(), e.must_cite.len()), (1, 1, 1));
    assert!(!e.must_abstain);
    assert_eq!(e.time_anchor.as_deref(), Some("2030-06-01T00:00:00Z"));
    assert!(c.recency_bound() && c.regression());
    assert_eq!(c.must_not(), [RowRef::new("crm/deals", "d9")].into_iter().collect());

    // A field outside the format is a typo, never silently dropped truth.
    let typo = LINE.replace("must_not_retrieve", "must_not_retreive");
    assert!(load(&typo).unwrap_err().reason.contains("must_not_retreive"));
    let twice = format!("{LINE}\n{LINE}");
    assert_eq!(load(&twice).unwrap_err().line, 2);
    assert!(load("\n  \n").is_err(), "a file with no case refuses");
    let silent = LINE.replace("solar battery storage", " ");
    assert!(load(&silent).unwrap_err().reason.contains("asks no question"));
}

/// A case names a row as `<table>#<key>`, the key joining the row's declared primary-key values with commas, and the runner keys each returned row the same way.
// spec: assurance.evaluate.row-reference@71384683
#[test]
fn a_row_is_named_by_its_table_and_key_after_the_last_hash() {
    assert_eq!(row_ref("news/wire#n1"), Some(RowRef::new("news/wire", "n1")));
    assert_eq!(row_ref("lab/orders#o7,2030"), Some(RowRef::new("lab/orders", "o7,2030")));
    assert_eq!(row_ref("a#b#c"), Some(RowRef::new("a#b", "c")));
    for bad in ["news/wire", "#n1", "news/wire#"] {
        assert_eq!(row_ref(bad), None, "{bad}");
    }
    assert_eq!(render(&RowRef::new("news/wire", "n1")), "news/wire#n1");
    let bad = LINE.replace("news/wire#n2", "news/wire");
    assert!(load(&bad).unwrap_err().reason.contains("`news/wire`"));
}
