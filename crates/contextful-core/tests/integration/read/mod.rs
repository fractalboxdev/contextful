//! The `read` contract's domain, one module per operation.

mod embed;
mod face;
mod rank;
mod respond;
mod retrieve;
mod template;

use contextful_core::time::Instant;

pub fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

pub fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}
