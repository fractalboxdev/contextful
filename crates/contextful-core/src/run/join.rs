//! `run.journal.fan-out-join`: fan-out bodies and the explicit join node they rejoin at.
//!
//! A [`FanOut`] holds each branch's outcome and hands no output back except through
//! [`FanOut::join`], so bodies rejoin only at a declared [`Join`]. A failed branch fails
//! the join unless it declares `allow_partial`; a partial join records each failed
//! branch's label and failure tag on the run record.

use super::failure::{Failure, FailureTag};
use super::record::RunRow;
use serde::{Deserialize, Serialize};

/// An explicit join node: its label and whether it admits a partial result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Join {
    pub label: String,
    #[serde(default)]
    pub allow_partial: bool,
}

/// A branch that failed into a partial join, as the run record carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedBranch {
    pub label: String,
    pub tag: FailureTag,
}

/// The outcomes of one fan-out's bodies, in branch order, awaiting their join.
#[derive(Debug, Clone, Default, PartialEq)]
#[must_use = "fan-out bodies rejoin only at an explicit join node"]
pub struct FanOut<T> {
    branches: Vec<(String, Result<T, Failure>)>,
}

/// What a join passed on: each succeeding branch's output, labelled, and each failed
/// branch a partial join admitted.
#[derive(Debug, Clone, PartialEq)]
pub struct Joined<T> {
    pub outputs: Vec<(String, T)>,
    pub failed: Vec<FailedBranch>,
}

impl<T> FanOut<T> {
    pub fn new() -> FanOut<T> {
        FanOut { branches: Vec::new() }
    }

    /// Record branch `label`'s outcome.
    pub fn branch(mut self, label: &str, outcome: Result<T, Failure>) -> FanOut<T> {
        self.branches.push((label.to_string(), outcome));
        self
    }

    /// Rejoin at `join`. A failed branch fails the join with that branch's tag, naming the
    /// branch and the join, unless `join` declares `allow_partial`, which passes the
    /// succeeding outputs on and lists every failed branch.
    pub fn join(self, join: &Join) -> Result<Joined<T>, Failure> {
        let mut outputs = Vec::new();
        let mut failed = Vec::new();
        for (label, outcome) in self.branches {
            match outcome {
                Ok(output) => outputs.push((label, output)),
                Err(failure) if !join.allow_partial => {
                    return Err(Failure {
                        message: format!("branch `{label}` failed into join `{}`, which declares no `allow_partial`: {}", join.label, failure.message),
                        ..failure
                    })
                }
                Err(failure) => failed.push(FailedBranch { label, tag: failure.tag }),
            }
        }
        Ok(Joined { outputs, failed })
    }
}

impl<T> Joined<T> {
    /// Record each failed branch's label and failure tag on the run record, so a partial
    /// result never reads as a complete one.
    pub fn record(&self, row: &mut RunRow) {
        row.failed_branches.extend(self.failed.iter().cloned());
    }
}
