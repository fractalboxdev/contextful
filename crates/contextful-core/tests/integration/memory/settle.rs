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

/// An observation carrying a verdict whose resolution source is absent or differs from the registration's raises `OutcomeSourceMismatch`.
// spec: read.settle.source-mismatch@9bab79ec
#[test]
fn a_verdict_from_another_source_is_refused() {
    let reg = Registration::check(raw(Some("metric"), Some("revenue >= 1e6"))).unwrap();
    assert_eq!(observe(&reg, &obs(Some(true), Some("metric"), None)), Ok(()));
    assert_eq!(observe(&reg, &obs(None, None, None)), Ok(()), "a self-rated outcome carries no verdict");
    assert!(matches!(observe(&reg, &obs(Some(true), None, None)), Err(MemoryError::OutcomeSourceMismatch(_))));
    assert!(matches!(observe(&reg, &obs(Some(true), Some("manual"), Some("https://x"))), Err(MemoryError::OutcomeSourceMismatch(_))));
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
