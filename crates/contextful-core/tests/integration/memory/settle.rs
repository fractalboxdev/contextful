//! `read.settle`: registrations, observations and the label window.

use super::at;
use contextful_core::memory::settle::{in_label_window, observe, Form, Observation, RawRegistration, Registration, GRACE_WINDOW_SECS};
use contextful_core::memory::MemoryError;

fn raw(source: Option<&str>, comparator: Option<&str>) -> RawRegistration {
    RawRegistration {
        prediction_id: "p1".into(),
        subject: "acme ships v2".into(),
        predicted_at: at("2030-01-01T00:00:00Z"),
        horizon_secs: None,
        deadline_at: Some(at("2030-02-01T00:00:00Z")),
        watch: false,
        resolution_source: source.map(str::to_string),
        comparator: comparator.map(str::to_string),
        statement: None,
        metric: None,
    }
}

fn invalid(r: RawRegistration) {
    assert!(matches!(Registration::check(r), Err(MemoryError::OutcomeRegistrationInvalid(_))));
}

fn obs(verdict: Option<bool>, source: Option<&str>, citation: Option<&str>) -> Observation {
    Observation {
        prediction_id: "p1".into(),
        observed_at: at("2030-01-20T00:00:00Z"),
        verdict,
        resolution_source: source.map(str::to_string),
        signal: None,
        settling_citation: citation.map(str::to_string),
        metric: None,
    }
}

/// A registration names exactly one form (relative horizon, absolute deadline, open watch) and one source (`metric`, `adjudicator`, `manual`), with a comparator exactly when the source is `metric`; otherwise it raises `OutcomeRegistrationInvalid`.
// spec: read.settle.registration@c2997af9
#[test]
fn a_registration_names_one_form_one_source_and_a_metric_comparator() {
    assert_eq!(Registration::check(raw(Some("manual"), None)).unwrap().form, Form::Deadline(at("2030-02-01T00:00:00Z")));
    assert!(Registration::check(raw(Some("metric"), Some("revenue >= 1e6"))).is_ok());
    let horizon = RawRegistration { deadline_at: None, horizon_secs: Some(3600), ..raw(Some("adjudicator"), None) };
    assert_eq!(Registration::check(horizon).unwrap().deadline(), Some(at("2030-01-01T01:00:00Z")));
    let watch = RawRegistration { deadline_at: None, watch: true, ..raw(Some("manual"), None) };
    assert_eq!(Registration::check(watch).unwrap().deadline(), None);
    invalid(RawRegistration { horizon_secs: Some(60), ..raw(Some("manual"), None) });
    invalid(RawRegistration { deadline_at: None, ..raw(Some("manual"), None) });
    invalid(raw(None, None));
    invalid(raw(Some("oracle"), None));
    invalid(raw(Some("metric"), None));
    invalid(raw(Some("manual"), Some("x > 1")));
}

/// An observation naming a `metric` series other than the registration's, or carrying a verdict whose resolution source or registered series is absent or differs, raises `OutcomeSourceMismatch`, naming each source by its wire name.
// spec: read.settle.source-mismatch@464b854b
#[test]
fn a_verdict_from_another_source_is_refused() {
    let reg = Registration::check(raw(Some("metric"), Some("revenue >= 1e6"))).unwrap();
    assert_eq!(observe(&reg, &obs(Some(true), Some("metric"), None)), Ok(()));
    assert_eq!(observe(&reg, &obs(None, None, None)), Ok(()), "a self-rated outcome carries no verdict");
    assert!(matches!(observe(&reg, &obs(Some(true), None, None)), Err(MemoryError::OutcomeSourceMismatch(_))));
    match observe(&reg, &obs(Some(true), Some("manual"), Some("https://x"))) {
        Err(e @ MemoryError::OutcomeSourceMismatch(_)) => {
            let text = e.to_string();
            assert!(text.contains("`metric`") && text.contains("`manual`") && !text.contains("Metric"), "{text}");
        }
        other => panic!("{other:?}"),
    }
    let series = Registration::check(RawRegistration { metric: Some("metrics/revenue:acme-q3".into()), ..raw(Some("metric"), Some(">= 1e6")) }).unwrap();
    let on = |metric: Option<&str>| Observation { metric: metric.map(str::to_string), ..obs(Some(true), Some("metric"), None) };
    assert_eq!(observe(&series, &on(Some("metrics/revenue:acme-q3"))), Ok(()));
    match observe(&series, &on(None)) {
        Err(MemoryError::OutcomeSourceMismatch(why)) => assert!(why.contains("metrics/revenue:acme-q3"), "{why}"),
        other => panic!("a verdict states the series it read: {other:?}"),
    }
    let manual = Observation { resolution_source: Some("manual".into()), settling_citation: Some("https://example.org/r".into()), ..on(None) };
    match observe(&series, &manual) {
        Err(MemoryError::OutcomeSourceMismatch(why)) => assert!(why.contains("settles by `metric`; the verdict names `manual`"), "the source is checked before the series: {why}"),
        other => panic!("{other:?}"),
    }
    let open = Observation { verdict: None, resolution_source: None, ..on(None) };
    assert_eq!(observe(&series, &open), Ok(()), "an observation without a verdict settles nothing");
    let elsewhere = Observation { verdict: None, resolution_source: None, ..on(Some("metrics/revenue:acme-q4")) };
    assert!(matches!(observe(&series, &elsewhere), Err(MemoryError::OutcomeSourceMismatch(_))));
    match observe(&series, &on(Some("metrics/revenue:acme-q4"))) {
        Err(MemoryError::OutcomeSourceMismatch(why)) => assert!(why.contains("acme-q4"), "{why}"),
        other => panic!("{other:?}"),
    }
}

/// A registration's optional `statement`, the falsifiable claim it scores, holds at most 4096 B; a longer one raises `OutcomeStatementTooLong`, and the statement takes part in {{read.settle.registration-conflict}}.
// spec: read.settle.statement-bound@0a5e2164
#[test]
fn a_statement_over_the_bound_is_refused() {
    use contextful_core::memory::settle::STATEMENT_MAX_BYTES;
    assert_eq!(STATEMENT_MAX_BYTES, 4096);
    let with = |n: usize| RawRegistration { statement: Some("x".repeat(n)), ..raw(Some("manual"), None) };
    assert_eq!(Registration::check(with(4096)).unwrap().statement.map(|s| s.len()), Some(4096));
    assert!(matches!(Registration::check(with(4097)), Err(MemoryError::OutcomeStatementTooLong(_))));
    let checked = Registration::check(with(10)).unwrap();
    assert_eq!(checked.raw().statement, Some("x".repeat(10)), "the statement round-trips with the registration");
    assert_ne!(checked, Registration::check(with(11)).unwrap(), "a changed statement is a changed registration");
}

/// A registration's optional `metric`, `<table>:<series_id>` with both parts non-empty, names the series a `metric` source resolves against and is never looked up in the store; a malformed one, or one under another source, raises `OutcomeMetricReferenceInvalid`.
// spec: read.settle.metric-reference@dbd54df2
#[test]
fn a_metric_reference_names_a_table_and_series_under_a_metric_source() {
    let with = |metric: &str, source: &str, comparator: Option<&str>| RawRegistration { metric: Some(metric.into()), ..raw(Some(source), comparator) };
    assert_eq!(Registration::check(with("metrics/revenue:acme-q3", "metric", Some(">= 1e6"))).unwrap().metric.as_deref(), Some("metrics/revenue:acme-q3"));
    for bad in ["metrics/revenue", ":acme", "metrics/revenue:", " : "] {
        assert!(matches!(Registration::check(with(bad, "metric", Some(">= 1e6"))), Err(MemoryError::OutcomeMetricReferenceInvalid(_))), "{bad}");
    }
    assert!(matches!(Registration::check(with("metrics/revenue:acme-q3", "manual", None)), Err(MemoryError::OutcomeMetricReferenceInvalid(_))));
}

/// A `deadline_at` given as a bare `YYYY-MM-DD` resolves as {{store.bound-time.instant-comparison}} resolves a date: the deadline is the next UTC midnight, so the whole named day precedes it.
// spec: read.settle.deadline-date@81a7c3f4
#[test]
fn a_bare_date_deadline_runs_through_the_named_day() {
    let parse = |deadline: &str| -> RawRegistration {
        serde_json::from_value(serde_json::json!({ "prediction_id": "p1", "subject": "acme", "predicted_at": "2030-01-01T00:00:00Z",
            "deadline_at": deadline, "resolution_source": "manual" }))
        .unwrap()
    };
    let dated = Registration::check(parse("2030-02-01")).unwrap();
    assert_eq!(dated.deadline(), Some(at("2030-02-02T00:00:00Z")));
    assert_eq!(dated, Registration::check(parse("2030-02-02T00:00:00Z")).unwrap(), "a date and its next midnight are one deadline");
    assert_eq!(Registration::check(parse("2030-02-01T12:00:00Z")).unwrap().deadline(), Some(at("2030-02-01T12:00:00Z")));
    let malformed: Result<RawRegistration, _> = serde_json::from_value(serde_json::json!({ "prediction_id": "p1", "subject": "acme",
        "predicted_at": "2030-01-01T00:00:00Z", "deadline_at": "Feb 1", "resolution_source": "manual" }));
    assert!(malformed.is_err());
}

/// An `adjudicator` or `manual` verdict without an `http` or `https` settling citation raises `OutcomeCitationMissing`; the citation rides the label view.
// spec: read.settle.settling-citation@294fdd07
#[test]
fn a_judged_verdict_carries_a_web_citation() {
    for source in ["adjudicator", "manual"] {
        let reg = Registration::check(raw(Some(source), None)).unwrap();
        assert_eq!(observe(&reg, &obs(Some(false), Some(source), Some("https://example.org/release"))), Ok(()));
        assert_eq!(observe(&reg, &obs(Some(false), Some(source), Some("http://example.org/release"))), Ok(()));
        for bad in [None, Some("ftp://example.org"), Some("see notes")] {
            assert!(matches!(observe(&reg, &obs(Some(false), Some(source), bad)), Err(MemoryError::OutcomeCitationMissing(_))));
        }
    }
    assert!(contextful_core::memory::settle::LABEL_VIEW.contains("o.settling_citation"));
}

/// The label join keeps an observation from the prediction instant through the deadline plus an inclusive grace of 86400 s.
// spec: read.settle.grace-window@b1339024
#[test]
fn the_label_window_runs_through_the_deadline_plus_a_day() {
    assert_eq!(GRACE_WINDOW_SECS, 86_400);
    let (p, d) = (at("2030-01-01T00:00:00Z"), Some(at("2030-02-01T00:00:00Z")));
    assert!(in_label_window(p, d, p));
    assert!(in_label_window(p, d, at("2030-02-02T00:00:00Z")), "the grace is inclusive");
    assert!(!in_label_window(p, d, at("2030-02-02T00:00:01Z")));
    assert!(!in_label_window(p, d, at("2029-12-31T23:59:59Z")));
    assert!(!in_label_window(p, None, at("2030-01-02T00:00:00Z")));
    assert!(contextful_core::memory::settle::LABEL_VIEW.contains("+ 86400"));
}
