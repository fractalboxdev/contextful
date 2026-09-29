//! The secret guard at the one pull path: every batch is masked as the source hands it
//! over, ahead of the recorded pull and the land path (`run.guard-secrets.placement`).

use contextful_core::pipeline::guard::guard_rows;
use contextful_core::run::ports::{Cancellation, Pull, PullRequest, Source};
use contextful_core::run::{Failure, FailureTag};

/// A source whose every pull passes the secret guard; `Engine::run_with` wraps every
/// source it is handed in one.
pub struct Guarded<S> {
    pub inner: S,
    /// Where the per-column masked-cell counts of each pull are reported.
    pub report: fn(&str, &std::collections::BTreeMap<String, usize>),
}

/// Log a pull's masked-cell counts per column to standard error.
pub fn log_counts(step: &str, counts: &std::collections::BTreeMap<String, usize>) {
    if !counts.is_empty() {
        let parts: Vec<String> = counts.iter().map(|(c, n)| format!("{c}={n}")).collect();
        eprintln!("{step}: secret guard masked cells {}", parts.join(" "));
    }
}

impl<S: Source> Source for Guarded<S> {
    fn pull(&mut self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        let bytes = self.inner.pull(request, cancel)?;
        let Ok(mut pull) = Pull::decode(&bytes) else { return Ok(bytes) };
        let counts = guard_rows(&mut pull.rows);
        (self.report)(&request.step_label, &counts);
        if counts.is_empty() {
            return Ok(bytes);
        }
        serde_json::to_vec(&pull).map_err(|e| Failure::new(FailureTag::Permanent, e.to_string()))
    }
}
