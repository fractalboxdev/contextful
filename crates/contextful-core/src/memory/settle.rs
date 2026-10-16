//! `read.settle`: prediction registrations, observations, and the label view's window.

use super::MemoryError;
use crate::time::Instant;
use serde::Deserialize;

/// Inclusive window past a deadline within which an observation is scored, in seconds
/// (`read.settle.grace-window`).
pub const GRACE_WINDOW_SECS: u64 = 86_400;

/// The longest registration `statement`, in bytes (`read.settle.statement-bound`).
pub const STATEMENT_MAX_BYTES: usize = 4096;

/// A `deadline_at` as an RFC 3339 instant, or a bare `YYYY-MM-DD` resolved to the next
/// UTC midnight (`read.settle.deadline-date`, `store.bound-time.instant-comparison`).
fn deadline<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Instant>, D::Error> {
    let s: Option<String> = Option::deserialize(d)?;
    s.map(|s| crate::store::bound_time::Bound::parse(&s).map(|b| b.at).map_err(serde::de::Error::custom)).transpose()
}

/// A metric reference's table and series (`read.settle.metric-reference`).
fn metric_parts(reference: &str) -> Option<(&str, &str)> {
    reference.split_once(':').filter(|(table, series)| !table.trim().is_empty() && !series.trim().is_empty())
}

// One text for both the join body and the view statement.
macro_rules! label_select {
    () => {
        "SELECT p.prediction_id, p.subject, p.resolution_source, o.verdict, o.signal, o.settling_citation, \
epoch(CAST(o.observed_at AS TIMESTAMP)) - epoch(CAST(p.predicted_at AS TIMESTAMP)) AS lead_time_s \
FROM predictions p JOIN outcomes o ON o.prediction_id = p.prediction_id \
WHERE o.observed_at IS NOT NULL AND o.signal IS DISTINCT FROM 'self-rated' \
AND epoch(CAST(o.observed_at AS TIMESTAMP)) >= epoch(CAST(p.predicted_at AS TIMESTAMP)) \
AND epoch(CAST(o.observed_at AS TIMESTAMP)) <= epoch(CAST(p.deadline_at AS TIMESTAMP)) + 86400"
    };
}

/// The label join's body over relations named `predictions` and `outcomes`
/// (`read.settle.grace-window`).
pub const LABEL_SELECT: &str = label_select!();

/// The label view (`read.settle.grace-window`).
pub const LABEL_VIEW: &str = concat!("CREATE VIEW outcome_labels AS ", label_select!());

/// The label view's name.
pub const LABEL_VIEW_NAME: &str = "outcome_labels";

/// The label view as common table expressions over a declared `predictions` and
/// `outcomes` relation pair, each rendered as a quoted identifier, ahead of `then`
/// (`read.settle.label-join`).
pub fn label_join(predictions: &str, outcomes: &str, then: &str) -> String {
    let quote = |name: &str| format!("\"{}\"", name.replace('"', "\"\""));
    format!(
        "WITH predictions AS (SELECT * FROM {}), outcomes AS (SELECT * FROM {}), {LABEL_VIEW_NAME} AS ({LABEL_SELECT}) {then}",
        quote(predictions),
        quote(outcomes)
    )
}

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
    pub fn name(self) -> &'static str {
        match self {
            Source::Metric => "metric",
            Source::Adjudicator => "adjudicator",
            Source::Manual => "manual",
        }
    }

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
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRegistration {
    pub prediction_id: String,
    pub subject: String,
    pub predicted_at: Instant,
    #[serde(default)]
    pub horizon_secs: Option<u64>,
    #[serde(default, deserialize_with = "deadline")]
    pub deadline_at: Option<Instant>,
    #[serde(default)]
    pub watch: bool,
    #[serde(default)]
    pub resolution_source: Option<String>,
    #[serde(default)]
    pub comparator: Option<String>,
    /// The falsifiable claim the prediction scores (`read.settle.statement-bound`).
    #[serde(default)]
    pub statement: Option<String>,
    /// The series a `metric` source resolves against, `<table>:<series_id>`
    /// (`read.settle.metric-reference`).
    #[serde(default)]
    pub metric: Option<String>,
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
    pub statement: Option<String>,
    pub metric: Option<String>,
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
        if let Some(statement) = &raw.statement {
            if statement.len() > STATEMENT_MAX_BYTES {
                return Err(MemoryError::OutcomeStatementTooLong(format!(
                    "prediction `{}`: the statement holds {} B, over {STATEMENT_MAX_BYTES} B",
                    raw.prediction_id,
                    statement.len()
                )));
            }
        }
        if let Some(metric) = &raw.metric {
            let why = match (source, metric_parts(metric)) {
                (Source::Metric, Some(_)) => None,
                (Source::Metric, None) => Some(format!("`{metric}` is no `<table>:<series_id>` reference")),
                (other, _) => Some(format!("a `{}` source names no metric series", other.name())),
            };
            if let Some(why) = why {
                return Err(MemoryError::OutcomeMetricReferenceInvalid(format!("prediction `{}`: {why}", raw.prediction_id)));
            }
        }
        Ok(Registration {
            statement: raw.statement.clone(),
            metric: raw.metric.clone(),
            form: *form,
            source,
            comparator: raw.comparator.clone(),
            prediction_id: raw.prediction_id,
            subject: raw.subject,
            predicted_at: raw.predicted_at,
        })
    }

    /// The registration as submitted: the inverse of [`Registration::check`].
    pub fn raw(&self) -> RawRegistration {
        RawRegistration {
            prediction_id: self.prediction_id.clone(),
            subject: self.subject.clone(),
            predicted_at: self.predicted_at,
            horizon_secs: match self.form {
                Form::Horizon(secs) => Some(secs),
                _ => None,
            },
            deadline_at: match self.form {
                Form::Deadline(at) => Some(at),
                _ => None,
            },
            watch: self.form == Form::Watch,
            resolution_source: Some(self.source.name().to_string()),
            comparator: self.comparator.clone(),
            statement: self.statement.clone(),
            metric: self.metric.clone(),
        }
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
#[derive(Debug, Clone, PartialEq, Deserialize)]
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
    /// The series the observation read, `<table>:<series_id>` (`read.settle.source-mismatch`).
    #[serde(default)]
    pub metric: Option<String>,
}

/// Check an observation against its registration. A verdict whose source is absent or
/// differs refuses (`read.settle.source-mismatch`); an `adjudicator` or `manual` verdict
/// carries an `http` or `https` settling citation (`read.settle.settling-citation`).
pub fn observe(registration: &Registration, obs: &Observation) -> Result<(), MemoryError> {
    if let (Some(registered), Some(observed)) = (&registration.metric, &obs.metric) {
        if registered != observed {
            return Err(MemoryError::OutcomeSourceMismatch(format!(
                "prediction `{}` settles against the `metric` series `{registered}`; the observation names `{observed}`",
                registration.prediction_id
            )));
        }
    }
    if obs.verdict.is_none() {
        return Ok(());
    }
    let source = obs.resolution_source.as_deref().and_then(Source::parse);
    if source != Some(registration.source) {
        return Err(MemoryError::OutcomeSourceMismatch(format!(
            "prediction `{}` settles by `{}`; the verdict names {}",
            registration.prediction_id,
            registration.source.name(),
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
