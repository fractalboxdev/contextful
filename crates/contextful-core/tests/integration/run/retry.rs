//! `run.retry`: the failure taxonomy, the schedule and the pure retry decision.

use contextful_core::run::failure::{Failure, FailureTag};
use contextful_core::run::retry::{
    decide, delay_ms, Backoff, Decision, Schedule, ScheduleSpec, DEFAULT_ATTEMPTS, DEFAULT_BASE_MS, DEFAULT_JITTER_CEILING_MS,
    DEFAULT_MAX_DELAY_MS, RETRY_AFTER_CEILING_SECS, SINGLE_ATTEMPT,
};
use contextful_core::run::RunError;

fn transient() -> Failure {
    Failure::new(FailureTag::Transient, "connection reset")
}

fn failed_tag(d: &Decision) -> Option<FailureTag> {
    match d {
        Decision::Fail { error: RunError::StepFailed { failure, .. }, .. } => Some(failure.tag),
        _ => None,
    }
}

/// One tagged failure type crosses every port: `Transient`, `Permanent`, `SchemaIncompatible`, `AuthExpired`,
/// `RateLimited`, `Config`, `Storage`, `UnknownConnector`, `SecretNotFound` and `Canceled`. A caller branches on
/// the tag, never on a message.
// spec: run.retry.failure-taxonomy@309d838f
#[test]
fn ten_tags_cross_every_port_and_the_decision_reads_the_tag_alone() {
    let names: Vec<&str> = FailureTag::ALL.iter().map(|t| t.name()).collect();
    assert_eq!(
        names,
        ["Transient", "Permanent", "SchemaIncompatible", "AuthExpired", "RateLimited", "Config", "Storage", "UnknownConnector", "SecretNotFound", "Canceled"]
    );
    for t in FailureTag::ALL {
        assert_eq!(FailureTag::parse(t.name()), Some(t));
    }
    assert_eq!(FailureTag::parse("transient"), None);
    // Two failures differing only in message decide identically; a message naming another tag changes nothing.
    let s = Schedule::default();
    let a = decide(&s, "pull-0", 1, &Failure::new(FailureTag::Permanent, "try again later, Transient"), 7);
    let b = decide(&s, "pull-0", 1, &Failure::new(FailureTag::Permanent, "gone"), 7);
    assert_eq!(failed_tag(&a), Some(FailureTag::Permanent));
    assert_eq!(failed_tag(&b), Some(FailureTag::Permanent));
}

/// A schedule attaches to a step and carries a fixed or exponential backoff with base and factor, a total attempt
/// count including the first, a per-delay clamp, a jitter ceiling and `retry_on`.
// spec: run.retry.schedule@9a9fc881
#[test]
fn a_schedule_carries_backoff_attempts_clamp_jitter_and_retry_on() {
    let spec: ScheduleSpec =
        toml::from_str("backoff = \"fixed\"\nbase_ms = 40\nattempts = 3\nmax_delay_ms = 1000\njitter_ms = 0\nretry_on = [\"RateLimited\"]\n").unwrap();
    let s = spec.compile().unwrap();
    assert_eq!(s.backoff, Backoff::Fixed { delay_ms: 40 });
    assert_eq!(s.attempts, 3);
    assert_eq!(s.max_delay_ms, 1000);
    assert_eq!(s.jitter_ceiling_ms, 0);
    assert_eq!(s.retry_on, [FailureTag::RateLimited]);
    assert_eq!(delay_ms(&s, 1, 9), 40);
    assert_eq!(delay_ms(&s, 2, 9), 40);

    let spec: ScheduleSpec = toml::from_str("backoff = \"exponential\"\nbase_ms = 10\nfactor = 3\nmax_delay_ms = 50\njitter_ms = 0\n").unwrap();
    let s = spec.compile().unwrap();
    assert_eq!(s.backoff, Backoff::Exponential { base_ms: 10, factor: 3 });
    assert_eq!([1, 2, 3].map(|a| delay_ms(&s, a, 0)), [10, 30, 50], "each delay clamps at the schedule's maximum");
    // The first attempt counts: a 3-attempt schedule fails on the close of attempt 3.
    let s = ScheduleSpec { attempts: Some(3), jitter_ms: Some(0), ..Default::default() }.compile().unwrap();
    assert!(matches!(decide(&s, "pull-0", 2, &transient(), 0), Decision::Retry { .. }));
    assert_eq!(failed_tag(&decide(&s, "pull-0", 3, &transient(), 0)), Some(FailureTag::Transient));
}

/// The default schedule is exponential from a 100 ms base with factor 2, to 5 attempts in total, each delay
/// clamped at 30 s, plus seeded jitter below 250 ms.
// spec: run.retry.default-policy@e7c6d19d
#[test]
fn the_default_schedule_doubles_from_100_ms_over_5_attempts() {
    assert_eq!((DEFAULT_BASE_MS, DEFAULT_ATTEMPTS, DEFAULT_MAX_DELAY_MS, DEFAULT_JITTER_CEILING_MS), (100, 5, 30_000, 250));
    let s = Schedule::default();
    assert_eq!(s, ScheduleSpec::default().compile().unwrap(), "an undeclared schedule is the default");
    assert_eq!(s.backoff, Backoff::Exponential { base_ms: 100, factor: 2 });
    let bare = Schedule { jitter_ceiling_ms: 0, ..s.clone() };
    assert_eq!([1, 2, 3, 4].map(|a| delay_ms(&bare, a, 0)), [100, 200, 400, 800]);
    assert_eq!(delay_ms(&bare, 12, 0), 30_000, "a delay clamps at 30 s");
    // Jitter is seeded, below 250 ms, and differs across seeds.
    let mut seen = std::collections::BTreeSet::new();
    for seed in 0..64u64 {
        let d = delay_ms(&s, 1, seed);
        assert!((100..350).contains(&d), "{d}");
        assert_eq!(d, delay_ms(&s, 1, seed));
        seen.insert(d);
    }
    assert!(seen.len() > 8, "jitter spreads runs apart: {seen:?}");
    // A fifth transient failure exhausts the schedule.
    assert!(matches!(decide(&s, "pull-0", 4, &transient(), 1), Decision::Retry { .. }));
    assert_eq!(failed_tag(&decide(&s, "pull-0", 5, &transient(), 1)), Some(FailureTag::Transient));
}

/// A schedule of 1 attempts is the no-retry schedule.
// spec: run.retry.single-attempt@6e441e18
#[test]
fn a_one_attempt_schedule_never_retries() {
    assert_eq!(SINGLE_ATTEMPT, 1);
    let s = Schedule::single_attempt();
    assert_eq!(s.attempts, 1);
    assert_eq!(ScheduleSpec { attempts: Some(1), ..Default::default() }.compile().unwrap(), s);
    for tag in [FailureTag::Transient, FailureTag::RateLimited] {
        assert_eq!(failed_tag(&decide(&s, "pull-0", 1, &Failure::new(tag, "x"), 3)), Some(tag));
    }
}

/// `Transient` and `RateLimited` retry; every other tag stands as the step's answer whatever budget remains.
/// `retry_on` narrows that pair.
// spec: run.retry.retryable-classes@e1d43cb2
#[test]
fn only_transient_and_rate_limited_retry() {
    let s = Schedule::default();
    for tag in FailureTag::ALL {
        let d = decide(&s, "pull-0", 1, &Failure::new(tag, "x"), 3);
        if matches!(tag, FailureTag::Transient | FailureTag::RateLimited) {
            assert!(matches!(d, Decision::Retry { .. }), "{tag} retries");
        } else {
            assert_eq!(failed_tag(&d), Some(tag), "{tag} stands with 4 attempts left");
        }
    }
    let narrowed = ScheduleSpec { retry_on: Some(vec!["RateLimited".into()]), ..Default::default() }.compile().unwrap();
    assert_eq!(failed_tag(&decide(&narrowed, "pull-0", 1, &transient(), 3)), Some(FailureTag::Transient));
    assert!(matches!(decide(&narrowed, "pull-0", 1, &Failure::new(FailureTag::RateLimited, "x"), 3), Decision::Retry { .. }));
}

/// A `retry_on` naming any tag outside `Transient` and `RateLimited` raises `RunRetryTagUnretryable` at plan
/// compile.
// spec: run.retry.unretryable-tag@487e4452
#[test]
fn retry_on_naming_an_unretryable_tag_is_refused_at_compile() {
    for name in ["Permanent", "Canceled", "AuthExpired", "Flaky"] {
        let spec = ScheduleSpec { retry_on: Some(vec!["Transient".into(), name.into()]), ..Default::default() };
        match spec.compile() {
            Err(RunError::RunRetryTagUnretryable(m)) => assert!(m.contains(name), "{m}"),
            other => panic!("{name}: {other:?}"),
        }
    }
    assert!(ScheduleSpec { retry_on: Some(vec!["Transient".into(), "RateLimited".into()]), ..Default::default() }.compile().is_ok());
}

/// A non-retryable failure, or a step exhausting its schedule, raises `StepFailed` carrying the step label and
/// the underlying tag.
// spec: run.retry.step-failed@4b70bd28
#[test]
fn a_terminal_or_exhausted_step_fails_with_its_label_and_tag() {
    let s = ScheduleSpec { attempts: Some(2), ..Default::default() }.compile().unwrap();
    for (attempt, failure) in [(1, Failure::new(FailureTag::AuthExpired, "token expired")), (2, transient())] {
        match decide(&s, "pull-3", attempt, &failure, 0) {
            Decision::Fail { error, .. } => {
                let text = error.to_string();
                assert!(text.starts_with("StepFailed"), "{text}");
                assert!(text.contains("pull-3") && text.contains(failure.tag.name()), "{text}");
                assert_eq!(error, RunError::StepFailed { label: "pull-3".into(), failure: failure.clone() });
            }
            other => panic!("{other:?}"),
        }
    }
}

/// A refusal whose check is pure over static input is terminal and consumes no attempt budget.
// spec: run.retry.deterministic-verdict@b7f0313c
#[test]
fn a_deterministic_refusal_is_terminal_and_spends_no_attempt() {
    let s = Schedule::default();
    let pure = Failure::deterministic(FailureTag::Transient, "the plan names an unknown column");
    match decide(&s, "pull-0", 1, &pure, 0) {
        Decision::Fail { consumed_attempt, error } => {
            assert!(!consumed_attempt);
            assert!(matches!(error, RunError::StepFailed { .. }));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(decide(&s, "pull-0", 1, &Failure::new(FailureTag::Permanent, "x"), 0), Decision::Fail { consumed_attempt: true, .. }));
}

/// A server-supplied `Retry-After` supersedes the computed delay up to 300 s; a longer one closes the step as
/// `RateLimited` and the run is rescheduled instead of sleeping.
// spec: run.retry.retry-after@b7ad72c0
#[test]
fn retry_after_supersedes_the_delay_up_to_300_s() {
    assert_eq!(RETRY_AFTER_CEILING_SECS, 300);
    let s = Schedule::default();
    let limited = |secs| Failure::new(FailureTag::RateLimited, "slow down").with_retry_after(secs);
    assert_eq!(decide(&s, "pull-0", 1, &limited(7), 0), Decision::Retry { delay_ms: 7_000 });
    assert_eq!(decide(&s, "pull-0", 1, &limited(300), 0), Decision::Retry { delay_ms: 300_000 }, "not clamped at 30 s");
    match decide(&s, "pull-0", 1, &Failure::new(FailureTag::Transient, "busy").with_retry_after(301), 0) {
        Decision::Reschedule { error: RunError::StepFailed { failure, .. }, retry_after_secs } => {
            assert_eq!(failure.tag, FailureTag::RateLimited);
            assert_eq!(retry_after_secs, 301);
        }
        other => panic!("{other:?}"),
    }
}

/// The retry decision is a pure function of the one-based attempt that just closed and the classified failure,
/// jitter derived from an injected seed. The runner owns the sleep and the attempt counter.
// spec: run.retry.decision-is-pure@bb5beafc
#[test]
fn the_decision_is_a_function_of_attempt_failure_and_seed() {
    let s = Schedule::default();
    for attempt in 1..5 {
        for seed in [0u64, 1, 99, u64::MAX] {
            assert_eq!(decide(&s, "pull-0", attempt, &transient(), seed), decide(&s, "pull-0", attempt, &transient(), seed));
        }
    }
    let delays: Vec<_> = (1..5)
        .map(|a| match decide(&Schedule { jitter_ceiling_ms: 0, ..s.clone() }, "pull-0", a, &transient(), 0) {
            Decision::Retry { delay_ms } => delay_ms,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(delays, [100, 200, 400, 800], "the one-based attempt that closed picks the delay");
}
