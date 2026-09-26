//! `contextful-eval` — the read-path quality harness's pure core: the deterministic
//! retrieval metrics, the absolute floors, and the baseline gate a run report answers to.
//!
//! Nothing here calls a model, opens a store or touches the network; every function is
//! a pure computation over rankings, relevant sets and a serialized run report.

pub mod baseline;
mod error;
pub mod floors;
pub mod metrics;

pub use error::EvalError;
