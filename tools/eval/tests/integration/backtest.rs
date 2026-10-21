//! `assurance.backtest`: bounded cases, splits, routing, slicing, fills, trials and
//! trajectory checks.

use std::collections::BTreeMap;

use contextful_core::connector::model_ref::{Cutoff, ModelRef};
use contextful_core::store::declare::TableDecl;
use contextful_core::time::Instant;
use contextful_eval::backtest::*;
use serde_json::{json, Map};

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

fn model(snapshot: &str, posttrain: Option<&str>) -> ModelRef {
    ModelRef { snapshot: snapshot.into(), pretrain_cutoff: None, posttrain_cutoff: posttrain.map(|c| Cutoff::parse(c).unwrap()) }
}

fn case(id: &str, t0: &str, t1: &str) -> BoundedCase {
    BoundedCase { id: id.into(), horizon: at(t0), label: Some(LabelInterval { t0: at(t0), t1: at(t1) }), trajectory: Vec::new() }
}

/// A bounded case carries a horizon, an instant each of its reads passes as {{store.available-time.available-as-of}}; its trajectory is the ordered tool calls, queries and retrieved documents its run records.
// spec: assurance.backtest.bounded-case@0a9a08ad
#[test]
fn every_read_of_a_bounded_case_passes_its_horizon() {
    let mut c = case("q1", "2015-01-01T00:00:00Z", "2015-03-31T00:00:00Z");
    let mut caller = Map::new();
    caller.insert("sql".into(), json!("SELECT 1"));
    caller.insert("available_as_of".into(), json!("2030-01-01T00:00:00Z"));
    let sent = c.read_arguments(caller);
    assert_eq!(sent["available_as_of"], json!("2015-01-01T00:00:00.000000000Z"), "the horizon replaces a later bound");
    assert_eq!(sent["sql"], json!("SELECT 1"));
    c.trajectory.push(Step { tool: "context.query".into(), query: Some("SELECT 1".into()), documents: vec!["research/notes#n1".into()] });
    c.trajectory.push(Step { tool: "corpus.retrieve".into(), query: Some("acme".into()), documents: Vec::new() });
    assert_eq!(c.trajectory.iter().map(|s| s.tool.as_str()).collect::<Vec<_>>(), ["context.query", "corpus.retrieve"]);
}

/// Every bounded case declares its label interval `[t0, t1]`, the span its outcome is observed over; a bounded case without one raises `EvalLabelIntervalMissing` before any of its reads.
// spec: assurance.backtest.label-interval@d254bc64
#[test]
fn a_bounded_case_without_a_label_interval_is_refused() {
    let mut c = case("q1", "2015-01-01T00:00:00Z", "2015-03-31T00:00:00Z");
    assert!(c.label_interval().is_ok());
    c.label = None;
    let e = c.label_interval().unwrap_err();
    assert_eq!(e.code(), "EvalLabelIntervalMissing");
    assert!(e.to_string().contains("q1"), "{e}");
    // A split over it refuses rather than guessing a span.
    assert_eq!(split(&[c.clone()], &[case("t", "2016-01-01T00:00:00Z", "2016-02-01T00:00:00Z")], 0).unwrap_err().code(), "EvalLabelIntervalMissing");
}

/// A split purges each training case whose label interval overlaps a test case's, and embargoes each training case starting within a declared span after a test fold ends.
// spec: assurance.backtest.split@b1400cfe
#[test]
fn a_split_purges_overlapping_labels_and_embargoes_the_cases_after_the_fold() {
    let test = [case("test", "2015-04-01T00:00:00Z", "2015-06-30T00:00:00Z")];
    let train = [
        case("before", "2015-01-01T00:00:00Z", "2015-03-31T00:00:00Z"),
        case("overlapping", "2015-03-01T00:00:00Z", "2015-04-15T00:00:00Z"),
        case("embargoed", "2015-07-05T00:00:00Z", "2015-09-30T00:00:00Z"),
        case("after", "2015-08-01T00:00:00Z", "2015-10-31T00:00:00Z"),
    ];
    let kept = split(&train, &test, 10 * 86_400).unwrap();
    assert_eq!(kept, ["before", "after"]);
    assert_eq!(split(&train, &test, 0).unwrap(), ["before", "embargoed", "after"]);
}

/// A reader, derive engine or embedder naming a checkpoint family resolves each bounded case to the newest family checkpoint whose post-training {{connector.infer.cutoffs}} precedes the case's horizon.
// spec: assurance.backtest.checkpoint-route@755df5bd
#[test]
fn a_family_routes_each_case_to_its_newest_checkpoint_before_the_horizon() {
    let now = at("2030-01-01T00:00:00Z");
    let family = [model("m-2014", Some("2014-06:documented")), model("m-2016", Some("2016-03:estimated")), model("m-2019", Some("2019-12:documented")), model("m-undated", None)];
    assert_eq!(route(&family, at("2017-01-01T00:00:00Z"), now).unwrap().snapshot, "m-2016");
    assert_eq!(route(&family, at("2016-04-01T00:00:00Z"), now).unwrap().snapshot, "m-2016", "the month ends at the next month's start");
    assert_eq!(route(&family, at("2016-03-15T00:00:00Z"), now).unwrap().snapshot, "m-2014");
    assert!(route(&family, at("2014-01-01T00:00:00Z"), now).is_none());
    // An undeclared cutoff compares as the call's instant, which no past horizon precedes.
    assert_eq!(route(&family, at("2029-01-01T00:00:00Z"), now).unwrap().snapshot, "m-2019");
}

/// A judge scores a case resolved before the judge's post-training cutoff against the case's stored expected answer alone.
// spec: assurance.backtest.answer-key-only@70a250f1
#[test]
fn a_case_resolved_inside_the_judges_knowledge_is_scored_by_answer_key_alone() {
    let now = at("2030-01-01T00:00:00Z");
    let judge = model("judge-7b", Some("2024-10:documented"));
    assert_eq!(judge_mode(&judge, at("2024-06-01T00:00:00Z"), now), JudgeMode::AnswerKeyOnly);
    assert_eq!(judge_mode(&judge, at("2024-11-01T00:00:00Z"), now), JudgeMode::Reasoned);
    // An undeclared cutoff is the call's instant: every past resolution is inside it.
    assert_eq!(judge_mode(&model("j", None), at("2029-12-31T00:00:00Z"), now), JudgeMode::AnswerKeyOnly);
}

/// A backtest report places each case by its resolution instant minus the reader's post-training cutoff and states leakage against a declared reference, a cutoff discontinuity or a clean control model, never a bare pre/post gap.
// spec: assurance.backtest.cutoff-slice@0c71e113
#[test]
fn a_report_places_each_case_relative_to_the_readers_cutoff() {
    let now = at("2030-01-01T00:00:00Z");
    let reader = model("reader", Some("2024-10:documented"));
    let cases = [
        Scored { resolved: at("2024-10-02T00:00:00Z"), score: 1.0 },
        Scored { resolved: at("2024-11-11T00:00:00Z"), score: 0.0 },
        Scored { resolved: at("2024-12-01T00:00:00Z"), score: 1.0 },
    ];
    let s = cutoff_slice(&cases, &reader, now, LeakageReference::CleanControl { model: "control-2023".into() });
    assert_eq!(s.offsets_days, [-30.0, 10.0, 30.0]);
    assert_eq!((s.before.n, s.before.mean), (1, 1.0));
    assert_eq!((s.after.n, s.after.mean), (2, 0.5));
    let json = serde_json::to_value(&s).unwrap();
    assert_eq!(json["reference"], json!({ "kind": "clean_control", "model": "control-2023" }));
}

/// The report repeats {{assurance.backtest.cutoff-slice}} with each `estimated` cutoff moved across its declared uncertainty interval, and states the minimum detectable effect.
// spec: assurance.backtest.cutoff-sensitivity@6fcbb55f
#[test]
fn an_estimated_cutoff_is_moved_across_its_interval_and_the_report_states_the_mde() {
    let now = at("2030-01-01T00:00:00Z");
    let cases: Vec<Scored> = ["2024-08-15", "2024-09-15", "2024-10-15", "2024-11-15", "2024-12-15"]
        .iter()
        .enumerate()
        .map(|(i, d)| Scored { resolved: at(&format!("{d}T00:00:00Z")), score: (i % 2) as f64 })
        .collect();
    let reference = LeakageReference::CutoffDiscontinuity;
    let estimated = cutoff_sensitivity(&cases, &model("r", Some("2024-10:estimated")), now, &[-2, 0, 2], &reference);
    let befores: Vec<usize> = estimated.iter().map(|(_, s, _)| s.before.n).collect();
    assert_eq!(befores, [1, 3, 5]);
    assert!(estimated[1].2.is_finite() && estimated[1].2 > 0.0, "{:?}", estimated[1].2);
    assert!(estimated[2].2.is_infinite(), "no case after the moved cutoff detects nothing");
    // A documented cutoff is not moved.
    let documented = cutoff_sensitivity(&cases, &model("r", Some("2024-10:documented")), now, &[-2, 0, 2], &reference);
    assert_eq!(documented.len(), 1);
    // The minimum detectable effect shrinks as cases grow.
    assert!(minimum_detectable_effect(0.5, 400, 400) < minimum_detectable_effect(0.5, 40, 40));
    assert!((minimum_detectable_effect(0.5, 100, 100) - 0.198).abs() < 0.001);
}

/// A case set run under a checkpoint family also runs the family's latest checkpoint as a paired arm, and the report states the latest-minus-routed difference.
// spec: assurance.backtest.latest-arm@580272f9
#[test]
fn the_report_states_the_latest_minus_routed_premium_over_paired_cases() {
    let routed: BTreeMap<String, f64> = [("a", 0.5), ("b", 0.25), ("c", 0.0)].map(|(k, v)| (k.to_string(), v)).into();
    let latest: BTreeMap<String, f64> = [("a", 1.0), ("b", 0.75)].map(|(k, v)| (k.to_string(), v)).into();
    assert_eq!(latest_premium(&routed, &latest), Some(0.5));
    assert_eq!(latest_premium(&routed, &BTreeMap::new()), None);
}

/// Beyond {{assurance.evaluate.judged-cutoffs}}, taken through {{assurance.evaluate.cutoff-flags}}, a backtest run block stamps the derive engine's and embedder's snapshot ids and cutoffs, the temperature, seed, prompt digest and tool-schema version.
// spec: assurance.backtest.run-stamp@6db8d1f9
#[test]
fn a_backtest_run_block_stamps_every_trial_dimension() {
    let stamp = BacktestStamp {
        reader: model("reader", Some("2024-10:documented")),
        judge: model("judge", None),
        derive: model("derive", Some("2023-12:estimated")),
        embedder: model("embed", None),
        temperature: 0.0,
        seed: 7,
        prompt_digest: "ab".repeat(32),
        tool_schema_version: "2030-01".into(),
    };
    let json = serde_json::to_value(&stamp).unwrap();
    assert_eq!(json["derive"], json!({ "snapshot": "derive", "posttrain_cutoff": "2023-12:estimated" }));
    assert_eq!(json["embedder"]["snapshot"], json!("embed"));
    for key in ["temperature", "seed", "prompt_digest", "tool_schema_version"] {
        assert!(json.get(key).is_some(), "{key}");
    }
    assert_eq!(serde_json::from_value::<BacktestStamp>(json.clone()).unwrap(), stamp);
    let mut changed = json;
    changed["seed"] = json!(8);
    assert_ne!(serde_json::from_value::<BacktestStamp>(changed).unwrap(), stamp, "another seed is another trial");
}

/// A financial return-series profile records each run against its case set as a trial row and computes the deflated Sharpe ratio from trial count, Sharpe variance, series length, skew and kurtosis.
// spec: assurance.backtest.trial-ledger@f321bc53
#[test]
fn the_ledger_deflates_the_best_sharpe_by_every_trial() {
    assert!((normal_quantile(0.975) - 1.959_964).abs() < 1e-5);
    assert!((normal_cdf(1.959_964) - 0.975).abs() < 1e-6);
    let series = |drift: f64| (0..250).map(|i| drift + 0.01 * ((i * 37 % 17) as f64 - 8.0) / 8.0).collect::<Vec<f64>>();
    let mut alone = TrialLedger::default();
    alone.land("momentum", series(0.002));
    let single = alone.deflated_sharpe("momentum").unwrap();
    assert!(single > 0.99, "{single}");

    let mut searched = alone.clone();
    for (i, drift) in [0.0, -0.001, 0.0015, 0.0005, -0.0005].iter().enumerate() {
        searched.land(&format!("variant-{i}"), series(*drift));
    }
    assert_eq!(searched.trials.len(), 6);
    let deflated = searched.deflated_sharpe("momentum").unwrap();
    assert!(deflated < single, "more trials raise the bar: {deflated} vs {single}");
    assert!((0.0..=1.0).contains(&deflated));
    assert!(searched.deflated_sharpe("absent").is_none());
}

/// A configuration runs its declared repeat count; its repeats count as one trial, and the report shows their spread.
// spec: assurance.backtest.seed-repeats@3ecc470b
#[test]
fn a_configurations_repeats_are_one_trial_with_a_spread() {
    let mut ledger = TrialLedger::default();
    ledger.land("cfg", vec![0.01, 0.02, -0.01, 0.03]);
    ledger.land("cfg", vec![0.02, 0.01, 0.00, 0.01]);
    ledger.land("cfg", vec![-0.01, 0.02, 0.01, 0.00]);
    assert_eq!(ledger.trials.len(), 1);
    let trial = &ledger.trials["cfg"];
    assert_eq!(trial.repeats.len(), 3);
    let (lo, hi) = trial.spread();
    assert!(lo < trial.sharpe() && trial.sharpe() < hi, "{lo} {} {hi}", trial.sharpe());
}

/// A case evaluating changing membership names a membership table declaring valid time and `available_time`; a case naming literal identifiers reports under a separate `fixed-universe` slice.
// spec: assurance.backtest.universe-table@cfe72adc
#[test]
fn a_literal_universe_reports_apart_and_a_table_declares_both_clocks() {
    let tables = TableDecl::parse_pipeline(
        "[[pipeline.tables]]\nname = \"sp500\"\n[pipeline.tables.valid_time]\nfrom = \"joined\"\nto = \"left\"\n[pipeline.tables.available_time]\ncolumn = \"announced\"\n\n[[pipeline.tables]]\nname = \"today\"\n",
    )
    .unwrap();
    assert_eq!(Universe::Table("sp500".into()).slice(&tables), Ok(None));
    assert!(Universe::Table("today".into()).slice(&tables).is_err());
    assert_eq!(Universe::Symbols(vec!["AAPL".into(), "MSFT".into()]).slice(&tables), Ok(Some(FIXED_UNIVERSE)));
}

/// A financial execution profile fills each decision at the first price available strictly after the decision's recorded instant.
// spec: assurance.backtest.fill-instant@ca63f365
#[test]
fn a_decision_fills_at_the_first_price_strictly_after_it() {
    let prices = [
        Price { available: at("2015-01-02T21:00:00Z"), price: 10.0 },
        Price { available: at("2015-01-05T21:00:00Z"), price: 11.0 },
        Price { available: at("2015-01-06T21:00:00Z"), price: 12.0 },
    ];
    assert_eq!(fill(&prices, at("2015-01-02T21:00:00Z")).unwrap().price, 11.0, "a price at the decision instant is not after it");
    assert_eq!(fill(&prices, at("2015-01-03T00:00:00Z")).unwrap().price, 11.0);
    assert!(fill(&prices, at("2015-01-07T00:00:00Z")).is_none());
}

/// A backtest report stamps its declared execution component's digest and parameters under {{topology.bound-application.domain-neutral}}; a report without one raises `EvalExecutionUndeclared`.
// spec: assurance.backtest.execution-component@38f1770c
#[test]
fn a_report_stamps_its_execution_component_or_refuses() {
    let component = ExecutionComponent { digest: "cd".repeat(32), parameters: [("cost_bps".to_string(), json!(5)), ("adv_cap".to_string(), json!(0.1))].into() };
    let block = execution_block("q1-report", Some(&component)).unwrap();
    assert_eq!(block, json!({ "digest": "cd".repeat(32), "parameters": { "adv_cap": 0.1, "cost_bps": 5 } }));
    let e = execution_block("q1-report", None).unwrap_err();
    assert_eq!(e.code(), "EvalExecutionUndeclared");
    assert!(e.to_string().contains("q1-report"), "{e}");
}

/// A bounded case fails when a call in its trajectory lacks the {{disclosure.record.bound-entry}} attributes, or a retrieved document scores at or above the case's declared leak threshold.
// spec: assurance.backtest.trajectory-check@3353130d
#[test]
fn a_case_fails_on_an_unbounded_call_or_a_leaking_document() {
    let bounded = json!({
        "contextful.tool": "context.query",
        "contextful.bounds": { "available_as_of": "2015-01-01T00:00:00.000000000Z", "inclusive": true },
        "contextful.read.snapshots": { "research/earnings": "00" },
        "contextful.read.query": "SELECT 1",
        "contextful.result.sha256": "ff",
    });
    let unbounded = json!({ "contextful.tool": "context.query" });
    let clean: BTreeMap<String, u8> = [("doc-1".to_string(), 1)].into();
    assert!(trajectory_check(std::slice::from_ref(&bounded), &clean, 3).is_empty());
    let failed = trajectory_check(&[bounded.clone(), unbounded], &clean, 3);
    assert_eq!(failed.len(), 1);
    assert!(failed[0].contains("call 1") && failed[0].contains("contextful.bounds"), "{failed:?}");
    let leaking: BTreeMap<String, u8> = [("doc-1".to_string(), 1), ("doc-2".to_string(), 3)].into();
    let failed = trajectory_check(&[bounded], &leaking, 3);
    assert_eq!(failed.len(), 1);
    assert!(failed[0].contains("doc-2"), "{failed:?}");
}

/// A recorded query naming an entity first available after the case's horizon lands a finding naming the call.
// spec: assurance.backtest.query-finding@6258666d
#[test]
fn a_query_naming_a_later_entity_lands_a_finding() {
    let mut c = case("q1", "2021-01-01T00:00:00Z", "2021-03-31T00:00:00Z");
    c.trajectory = vec![
        Step { tool: "corpus.retrieve".into(), query: Some("Facebook quarterly revenue".into()), documents: Vec::new() },
        Step { tool: "corpus.retrieve".into(), query: Some("Meta Platforms rebrand".into()), documents: Vec::new() },
        Step { tool: "context.files".into(), query: None, documents: Vec::new() },
    ];
    let first: BTreeMap<String, Instant> = [("Facebook".to_string(), at("2004-02-04T00:00:00Z")), ("Meta Platforms".to_string(), at("2021-10-28T00:00:00Z"))].into();
    let findings = query_findings(&c, &first);
    assert_eq!(findings.len(), 1);
    assert!(findings[0].contains("call 1") && findings[0].contains("Meta Platforms"), "{findings:?}");
}
