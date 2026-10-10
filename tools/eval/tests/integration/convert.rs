use contextful_eval::case::load;
use contextful_eval::convert::{beir, write};
use contextful_eval::metrics::RowRef;

const QUERIES: &str = r#"{"_id": "q1", "text": "solar battery storage", "metadata": {}}
{"_id": "q2", "text": "wind turbine blades"}
{"_id": "q3", "text": "a query nobody judged"}
"#;

const QRELS: &str = "query-id\tcorpus-id\tscore\nq1\tdoc-7\t1\nq1\tdoc-9\t2\nq2\tdoc-3\t1\nq2\tdoc-4\t0\n";

/// Each benchmark and application ground truth converts once into the case format through its own converter, and the runner reads nothing else.
// spec: assurance.evaluate.converter@fef18126
#[test]
fn a_benchmark_converts_into_the_case_format_and_the_runner_reads_nothing_else() {
    let target = beir::Target { corpus: "../corpora/beir-scifact", table: "beir/docs" };
    let cases = beir::convert(QUERIES, QRELS, &target).unwrap();
    assert_eq!(cases.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["beir-q1", "beir-q2"], "an unjudged query converts to no case");
    assert_eq!(cases[0].question, "solar battery storage");
    assert_eq!(cases[0].corpus, "../corpora/beir-scifact");
    assert_eq!(cases[0].tags, ["beir"]);
    assert_eq!(cases[0].relevant(), [RowRef::new("beir/docs", "doc-7"), RowRef::new("beir/docs", "doc-9")].into_iter().collect());
    assert_eq!(cases[1].expected.artifacts, ["beir/docs#doc-3"], "a zero score is a judged miss");

    // The converted file loads through the one loader to the same cases.
    let file = write(&cases);
    assert_eq!(load(&file).unwrap(), cases);

    // The runner's loader refuses the benchmark's own records.
    assert!(load(QUERIES).unwrap_err().reason.contains("_id"));
    assert!(load(QRELS).is_err());

    // A malformed source names its file and line.
    let err = beir::convert(QUERIES, "query-id\tcorpus-id\tscore\nq1\tdoc-7\n", &target).unwrap_err();
    assert_eq!((err.source, err.line), ("qrels", 2));
    let err = beir::convert("{\"text\": \"no id\"}\n", QRELS, &target).unwrap_err();
    assert_eq!((err.source, err.line), ("queries", 1));
}
