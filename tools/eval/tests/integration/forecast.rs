//! `assurance.forecast`: forecast cases, imputation, coverage, tracks and the market and
//! no-evidence references.

use std::collections::BTreeMap;

use contextful_core::time::Instant;
use contextful_eval::forecast::*;

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

fn case(id: &str, outcome: f64, source: Source) -> ForecastCase {
    ForecastCase {
        id: id.into(),
        freeze: at("2024-01-01T00:00:00Z"),
        resolution: format!("{id} resolves against the first GDP release"),
        resolved_at: at("2024-07-01T00:00:00Z"),
        outcome,
        source,
        min_lead_secs: 7 * 86_400,
    }
}

fn answer(p: f64) -> Answer {
    Answer { probability: p, recorded: at("2024-01-02T00:00:00Z") }
}

fn answers(pairs: &[(&str, f64)]) -> BTreeMap<String, Answer> {
    pairs.iter().map(|(id, p)| (id.to_string(), answer(*p))).collect()
}

/// A forecast case carries a freeze instant, a resolution statement naming its vintage, a resolution instant and a probability answer; a backtest forecast case is a {{assurance.backtest.bounded-case}} whose horizon is its freeze instant.
// spec: assurance.forecast.forecast-case@e25c0f9f
#[test]
fn a_backtest_forecast_case_is_bounded_at_its_freeze() {
    let c = case("gdp", 1.0, Source::Dataset);
    assert!(c.resolution.contains("first GDP release"));
    let bounded = c.backtest().unwrap();
    assert_eq!(bounded.horizon, c.freeze);
    assert_eq!(bounded.id, "gdp");
}

/// A backtest forecast case whose resolution instant precedes its freeze instant raises `EvalCaseResolvedBeforeFreeze`.
// spec: assurance.forecast.resolved-before-freeze@0ab021d4
#[test]
fn a_case_resolved_before_its_freeze_is_refused() {
    let mut c = case("gdp", 1.0, Source::Dataset);
    c.resolved_at = at("2023-12-31T00:00:00Z");
    let e = c.backtest().unwrap_err();
    assert_eq!(e.code(), "EvalCaseResolvedBeforeFreeze");
    assert!(e.to_string().contains("gdp") && e.to_string().contains("2023-12-31"), "{e}");
    c.resolved_at = c.freeze;
    assert!(c.backtest().is_ok(), "resolving at the freeze is not before it");
}

/// An answer recorded less than the case's declared minimum lead before resolution scores on a separate track.
// spec: assurance.forecast.lead-time@ba95acdc
#[test]
fn a_late_answer_scores_on_its_own_track() {
    let c = case("gdp", 1.0, Source::Dataset);
    let early = answer(0.9);
    let late = Answer { probability: 1.0, recorded: at("2024-06-28T00:00:00Z") };
    assert_eq!(track_of(&c, &early, Track::Prospective), Track::Prospective);
    assert_eq!(track_of(&c, &late, Track::Prospective), Track::LateAnswer);
    // On the main track the late answer is missing and imputes.
    let given = [("gdp".to_string(), late)].into();
    let s = standing(&[c], &given, Track::Prospective, None);
    assert_eq!((s.answered, s.brier), (0, 0.25));
}

/// A market case carries the freeze-instant market price as an available row, and the report shows each forecaster's edge over it.
// spec: assurance.forecast.market-context@4b8851f1
#[test]
fn a_market_case_reports_each_forecasters_edge_over_the_freeze_price() {
    let cases = [case("fed-cut", 1.0, Source::Market { freeze_price: 0.6 }), case("cpi", 0.0, Source::Market { freeze_price: 0.3 })];
    let s = standing(&cases, &answers(&[("fed-cut", 0.8), ("cpi", 0.1)]), Track::Prospective, None);
    // Market Brier: (0.16 + 0.09) / 2; forecaster: (0.04 + 0.01) / 2.
    assert!((s.market_edge.unwrap() - 0.1).abs() < 1e-12, "{:?}", s.market_edge);
    let dataset = standing(&[case("gdp", 1.0, Source::Dataset)], &answers(&[("gdp", 0.8)]), Track::Prospective, None);
    assert_eq!(dataset.market_edge, None);
}

/// A missing answer scores as probability 0.5 on a dataset case and as the freeze-instant market price on a market case.
// spec: assurance.forecast.imputed@c271d81e
#[test]
fn a_missing_answer_imputes_the_neutral_or_market_probability() {
    assert_eq!(case("gdp", 1.0, Source::Dataset).imputed(), 0.5);
    assert_eq!(case("fed-cut", 1.0, Source::Market { freeze_price: 0.6 }).imputed(), 0.6);
    let s = standing(&[case("fed-cut", 1.0, Source::Market { freeze_price: 0.6 })], &BTreeMap::new(), Track::Prospective, None);
    assert!((s.brier - 0.16).abs() < 1e-12);
    assert_eq!(s.market_edge, Some(0.0));
}

/// A forecaster answering below 95 percent of a round's cases is reported unranked.
// spec: assurance.forecast.coverage-floor@8ee2c064
#[test]
fn a_forecaster_below_the_coverage_floor_is_unranked() {
    assert_eq!(COVERAGE_FLOOR_PERCENT, 95);
    let cases: Vec<ForecastCase> = (0..20).map(|i| case(&format!("c{i}"), 1.0, Source::Dataset)).collect();
    let all: BTreeMap<String, Answer> = (0..20).map(|i| (format!("c{i}"), answer(0.7))).collect();
    let mut nineteen = all.clone();
    nineteen.remove("c0");
    let mut eighteen = nineteen.clone();
    eighteen.remove("c1");
    assert!(standing(&cases, &all, Track::Prospective, None).ranked);
    assert!(standing(&cases, &nineteen, Track::Prospective, None).ranked, "19 of 20 is 95 percent");
    assert!(!standing(&cases, &eighteen, Track::Prospective, None).ranked);
}

/// Each round also scores a forecaster given the question text alone, and the report subtracts its Brier score from each forecaster's.
// spec: assurance.forecast.no-evidence-baseline@260f9d74
#[test]
fn each_forecaster_reports_against_the_question_only_baseline() {
    let cases = [case("a", 1.0, Source::Dataset), case("b", 0.0, Source::Dataset)];
    let question_only = answers(&[("a", 0.6), ("b", 0.4)]);
    let s = standing(&cases, &answers(&[("a", 0.9), ("b", 0.1)]), Track::ToolUsing, Some(&question_only));
    // Baseline Brier 0.16, forecaster 0.01.
    assert!((s.over_no_evidence.unwrap() - 0.15).abs() < 1e-12, "{:?}", s.over_no_evidence);
}

/// Backtest, prospective, tool-free and tool-using scores never pool into one figure.
// spec: assurance.forecast.tracks-separate@2aecaef0
#[test]
fn a_round_report_keeps_each_track_apart() {
    let cases = [case("a", 1.0, Source::Dataset)];
    let mut report = RoundReport::default();
    for track in [Track::Backtest, Track::Prospective, Track::ToolFree, Track::ToolUsing] {
        report.insert(track, "reader", standing(&cases, &answers(&[("a", 0.8)]), track, None));
    }
    let json = serde_json::to_value(&report).unwrap();
    let tracks: Vec<&String> = json["tracks"].as_object().unwrap().keys().collect();
    assert_eq!(tracks, ["backtest", "prospective", "tool-free", "tool-using"]);
    assert!(json.get("brier").is_none() && json.get("overall").is_none(), "{json}");
}
