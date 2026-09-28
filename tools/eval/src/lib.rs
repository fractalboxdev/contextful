//! `contextful-eval` — the read-path quality harness's pure core: the deterministic
//! retrieval metrics, the absolute floors, the baseline gate a run report answers to, and
//! the target ledger with its records and trend comparison.
//!
//! Nothing here calls a model, opens a store or touches the network. Every function is a
//! pure computation over rankings, relevant sets, a serialized run report or a ledger,
//! except [`record`], which writes and reads one JSON file per measure and stamps the
//! runner it measured on.

pub mod baseline;
mod error;
pub mod floors;
pub mod ledger;
pub mod metrics;
pub mod record;
pub mod trend;

pub use error::EvalError;
