//! The systems figures a run reports beside its quality figures
//! (`assurance.evaluate.systems-metrics`): tokens per query, latency and cost, each case
//! bucketed by its corpus's size in tokens relative to the reader's context window.

use serde::{Deserialize, Serialize};

/// Characters per token of the estimate every token count in a report uses.
pub const CHARS_PER_TOKEN: u64 = 4;

/// One case's systems figures.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Systems {
    /// Tokens the reader reads for the case: the question and every row it was handed.
    pub tokens: u64,
    /// Wall-clock milliseconds the case's ranked calls took.
    pub latency_ms: f64,
    /// What the case's model calls cost; zero in the deterministic tier, which calls none.
    pub cost: f64,
    /// Tokens across every row of the case's corpus.
    pub corpus_tokens: u64,
    /// Tokens the reader's context window holds.
    pub context_window: u64,
}

impl Systems {
    /// The case's corpus-size bucket.
    pub fn bucket(&self) -> String {
        bucket(self.corpus_tokens, self.context_window)
    }
}

/// The token estimate of `text`: its characters over [`CHARS_PER_TOKEN`], rounded up.
pub fn tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(CHARS_PER_TOKEN)
}

/// The bucket of a corpus of `corpus_tokens` read through a `window`-token context:
/// `le_<n>x`, `n` the least power of two at or above the corpus-to-window ratio. A corpus
/// fitting the window is `le_1x`; a zero window reads every corpus as unbounded, `over_window`.
pub fn bucket(corpus_tokens: u64, window: u64) -> String {
    if window == 0 {
        return "over_window".into();
    }
    let ratio = corpus_tokens.div_ceil(window).max(1);
    format!("le_{}x", ratio.next_power_of_two())
}
