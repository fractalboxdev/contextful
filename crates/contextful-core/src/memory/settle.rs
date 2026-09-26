//! `read.settle`: prediction registrations, observations, and the label view's window.

use super::MemoryError;
use crate::time::Instant;
use serde::Deserialize;

/// Inclusive window past a deadline within which an observation is scored, in seconds
/// (`read.settle.grace-window`).
pub const GRACE_WINDOW_SECS: u64 = 86_400;

/// The label view (`read.settle.grace-window`).
pub const LABEL_VIEW: &str = "CREATE VIEW outcome_labels AS \
SELECT p.prediction_id, p.subject, p.resolution_source, o.verdict, o.signal, o.settling_citation, \
epoch(CAST(o.observed_at AS TIMESTAMPTZ)) - epoch(CAST(p.predicted_at AS TIMESTAMPTZ)) AS lead_time_s \
FROM predictions p JOIN outcomes o ON o.prediction_id = p.prediction_id \
WHERE o.observed_at IS NOT NULL AND o.signal IS DISTINCT FROM 'self-rated' \
AND epoch(CAST(o.observed_at AS TIMESTAMPTZ)) >= epoch(CAST(p.predicted_at AS TIMESTAMPTZ)) \
AND epoch(CAST(o.observed_at AS TIMESTAMPTZ)) <= epoch(CAST(p.deadline_at AS TIMESTAMPTZ)) + 86400";

/// When a prediction resolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    Horizon(u64),
    Deadline(Instant),
    Watch,
}

/// Who settles a prediction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Metric,
    Adjudicator,
    Manual,
}

impl Source {
    fn parse(s: &str) -> Option<Source> {
        Some(match s {
            "metric" => Source::Metric,
            "adjudicator" => Source::Adjudicator,
            "manual" => Source::Manual,
            _ => return None,
        })
    }
}

/// A registration as submitted.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRegistration {
    pub prediction_id: String,
    pub subject: String,
    pub predicted_at: Instant,
    #[serde(default)]
    pub horizon_secs: Option<u64>,
    #[serde(default)]
    pub deadline_at: Option<Instant>,
    #[serde(default)]
    pub watch: bool,
    #[serde(default)]
    pub resolution_source: Option<String>,
    #[serde(default)]
    pub comparator: Option<String>,
}

/// A checked registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    pub prediction_id: String,
    pub subject: String,
    pub predicted_at: Instant,
    pub form: Form,
    pub source: Source,
    pub comparator: Option<String>,
}

impl Registration {
    /// Check a registration: exactly one form, one source, and a comparator exactly when
    /// the source is `metric` (`read.settle.registration`).
    pub fn check(raw: RawRegistration) -> Result<Registration, MemoryError> {
        let invalid = |why: String| MemoryError::OutcomeRegistrationInvalid(format!("prediction `{}`: {why}", raw.prediction_id));
        let forms: Vec<Form> = [raw.horizon_secs.map(Form::Horizon), raw.deadline_at.map(Form::Deadline), raw.watch.then_some(Form::Watch)]
            .into_iter()
            .flatten()
            .collect();
        let [form] = forms.as_slice() else {
            return Err(invalid(format!("names {} forms; exactly one of horizon, deadline or watch", forms.len())));
        };
        let source = match raw.resolution_source.as_deref() {
            None => return Err(invalid("names no resolution source".into())),
            Some(s) => Source::parse(s).ok_or_else(|| invalid(format!("source `{s}` is not metric, adjudicator or manual")))?,
        };
        match (source, &raw.comparator) {
            (Source::Metric, None) => return Err(invalid("a `metric` source carries a comparator".into())),
            (Source::Adjudicator | Source::Manual, Some(_)) => return Err(invalid("only a `metric` source carries a comparator".into())),
            _ => {}
        }
        Ok(Registration {
            form: *form,
            source,
            comparator: raw.comparator.clone(),
            prediction_id: raw.prediction_id,
            subject: raw.subject,
            predicted_at: raw.predicted_at,
        })
    }

    /// The deadline a label is scored against; an open watch has none.
    pub fn deadline(&self) -> Option<Instant> {
        match self.form {
            Form::Horizon(secs) => Some(self.predicted_at.plus_secs(secs)),
            Form::Deadline(at) => Some(at),
            Form::Watch => None,
        }
    }
}

/// An observation as submitted.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub prediction_id: String,
    pub observed_at: Instant,
    #[serde(default)]
    pub verdict: Option<bool>,
    #[serde(default)]
    pub resolution_source: Option<String>,
    #[serde(default)]
    pub signal: Option<String>,
    #[serde(default)]
    pub settling_citation: Option<String>,
}

/// Check an observation against its registration. A verdict whose source is absent or
/// differs refuses (`read.settle.source-mismatch`); an `adjudicator` or `manual` verdict
/// carries an `http` or `https` settling citation (`read.settle.settling-citation`).
pub fn observe(registration: &Registration, obs: &Observation) -> Result<(), MemoryError> {
    if obs.verdict.is_none() {
        return Ok(());
    }
    let source = obs.resolution_source.as_deref().and_then(Source::parse);
    if source != Some(registration.source) {
        return Err(MemoryError::OutcomeSourceMismatch(format!(
            "prediction `{}` settles by {:?}; the verdict names {}",
            registration.prediction_id,
            registration.source,
            obs.resolution_source.as_deref().map_or("no source".to_string(), |s| format!("`{s}`"))
        )));
    }
    if matches!(registration.source, Source::Adjudicator | Source::Manual) {
        let cited = obs.settling_citation.as_deref().is_some_and(|c| c.starts_with("https://") || c.starts_with("http://"));
        if !cited {
            return Err(MemoryError::OutcomeCitationMissing(format!(
                "prediction `{}`: a judged verdict carries an http or https settling citation",
                registration.prediction_id
            )));
        }
    }
    Ok(())
}

/// Whether the label join keeps an observation: from the prediction instant through the
/// deadline plus the inclusive grace (`read.settle.grace-window`).
pub fn in_label_window(predicted_at: Instant, deadline: Option<Instant>, observed_at: Instant) -> bool {
    deadline.is_some_and(|d| observed_at >= predicted_at && observed_at <= d.plus_secs(GRACE_WINDOW_SECS))
}
