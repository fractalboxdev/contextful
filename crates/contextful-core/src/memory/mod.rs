//! Synthesized memory's pure domain: the five table shapes and the relation vocabulary,
//! extraction validation and its attempt budget, entity resolution, claim standing and
//! revision, the recall evidence gate, and settled outcomes.

pub mod declare;
pub mod error;
pub mod recall;
pub mod resolve;
pub mod revise;
pub mod settle;
pub mod synthesize;

pub use error::MemoryError;
