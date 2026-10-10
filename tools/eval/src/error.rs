use std::fmt;

/// A refusal the harness raises. Each variant is the error its clause names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalError {
    /// A baseline entry naming no report field, under-sampled, or malformed
    /// (`assurance.baseline.unresolvable-path`). `path` is `_run` for the run block and
    /// empty for the file as a whole.
    BaselinePathUnresolved { path: String, reason: String },
    /// A baseline run block differing from the run's configuration
    /// (`assurance.baseline.run-stamp-drift`).
    BaselineRunStampMismatch { recorded: String, run: String },
    /// A ledger entry naming no clause, method or metric path
    /// (`assurance.measure.unresolved-entry`). `entry` is empty for the ledger as a whole.
    MeasureEntryUnresolved { entry: String, reason: String },
    /// A gate-tier method finishing without writing its record (`assurance.measure.record`).
    MeasureRecordMissing { entry: String, reason: String },
    /// A hosted trace endpoint configured for a run touching a deployed store
    /// (`assurance.baseline.trace-export`).
    TraceExportOutOfPerimeter { endpoint: String, reason: String },
    /// A corpus whose tables declare no zone or row policy, read with no `local:` zone
    /// (`assurance.evaluate.unlabeled-corpus`).
    EvalCorpusUnlabeled { corpus: String },
}

impl EvalError {
    /// The error identifier, as the spec registers it.
    pub fn code(&self) -> &'static str {
        match self {
            EvalError::BaselinePathUnresolved { .. } => "BaselinePathUnresolved",
            EvalError::BaselineRunStampMismatch { .. } => "BaselineRunStampMismatch",
            EvalError::MeasureEntryUnresolved { .. } => "MeasureEntryUnresolved",
            EvalError::MeasureRecordMissing { .. } => "MeasureRecordMissing",
            EvalError::TraceExportOutOfPerimeter { .. } => "TraceExportOutOfPerimeter",
            EvalError::EvalCorpusUnlabeled { .. } => "EvalCorpusUnlabeled",
        }
    }

    pub(crate) fn unresolved(path: &str, reason: impl Into<String>) -> Self {
        EvalError::BaselinePathUnresolved { path: path.to_string(), reason: reason.into() }
    }

    pub(crate) fn entry_unresolved(entry: &str, reason: impl Into<String>) -> Self {
        EvalError::MeasureEntryUnresolved { entry: entry.to_string(), reason: reason.into() }
    }

    pub(crate) fn record_missing(entry: &str, reason: impl Into<String>) -> Self {
        EvalError::MeasureRecordMissing { entry: entry.to_string(), reason: reason.into() }
    }
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvalError::BaselinePathUnresolved { path, reason } => {
                write!(f, "BaselinePathUnresolved: `{path}`: {reason}")
            }
            EvalError::BaselineRunStampMismatch { recorded, run } => {
                write!(f, "BaselineRunStampMismatch: the baseline was recorded at {recorded}; this run is {run}")
            }
            EvalError::MeasureEntryUnresolved { entry, reason } => {
                write!(f, "MeasureEntryUnresolved: `{entry}`: {reason}")
            }
            EvalError::MeasureRecordMissing { entry, reason } => {
                write!(f, "MeasureRecordMissing: `{entry}`: {reason}")
            }
            EvalError::TraceExportOutOfPerimeter { endpoint, reason } => {
                write!(f, "TraceExportOutOfPerimeter: `{endpoint}`: {reason}")
            }
            EvalError::EvalCorpusUnlabeled { corpus } => write!(
                f,
                "EvalCorpusUnlabeled: `{corpus}`: no table declares a zone or row policy; label the corpus or pass a `local:` zone as --zone"
            ),
        }
    }
}

impl std::error::Error for EvalError {}
