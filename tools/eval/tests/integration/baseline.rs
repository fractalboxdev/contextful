use contextful_eval::baseline::*;
use contextful_eval::EvalError;
use serde_json::{json, Value};

use crate::{node, run};

/// The baseline file the spec shows, recorded in the deterministic tier.
const FILE: &str = r#"{
  "_run": { "k": 10, "tier": "deterministic", "model": null, "samples": 1 },
  "retrieval.hybrid.recall_at_k": 0.71,
  "retrieval.hybrid.recall_at_k.n": 184,
  "retrieval.hybrid.ndcg_at_k": { "value": 0.63, "band": 0.03 },
  "retrieval.hybrid.ndcg_at_k.n": 184,
  "judge.citation_faithfulness": 0.82,
  "judge.citation_faithfulness.n": 184,
  "latency_ms": { "value": 480, "band": 60 },
  "n_cases": 184
}"#;

/// A report measuring exactly what [`FILE`] records.
fn report() -> Value {
    json!({
        "run": run(10),
        "n_cases": 184,
        "retrieval": { "hybrid": { "recall_at_k": node(184, 0.71), "ndcg_at_k": node(184, 0.63) } },
        "judge": { "citation_faithfulness": node(184, 0.82) },
        "latency_ms": node(184, 480.0),
    })
}

fn file(entries: Value) -> Baselines {
    let mut map = entries.as_object().unwrap().clone();
    map.insert("_run".into(), run(10));
    Baselines::parse(&Value::Object(map).to_string()).unwrap()
}

fn outcome(b: &Baselines, report: &Value, path: &str) -> Outcome {
    gate(report, b).unwrap().comparisons.into_iter().find(|c| c.path == path).unwrap().outcome
}

fn set(report: &mut Value, pointer: &str, v: Value) {
    *report.pointer_mut(pointer).unwrap() = v;
}

fn unresolved(r: Result<Baselines, EvalError>) -> String {
    match r {
        Err(EvalError::BaselinePathUnresolved { path, .. }) => path,
        other => panic!("expected BaselinePathUnresolved, got {other:?}"),
    }
}

/// A baseline file carries a reserved run block `{k, tier, model, samples}` and one entry per gated metric: a bare
/// number at the run's dead band, or an object overriding the band.
// spec: assurance.baseline.file@0c9b68ab
#[test]
fn a_file_holds_a_run_block_and_bare_or_banded_entries() {
    let b = Baselines::parse(FILE).unwrap();
    assert_eq!(b.run, RunStamp { k: 10, tier: Tier::Deterministic, model: None, samples: 1 });
    assert_eq!(b.entries.len(), 8);
    assert_eq!(b.entries["retrieval.hybrid.recall_at_k"], Entry { value: 0.71, band: None });
    assert_eq!(b.entries["latency_ms"], Entry { value: 480.0, band: Some(60.0) });
    assert_eq!(Baselines::parse(&b.to_json()).unwrap(), b, "the file round-trips");

    let v = gate(&report(), &b).unwrap();
    assert_eq!(v.comparisons.len(), 8, "every entry is one gated comparison");
    assert!(v.passed());

    let no_run = FILE.replace(r#""_run": { "k": 10, "tier": "deterministic", "model": null, "samples": 1 },"#, "");
    assert_eq!(unresolved(Baselines::parse(&no_run)), "_run");
    let half_object = FILE.replace(r#"{ "value": 0.63, "band": 0.03 }"#, r#"{ "value": 0.63 }"#);
    assert_eq!(unresolved(Baselines::parse(&half_object)), "retrieval.hybrid.ndcg_at_k");
    let text = FILE.replace("0.82", r#""0.82""#);
    assert_eq!(unresolved(Baselines::parse(&text)), "judge.citation_faithfulness");
}

/// A rate-valued entry with no override gates at a 2 percent dead band.
// spec: assurance.baseline.default-dead-band@cee1030c
#[test]
fn a_bare_rate_entry_gates_at_two_percent() {
    assert_eq!(DEFAULT_DEAD_BAND, 0.02);
    let b = file(json!({ "retrieval.hybrid.recall_at_k": 0.71 }));
    assert_eq!(b.band("retrieval.hybrid.recall_at_k").unwrap(), 0.02);
    let mut r = report();
    set(&mut r, "/retrieval/hybrid/recall_at_k/mean", json!(0.69));
    assert_eq!(outcome(&b, &r, "retrieval.hybrid.recall_at_k"), Outcome::Stable);
    set(&mut r, "/retrieval/hybrid/recall_at_k/mean", json!(0.689));
    assert_eq!(outcome(&b, &r, "retrieval.hybrid.recall_at_k"), Outcome::Regressed);
    set(&mut r, "/retrieval/hybrid/recall_at_k/mean", json!(0.731));
    assert_eq!(outcome(&b, &r, "retrieval.hybrid.recall_at_k"), Outcome::Improved);
    // An override replaces the default.
    let wide = file(json!({ "retrieval.hybrid.recall_at_k": { "value": 0.71, "band": 0.05 } }));
    set(&mut r, "/retrieval/hybrid/recall_at_k/mean", json!(0.67));
    assert_eq!(outcome(&wide, &r, "retrieval.hybrid.recall_at_k"), Outcome::Stable);
}

/// The ranked-quality entry gates at a 3 percent dead band.
// spec: assurance.baseline.rank-quality-dead-band@f23a7d0f
#[test]
fn a_bare_ndcg_entry_gates_at_three_percent() {
    assert_eq!(RANK_DEAD_BAND, 0.03);
    let b = file(json!({ "retrieval.hybrid.ndcg_at_k": 0.63, "retrieval.hybrid.recall_at_k": 0.71 }));
    assert_eq!(b.band("retrieval.hybrid.ndcg_at_k").unwrap(), 0.03);
    let mut r = report();
    // The same 3-point drop: stable on nDCG, a regression on recall.
    set(&mut r, "/retrieval/hybrid/ndcg_at_k/mean", json!(0.60));
    set(&mut r, "/retrieval/hybrid/recall_at_k/mean", json!(0.68));
    assert_eq!(outcome(&b, &r, "retrieval.hybrid.ndcg_at_k"), Outcome::Stable);
    assert_eq!(outcome(&b, &r, "retrieval.hybrid.recall_at_k"), Outcome::Regressed);
    set(&mut r, "/retrieval/hybrid/ndcg_at_k/mean", json!(0.599));
    assert_eq!(outcome(&b, &r, "retrieval.hybrid.ndcg_at_k"), Outcome::Regressed);
}

/// A band carries its metric's own units, and a count entry is pinned at zero with no band.
// spec: assurance.baseline.band-units@653da3ad
#[test]
fn latency_bands_in_milliseconds_and_counts_pin_at_zero() {
    // Latency is in milliseconds, so the fractional default cannot apply to it.
    assert_eq!(unresolved(Baselines::parse(&FILE.replace(r#"{ "value": 480, "band": 60 }"#, "480"))), "latency_ms");
    let b = Baselines::parse(FILE).unwrap();
    let mut r = report();
    set(&mut r, "/latency_ms/mean", json!(540.0));
    assert_eq!(outcome(&b, &r, "latency_ms"), Outcome::Stable);
    set(&mut r, "/latency_ms/mean", json!(541.0));
    assert_eq!(outcome(&b, &r, "latency_ms"), Outcome::Regressed, "slower is worse");
    set(&mut r, "/latency_ms/mean", json!(400.0));
    assert_eq!(outcome(&b, &r, "latency_ms"), Outcome::Improved, "faster is better");

    // A count carries no band and loses a single case.
    assert_eq!(unresolved(Baselines::parse(&FILE.replace(r#""n_cases": 184"#, r#""n_cases": { "value": 184, "band": 2 }"#))), "n_cases");
    assert_eq!(b.band("n_cases").unwrap(), 0.0);
    assert_eq!(b.band("retrieval.hybrid.recall_at_k.n").unwrap(), 0.0);
    let mut r = report();
    set(&mut r, "/n_cases", json!(183));
    assert_eq!(outcome(&b, &r, "n_cases"), Outcome::Regressed);

    // A negative or absent band is malformed.
    for band in ["-0.01", "null"] {
        let raw = FILE.replace(r#""band": 0.03"#, &format!(r#""band": {band}"#));
        assert_eq!(unresolved(Baselines::parse(&raw)), "retrieval.hybrid.ndcg_at_k", "band {band}");
    }
}

/// An entry names a metric by the report's field path: `retrieval.<modality>.<metric>`,
/// `edge_retrieval.<modality>.<metric>`, `judge.<dimension>`, `slices.<tag>.<metric>`, `latency_ms` or `n_cases`.
// spec: assurance.baseline.metric-path@77977e9e
#[test]
fn an_entry_names_a_report_field_path() {
    for path in [
        "retrieval.lexical.recall_at_k",
        "retrieval.vector.r_precision",
        "retrieval.hybrid.in_window_rate",
        "edge_retrieval.hybrid.ndcg_at_k",
        "judge.accuracy",
        "judge.correct_refusal",
        "judge.hallucination_on_unknown",
        "judge.temporal_correctness",
        "slices.multi_hop.retrieval.hybrid.recall_at_k",
        "slices.multi_hop.judge.accuracy",
        "slices.multi_hop.n_cases",
        "latency_ms",
        "n_cases",
    ] {
        assert!(MetricPath::parse(path).is_ok(), "{path} is a metric path");
    }
    for path in [
        "retrieval.fts.recall_at_k",
        "retrieval.hybrid.precision_at_k",
        "retrieval.hybrid",
        "judge.faithfulness",
        "slices.multi_hop",
        "slices..judge.accuracy",
        "slices.a.slices.b.n_cases",
        "slices.a.latency_ms",
        "k",
    ] {
        match MetricPath::parse(path) {
            Err(EvalError::BaselinePathUnresolved { path: p, .. }) => assert_eq!(p, path),
            other => panic!("{path} parsed as {other:?}"),
        }
    }
    // The walk follows the report's fields.
    let p = MetricPath::parse("slices.multi_hop.retrieval.hybrid.recall_at_k").unwrap();
    assert_eq!(p.steps, ["slices", "multi_hop", "retrieval", "hybrid", "recall_at_k"]);
    let r = json!({ "slices": { "multi_hop": { "retrieval": { "hybrid": { "recall_at_k": node(40, 0.5) } } } } });
    assert_eq!(resolve(&r, "slices.multi_hop.retrieval.hybrid.recall_at_k").unwrap(), 0.5);
}

/// Every mean-valued path takes a `.n` suffix naming its sample count.
// spec: assurance.baseline.sample-count@d8c25be2
#[test]
fn a_mean_carries_its_sample_count_and_shrinking_it_regresses() {
    let b = Baselines::parse(FILE).unwrap();
    // Blanking the truth of the 40 worst cases lifts the NaN-filtered mean and leaves
    // `n_cases` alone; the `.n` entry is what moves.
    let mut r = report();
    set(&mut r, "/retrieval/hybrid/recall_at_k", node(144, 0.80));
    let v = gate(&r, &b).unwrap();
    let by = |p: &str| v.comparisons.iter().find(|c| c.path == p).unwrap().outcome;
    assert_eq!(by("retrieval.hybrid.recall_at_k"), Outcome::Improved);
    assert_eq!(by("n_cases"), Outcome::Stable);
    assert_eq!(by("retrieval.hybrid.recall_at_k.n"), Outcome::Regressed);
    assert!(!v.passed());

    for path in ["latency_ms.n", "judge.accuracy.n", "slices.t.retrieval.hybrid.recall_at_k.n"] {
        assert_eq!(MetricPath::parse(path).unwrap().kind, PathKind::Count, "{path}");
    }
    for path in ["n_cases.n", "slices.t.n_cases.n", "retrieval.hybrid.recall_at_k.n.n"] {
        assert!(MetricPath::parse(path).is_err(), "{path} names no sample count");
    }
}

#[test]
fn an_entry_naming_nothing_or_under_thirty_cases_refuses_the_whole_gate() {
    assert_eq!(GATED_MEAN_CASES, 30);
    let b = Baselines::parse(FILE).unwrap();
    let mut r = report();
    r["retrieval"]["hybrid"].as_object_mut().unwrap().remove("ndcg_at_k");
    match gate(&r, &b) {
        Err(EvalError::BaselinePathUnresolved { path, .. }) => assert_eq!(path, "retrieval.hybrid.ndcg_at_k"),
        other => panic!("a renamed field disabled its gate: {other:?}"),
    }
    let mut r = report();
    set(&mut r, "/judge/citation_faithfulness", node(29, 0.9));
    match gate(&r, &b) {
        Err(EvalError::BaselinePathUnresolved { path, .. }) => assert_eq!(path, "judge.citation_faithfulness"),
        other => panic!("a mean over 29 cases gated: {other:?}"),
    }
    set(&mut r, "/judge/citation_faithfulness", node(30, 0.9));
    assert!(gate(&r, &b).is_ok());
}

/// A run block differing from the run's own configuration raises `BaselineRunStampMismatch` before the suite runs.
// spec: assurance.baseline.run-stamp-drift@b45ac5d1
#[test]
fn a_run_configured_unlike_the_baseline_is_refused_from_its_configuration_alone() {
    let b = Baselines::parse(FILE).unwrap();
    let same = RunStamp { k: 10, tier: Tier::Deterministic, model: None, samples: 1 };
    assert!(b.check_run(&same).is_ok());
    // The configuration alone decides it: no report exists yet.
    for drift in [
        RunStamp { k: 20, ..same.clone() },
        RunStamp { tier: Tier::Judged, model: Some("pinned-open-weights".into()), ..same.clone() },
        RunStamp { samples: 3, ..same.clone() },
    ] {
        let e = b.check_run(&drift).unwrap_err();
        assert_eq!(e.code(), "BaselineRunStampMismatch", "{drift}");
        assert!(e.to_string().contains("k=10"), "{e}");
    }
    // A report whose run block differs yields no comparison at all.
    let mut r = report();
    r["run"] = run(20);
    assert_eq!(gate(&r, &b).unwrap_err().code(), "BaselineRunStampMismatch");
    r.as_object_mut().unwrap().remove("run");
    assert_eq!(gate(&r, &b).unwrap_err().code(), "BaselineRunStampMismatch");
}

/// A baseline update runs after every gate and floor passes, moves each improved entry to its measured value,
/// moves none in the worse direction, and adds no path.
// spec: assurance.baseline.raise-only@6cc800d6
#[test]
fn an_update_raises_improved_entries_only_and_only_on_green() {
    let mut b = Baselines::parse(FILE).unwrap();
    let mut r = report();
    set(&mut r, "/retrieval/hybrid/recall_at_k/mean", json!(0.80)); // improved
    set(&mut r, "/retrieval/hybrid/ndcg_at_k/mean", json!(0.61)); // worse, inside the band
    set(&mut r, "/latency_ms/mean", json!(400.0)); // improved: lower
    // A measured, ungated metric.
    r["retrieval"]["vector"] = json!({ "recall_at_k": node(184, 0.5) });
    let v = gate(&r, &b).unwrap();
    assert!(v.passed());
    let moved = b.raise(&v).unwrap();
    assert_eq!(moved, ["latency_ms", "retrieval.hybrid.recall_at_k"]);
    assert_eq!(b.entries["retrieval.hybrid.recall_at_k"].value, 0.80);
    assert_eq!(b.entries["latency_ms"], Entry { value: 400.0, band: Some(60.0) }, "the band stays");
    assert_eq!(b.entries["retrieval.hybrid.ndcg_at_k"].value, 0.63, "a worse figure inside the band moves nothing");
    assert_eq!(b.entries.len(), 8, "no path is added");

    // A red verdict — here a floor — runs no update.
    let mut red = r.clone();
    red["retrieval"]["hybrid"]["duplicate_row_rate"] = json!({ "n": 184, "mean": 0.01, "min": 0.0, "max": 0.5 });
    let before = b.clone();
    let v = gate(&red, &b).unwrap();
    assert!(v.baseline_passed() && !v.floors.passed);
    assert_eq!(b.raise(&v), None);
    assert_eq!(b, before);
}

/// An absolute floor gates independently of every committed value.
// spec: assurance.baseline.floors-are-absolute@0b023b57
#[test]
fn a_floor_reds_a_run_its_baseline_passes() {
    // A committed R-precision below the floor, and a run that improves on it.
    let b = file(json!({ "retrieval.hybrid.r_precision": 0.40 }));
    let r = json!({ "run": run(10), "retrieval": { "hybrid": { "r_precision": node(60, 0.45) } } });
    let v = gate(&r, &b).unwrap();
    assert_eq!(v.comparisons[0].outcome, Outcome::Improved);
    assert!(v.baseline_passed());
    assert!(!v.floors.passed, "the floor ignores the committed 0.40");
    assert!(!v.passed());

    // A file gating nothing still answers to the floors.
    let empty = file(json!({}));
    let v = gate(&r, &empty).unwrap();
    assert!(v.comparisons.is_empty() && !v.passed());
}
