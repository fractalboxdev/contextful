//! `contextful-eval` — the read-path quality harness's pure core: the deterministic
//! retrieval metrics, the absolute floors, the baseline gate a run report answers to, and
//! the target ledger with its records and trend comparison.
//!
//! Nothing here calls a model, opens a store or touches the network. Every function is a
//! pure computation over cases, rankings, relevant sets, a serialized run report or a
//! ledger, except [`record`], which writes and reads one JSON file per measure and stamps
//! the runner it measured on. The runner that lands a corpus and calls the read path is
//! `contextful eval run`.

pub mod baseline;
pub mod case;
pub mod checkpoint;
pub mod convert;
pub mod embed;
mod error;
pub mod floors;
pub mod ledger;
pub mod metrics;
pub mod record;
pub mod report;
pub mod systems;
pub mod trend;

pub use error::EvalError;
