use std::fmt;

/// A refusal the baseline gate raises. Each variant is the error its clause names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalError {
    /// A baseline entry naming no report field, under-sampled, or malformed
    /// (`assurance.baseline.unresolvable-path`). `path` is `_run` for the run block and
    /// empty for the file as a whole.
    BaselinePathUnresolved { path: String, reason: String },
    /// A baseline run block differing from the run's configuration
    /// (`assurance.baseline.run-stamp-drift`).
    BaselineRunStampMismatch { recorded: String, run: String },
}

impl EvalError {
    /// The error identifier, as the spec registers it.
    pub fn code(&self) -> &'static str {
        match self {
            EvalError::BaselinePathUnresolved { .. } => "BaselinePathUnresolved",
            EvalError::BaselineRunStampMismatch { .. } => "BaselineRunStampMismatch",
        }
    }

    pub(crate) fn unresolved(path: &str, reason: impl Into<String>) -> Self {
        EvalError::BaselinePathUnresolved { path: path.to_string(), reason: reason.into() }
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
        }
    }
}

impl std::error::Error for EvalError {}
