use contextful_eval::ledger::*;
use contextful_eval::baseline::Direction;

/// A tree holding one clause, one integration test, one case set and no probe.
struct Tree;

impl World for Tree {
    fn clause(&self, id: &str) -> bool {
        id == "run.journal.entry-key"
    }
    fn test(&self, path: &str) -> Result<(), String> {
        (path == "contextful_engine::journal::racers_run_one_effect").then_some(()).ok_or_else(|| "no such test function".into())
    }
    fn cases(&self, path: &str) -> bool {
        path == "evals/cases/deep-recall.jsonl"
    }
    fn probe(&self, _: &str) -> bool {
        false
    }
}

const ENTRY: &str = r#"
[entry.journal-effect-once]
clause = "run.journal.entry-key"
metric = "effects_per_key.max"
kind   = "test"
tier   = "gate"
method = { test = "contextful_engine::journal::racers_run_one_effect" }
target = { op = "==", value = 1 }
seed   = 0x5eed_0001

[entry.deep-recall]
clause = "run.journal.entry-key"
metric = "retrieval.lexical.hit_rate_at_k"
kind   = "eval"
tier   = "trend"
direction = "lower_is_better"
method = { issue = 44 }
"#;

fn ledger(text: &str) -> Ledger {
    toml::from_str(text).unwrap()
}

fn reasons(l: &Ledger) -> Vec<String> {
    l.unresolved(&Tree).into_iter().map(|e| e.to_string()).collect()
}

/// `evals/ledger.toml` holds one entry per tracked target: an id, an owning clause id, a run-report metric path, a tier, a method naming one integration test, case set or probe, and a threshold.
// spec: assurance.measure.ledger@7d30a989
#[test]
fn an_entry_carries_its_clause_metric_tier_method_and_threshold() {
    let l = ledger(ENTRY);
    assert_eq!(l.entry.len(), 2);
    let e = &l.entry["journal-effect-once"];
    assert_eq!((e.clause.as_str(), e.metric.as_str(), e.kind, e.tier), ("run.journal.entry-key", "effects_per_key.max", Kind::Test, Tier::Gate));
    assert_eq!(e.method(), Some(Method::Test("contextful_engine::journal::racers_run_one_effect")));
    assert_eq!(e.target, Some(Target { op: Op::Eq, value: 1.0 }));
    assert_eq!(e.seed, Some(0x5eed_0001));
    assert!(reasons(&l).is_empty(), "{:?}", reasons(&l));
    assert_eq!(test_target("contextful_engine::journal::racers_run_one_effect"), Some(("contextful-engine".into(), "journal::racers_run_one_effect".into())));
    assert_eq!(test_target("racers_run_one_effect"), None);

    // An unknown key is a malformed ledger, not a silently dropped field.
    assert!(toml::from_str::<Ledger>(&ENTRY.replace("seed   =", "sede   =")).is_err());
}

/// A gate-tier entry measures a count, a ratio within one run or a size under a locked resolve; a wall-clock or resident-memory figure is trend-tier.
// spec: assurance.measure.count-first@e7a56676
#[test]
fn a_timing_or_resident_set_figure_decides_no_gate() {
    for (from, to) in [
        ("kind   = \"test\"", "kind   = \"bench\""),
        ("metric = \"effects_per_key.max\"", "metric = \"read.latency_ms.p95\""),
        ("metric = \"effects_per_key.max\"", "metric = \"engine.idle_rss\""),
    ] {
        let r = reasons(&ledger(&ENTRY.replacen(from, to, 1)));
        assert_eq!(r.len(), 1, "{to}: {r:?}");
        assert!(r[0].contains("a timing is trend-tier"), "{to}: {r:?}");
        // The same figure at the trend tier records and decides nothing.
        let trend = ENTRY.replacen(from, to, 1).replacen("tier   = \"gate\"", "tier   = \"trend\"\ndirection = \"lower_is_better\"", 1);
        assert!(reasons(&ledger(&trend)).is_empty(), "{to}");
    }
    // Counts, ratios and sizes gate.
    for metric in ["journal.effects_per_key.max", "retrieval.hybrid.r_precision", "topology.store_write.unique_packages"] {
        let r = reasons(&ledger(&ENTRY.replacen("effects_per_key.max", metric, 1)));
        assert!(r.is_empty(), "{metric}: {r:?}");
    }
    // The committed ledger holds every gate entry to a count.
    let committed: Ledger = toml::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../evals/ledger.toml")).unwrap()).unwrap();
    assert!(!committed.entry.is_empty());
    assert!(committed.entry.values().any(|e| e.tier == Tier::Gate));
    assert!(committed.entry.values().filter(|e| e.tier == Tier::Gate).all(|e| e.kind != Kind::Bench));
}

/// A threshold is an absolute figure of this system's own measure, and it tightens only through {{assurance.baseline.raise-only}}.
// spec: assurance.measure.absolute-threshold@d8925df3
#[test]
fn a_threshold_is_a_finite_absolute_figure() {
    for value in ["inf", "-inf", "nan"] {
        let r = reasons(&ledger(&ENTRY.replacen("value = 1 }", &format!("value = {value} }}"), 1)));
        assert_eq!(r.len(), 1, "{value}: {r:?}");
        assert!(r[0].contains("no absolute figure"), "{value}: {r:?}");
    }
    // A target compares the measured figure alone: no baseline, history or other system enters.
    let t = Target { op: Op::Le, value: 12.0 };
    assert!(t.holds(12.0) && !t.holds(12.5));
    for relative in ["baseline = \"main\"", "relative = 0.05", "against = \"upstream\""] {
        let text = ENTRY.replacen("target = { op = \"==\", value = 1 }", &format!("target = {{ op = \"==\", value = 1, {relative} }}"), 1);
        assert!(toml::from_str::<Ledger>(&text).is_err(), "{relative}");
    }
}

#[test]
fn a_threshold_compares_by_its_operator() {
    let t = |op, value| Target { op, value };
    assert!(t(Op::Eq, 1.0).holds(1.0) && !t(Op::Eq, 1.0).holds(2.0));
    assert!(t(Op::Ge, 10.0).holds(10.0) && !t(Op::Ge, 10.0).holds(9.0));
    assert!(t(Op::Le, 0.0).holds(0.0) && !t(Op::Le, 0.0).holds(0.5));
    assert!(t(Op::Lt, 64.0).holds(63.0) && !t(Op::Lt, 64.0).holds(64.0));
    assert!(t(Op::Gt, 0.0).holds(0.1) && !t(Op::Gt, 0.0).holds(0.0));
    assert_eq!(t(Op::Ge, 0.95).to_string(), ">= 0.95");
}

#[test]
fn an_entry_naming_nothing_is_unresolved() {
    let cases = [
        ("clause = \"run.journal.entry-key\"", "clause = \"run.journal.entry-keys\"", "is no clause"),
        ("racers_run_one_effect\" }", "racers_run_two_effects\" }", "no such test function"),
        ("metric = \"effects_per_key.max\"", "metric = \"Effects per key\"", "no dotted path"),
        ("metric = \"retrieval.lexical.hit_rate_at_k\"", "metric = \"retrieval.lexical.nowhere\"", "BaselinePathUnresolved"),
        ("method = { issue = 44 }", "method = { issue = 44, probe = \"heap-open\" }", "exactly one"),
        ("method = { issue = 44 }", "method = { probe = \"heap-open\" }", "no binary of tools/probe"),
        ("method = { issue = 44 }", "method = { cases = \"evals/cases/nowhere.jsonl\" }", "does not exist"),
        ("target = { op = \"==\", value = 1 }\n", "", "carries a target"),
        ("[entry.deep-recall]", "[entry.Deep_Recall]", "slug"),
    ];
    for (from, to, needle) in cases {
        assert!(ENTRY.contains(from), "{from}");
        let r = reasons(&ledger(&ENTRY.replacen(from, to, 1)));
        assert_eq!(r.len(), 1, "{to}: {r:?}");
        assert!(r[0].starts_with("MeasureEntryUnresolved: `"), "{r:?}");
        assert!(r[0].contains(needle), "{to}: {r:?}");
    }
}

#[test]
fn the_status_view_lists_every_entry_and_an_issue_entry_as_open() {
    let l = ledger(ENTRY);
    assert_eq!(l.entry["deep-recall"].status(), Status::Open(44));
    assert_eq!(l.entry["journal-effect-once"].status(), Status::Gated);
    assert_eq!(l.runnable(Tier::Gate).map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["journal-effect-once"]);
    assert_eq!(l.runnable(Tier::Trend).count(), 0, "an open entry runs nothing");
    let md = l.render();
    assert!(md.contains("2 entries: 1 gated, 0 recorded, 0 scheduled, 1 open."), "{md}");
    assert!(md.contains("| `deep-recall` | `run.journal.entry-key` | `retrieval.lexical.hit_rate_at_k` | trend | issue 44 | — | open (issue 44) |"), "{md}");
    assert!(md.contains("| `journal-effect-once` |"), "{md}");
    assert_eq!(md, l.render(), "the view is a pure function of the ledger");
}

// spec: assurance.measure.trend-direction@4e4e0f8c
#[test]
fn every_trend_entry_declares_its_comparison_direction() {
    let l = ledger(ENTRY);
    assert_eq!(l.entry["deep-recall"].direction, Some(Direction::LowerIsBetter));
    let higher = ledger(&ENTRY.replace("lower_is_better", "higher_is_better"));
    assert_eq!(higher.entry["deep-recall"].direction, Some(Direction::HigherIsBetter));
    let without = ENTRY.replace("direction = \"lower_is_better\"\n", "");
    let findings = reasons(&ledger(&without));
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].contains("trend-tier entry declares a direction"), "{findings:?}");
}

/// An entry whose metric names a judged figure is scheduled-tier; on another tier it resolves to nothing under
/// {{assurance.measure.unresolved-entry}}.
// spec: assurance.measure.judged-tier@1f6b17d2
#[test]
fn a_judged_figure_runs_on_the_scheduled_tier_alone() {
    let entry = |tier: &str, metric: &str| {
        format!(
            "[entry.judged]\nclause = \"run.journal.entry-key\"\nmetric = \"{metric}\"\nkind = \"eval\"\ntier = \"{tier}\"\ndirection = \"higher_is_better\"\nmethod = {{ cases = \"evals/cases/deep-recall.jsonl\" }}\ntarget = {{ op = \">=\", value = 0.8 }}\n"
        )
    };
    assert!(reasons(&ledger(&entry("scheduled", "judge.accuracy"))).is_empty());
    for tier in ["gate", "trend"] {
        for metric in ["judge.accuracy", "slices.recall.judge.citation_faithfulness"] {
            let r = reasons(&ledger(&entry(tier, metric)));
            assert_eq!(r.len(), 1, "{tier} {metric}: {r:?}");
            assert!(r[0].contains("MeasureEntryUnresolved") && r[0].contains("scheduled"), "{r:?}");
        }
    }
    // A deterministic figure stays on any tier.
    assert!(reasons(&ledger(&entry("gate", "retrieval.hybrid.recall_at_k"))).is_empty());
}
