//! The `run` contract's durable-run domain: the journal's keys and placement, cursor
//! positions, suspension, the retry decision, execution owners, the stop mark, the run
//! record and the plan a run pins. The runner and its storage are `contextful-engine`.

pub mod advance;
pub mod cancel;
pub mod derive;
pub mod drive;
pub mod error;
pub mod failure;
pub mod journal;
pub mod own;
pub mod plan;
pub mod ports;
pub mod project;
pub mod record;
pub mod retry;
pub mod suspend;

pub use error::RunError;
pub use failure::{Failure, FailureTag};
