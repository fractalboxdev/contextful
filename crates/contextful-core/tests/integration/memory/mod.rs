//! Synthesized memory's domain, one module per operation.

mod calibrate;
mod declare;
mod recall;
mod resolve;
mod revise;
mod settle;
mod synthesize;

use contextful_core::time::Instant;

pub fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

pub const FACT_COLUMNS: &str = r#"["claim_id", "subject", "predicate", "object", "scope", "tier", "confidence", "valid_from", "valid_to", "evidence", "superseded_by", "grant_id", "agent"]"#;
