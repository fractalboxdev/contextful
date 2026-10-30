//! The orchestrator role's conformance suite against the in-tree filler (`run.orchestrate`).

use contextful_acceptance::bin;
use contextful_acceptance::orchestrate::suite::{self, Orchestrator, Target};
use contextful_acceptance::orchestrate::{Filler, Unit};

fn in_tree(target: &Target) -> Box<dyn Orchestrator> {
    Box::new(Filler::over(target))
}

/// The in-tree filler passes every scenario of the suite, each driving the stock binary as the
/// engine runner's reference record judges it: step replay, resume, a lease per step, one retry
/// layer, a stop, and a `call` delegate inside a job's step.
// spec: run.orchestrate.role@c9efbfde
// spec: run.orchestrate.semantics@8ca4fce7
// spec: run.orchestrate.delegate-inside@12608be9
#[test]
fn the_in_tree_filler_passes_the_orchestrator_suite() {
    let results = suite::run(&bin("contextful"), &in_tree);
    assert_eq!(results.len(), suite::SCENARIOS.len(), "the suite ran every scenario");
    assert!(!results.is_empty());
    let failed: Vec<String> = results.iter().filter_map(|(name, result)| result.as_ref().err().map(|e| format!("{name}: {e}"))).collect();
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// A provider that retries a failed step on its own, beside the engine's schedule.
struct Retrying(Filler);

impl Orchestrator for Retrying {
    fn step(&mut self, id: &str, unit: &Unit, now: &str) -> Result<String, String> {
        self.0.step(id, unit, now).or_else(|_| self.0.step(id, unit, now))
    }

    fn step_killed(&mut self, id: &str, unit: &Unit, now: &str, reached: &dyn Fn() -> bool) {
        self.0.step_killed(id, unit, now, reached)
    }

    fn step_during(&mut self, id: &str, unit: &Unit, now: &str, reached: &dyn Fn() -> bool, meanwhile: &mut dyn FnMut()) -> Result<String, String> {
        self.0.step_during(id, unit, now, reached, meanwhile)
    }

    fn invocations(&self, id: &str) -> u32 {
        self.0.record(id).map_or(0, |r| r.invocations)
    }
}

/// The suite fails a filler that adds a retry layer of its own.
#[test]
fn a_filler_retrying_failed_steps_fails_the_one_retry_layer_scenario() {
    let retrying = |target: &Target| -> Box<dyn Orchestrator> { Box::new(Retrying(Filler::over(target))) };
    let outcome = suite::one_retry_layer(&bin("contextful"), &retrying);
    assert!(outcome.as_ref().is_err_and(|e| e.contains("retries no failed step")), "{outcome:?}");
}
