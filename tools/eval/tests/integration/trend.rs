use contextful_eval::baseline::Direction;
use contextful_eval::record::Runner;
use contextful_eval::trend::*;

fn runner(nproc: u64) -> Runner {
    Runner { processor: "reference".into(), nproc, memory_limit: Some(12 << 30) }
}

fn figure(value: f64) -> Figure {
    Figure { value, runner: runner(4) }
}

/// A trend figure more than 25 percent worse than its baseline annotates the run report and fails no stage.
// spec: assurance.measure.trend-band@868bdd84
#[test]
fn a_figure_past_the_band_is_annotated_and_fails_nothing() {
    assert_eq!(TREND_BAND, 0.25);
    // A p95 moving from 40 ms to 52 ms: 30 percent worse.
    let c = compare(&figure(52.0), &figure(40.0), Direction::LowerIsBetter);
    assert!(matches!(c, Comparison::Annotated { worse_percent } if (worse_percent - 30.0).abs() < 1e-9), "{c:?}");
    assert_eq!(c.annotation().as_deref(), Some("+30 percent against the baseline"));
    // Exactly at the band, and inside it, nothing is annotated.
    assert!(matches!(compare(&figure(50.0), &figure(40.0), Direction::LowerIsBetter), Comparison::Within { .. }));
    assert_eq!(compare(&figure(49.0), &figure(40.0), Direction::LowerIsBetter).annotation(), None);
    // An improvement is within, with a negative worsening.
    assert!(matches!(compare(&figure(30.0), &figure(40.0), Direction::LowerIsBetter), Comparison::Within { worse_percent } if worse_percent < 0.0));
    // A higher-is-better figure worsens downward.
    assert!(matches!(compare(&figure(0.70), &figure(1.0), Direction::HigherIsBetter), Comparison::Annotated { .. }));
}

/// The run block carries the runner's processor model, processor count and memory limit, and a trend figure compares only against a baseline with the same stamp.
// spec: assurance.measure.runner-stamp@3f6f6e09
#[test]
fn a_figure_compares_only_on_a_matching_runner() {
    let here = Runner::detect();
    assert!(!here.processor.is_empty() && here.nproc >= 1);
    let slower = Figure { value: 400.0, runner: runner(2) };
    assert_eq!(compare(&slower, &figure(40.0), Direction::LowerIsBetter), Comparison::Incomparable);
    let other_limit = Figure { value: 40.0, runner: Runner { memory_limit: None, ..runner(4) } };
    assert_eq!(compare(&other_limit, &figure(40.0), Direction::LowerIsBetter), Comparison::Incomparable);
}

#[test]
fn a_timed_figure_is_the_median_batch_of_p50_and_p95() {
    assert_eq!((TIMING_ITERATIONS, TIMING_BATCHES), (200, 5));
    let batch = |base: f64| (1..=TIMING_ITERATIONS).map(|i| base + i as f64 / 100.0).collect::<Vec<_>>();
    let batches: Vec<Vec<f64>> = [5.0, 1.0, 3.0, 2.0, 4.0].iter().map(|b| batch(*b)).collect();
    let (p50, p95) = median_batch(&batches).unwrap();
    assert!((p50 - 4.0).abs() < 1e-9, "{p50}");
    assert!((p95 - 4.9).abs() < 1e-9, "{p95}");
    assert_eq!(median_batch(&[]), None);
}
