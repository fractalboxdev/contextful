//! `assurance.backtest.leak-score`: the judge's per-document score and the Brier per leak tier.

use std::collections::BTreeMap;

use contextful_core::store::declare::TableDecl;
use contextful_core::time::Instant;
use contextful_eval::backtest_run::*;
use contextful_eval::leak::{brier_by_tier, LeakJudge, StubLeakJudge};
use serde_json::{json, Map, Value};

/// A judge scoring every document by a fixed table, recording the reference it saw.
struct Scripted {
    scores: BTreeMap<&'static str, u8>,
    seen: std::cell::RefCell<Vec<(String, String, Instant)>>,
}

impl LeakJudge for Scripted {
    fn score(&self, document: &str, reference: &str, horizon: Instant) -> Result<u8, String> {
        self.seen.borrow_mut().push((document.to_string(), reference.to_string(), horizon));
        Ok(self.scores.iter().find(|(k, _)| document.contains(*k)).map_or(0, |(_, v)| *v))
    }
}

fn case(id: &str, outcome: f64, probability: f64) -> CaseSpec {
    serde_json::from_value(json!({
        "id": id, "corpus": "corpus", "horizon": "2014-05-01T00:00:00Z",
        "label": { "t0": "2014-05-01T00:00:00Z", "t1": "2014-06-01T00:00:00Z" },
        "reads": [{ "tool": "corpus.retrieve", "arguments": { "prefix": "research/pages", "query": "rates" } }],
        "forecast": {
            "resolution": "rates rise by June", "resolved_at": "2014-06-01T00:00:00Z", "outcome": outcome,
            "answer": { "probability": probability, "recorded": "2014-04-30T00:00:00Z" },
            "observed_at": "2014-06-01T00:00:00Z", "resolution_source": "metric", "comparator": "rate >= 0.5",
        },
    }))
    .unwrap()
}

fn retrieved(id: &str, rows: &[&str]) -> RecordedCall {
    let rows: Vec<Value> = rows.iter().map(|text| json!(["research/pages", { "body": text }, text])).collect();
    RecordedCall {
        case: id.into(),
        index: 0,
        tool: "corpus.retrieve".into(),
        arguments: Map::new(),
        answer: json!({ "columns": ["_table", "_row", "_snippet"], "rows": rows }),
        seq: None,
    }
}

/// The case's judge model scores each retrieved document 0 to 4 against a reference answer from rows available before the horizon, landing one row per document and case horizon; the report gives Brier per leak tier.
// spec: assurance.backtest.leak-score@177cc59f
#[test]
fn each_retrieved_document_lands_one_score_and_the_report_gives_brier_per_tier() {
    let cases = [case("clean", 1.0, 0.8), case("leaky", 1.0, 0.95)];
    let calls = [
        retrieved("clean", &["rates held steady in march", "rates outlook steady"]),
        retrieved("leaky", &["rates held steady in march", "the june hike to 0.5 percent", "the june hike to 0.5 percent"]),
    ];
    let decls: BTreeMap<String, Vec<TableDecl>> = [("corpus".to_string(), Vec::new())].into();
    let judge = Scripted { scores: [("june hike", 4)].into(), seen: Default::default() };
    let report = assess(&Recording { cases: &cases, decls: &decls, calls: &calls, entries: &BTreeMap::new(), judge: &judge }).unwrap();

    // One row per distinct document and case horizon, each on the 0 to 4 scale.
    let rows = report["leak"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 4, "{rows:?}");
    assert!(rows.iter().all(|r| r["horizon"] == json!("2014-05-01T00:00:00.000000000Z") && r["score"].as_u64().unwrap() <= 4), "{rows:?}");
    assert_eq!(rows.iter().filter(|r| r["case"] == json!("leaky") && r["score"] == json!(4)).count(), 1, "{rows:?}");

    // The reference a document is judged against holds the case's other rows, never the
    // document itself.
    for (document, reference, horizon) in judge.seen.borrow().iter() {
        assert_eq!(*horizon, Instant::parse("2014-05-01T00:00:00Z").unwrap());
        assert!(!reference.contains(document.as_str()), "{document} in {reference}");
    }

    // Brier per tier: the clean case sits in tier 0, the leaky one in tier 4.
    let tiers = &report["leak"]["brier_by_tier"];
    assert!((tiers["0"]["brier"].as_f64().unwrap() - 0.04).abs() < 1e-9, "{tiers}");
    assert!((tiers["4"]["brier"].as_f64().unwrap() - 0.0025).abs() < 1e-9, "{tiers}");
    assert_eq!(tiers["4"]["cases"], json!(1));
}

#[test]
fn the_stub_judge_scores_the_share_of_terms_the_reference_lacks() {
    let horizon = Instant::parse("2014-05-01T00:00:00Z").unwrap();
    assert_eq!(StubLeakJudge.score("rates held steady", "rates held steady in march", horizon).unwrap(), 0);
    assert_eq!(StubLeakJudge.score("june hike completed", "rates held steady", horizon).unwrap(), 4);
    assert_eq!(StubLeakJudge.score("rates hike", "rates held", horizon).unwrap(), 2);
    let tiers = brier_by_tier(&[(0, 0.8, 1.0), (0, 0.6, 0.0), (3, 0.9, 1.0)]);
    assert!((tiers[&0].1 - (0.04 + 0.36) / 2.0).abs() < 1e-12);
    assert_eq!(tiers[&3].0, 1);
}
