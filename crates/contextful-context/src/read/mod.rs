//! The read face: the embedded engine, the session's registered relations, statements
//! and templates, descriptions and file previews, and ranked retrieval.

pub mod audit;
pub(crate) mod engine;
pub mod evidence;
pub mod face;
pub mod fault;
mod input;
mod pin;
pub mod pool;
pub mod recall;
pub mod results;
pub mod retrieve;
mod shape;

pub use audit::{audit_reads, AUDIT_READS};
pub use engine::ENGINE;
pub use face::{operator_query, Face, ReadOptions};
pub use fault::ReadFault;
pub use pool::{PoolCounts, SessionPool};
pub use recall::RecallRequest;
pub use results::{ResultCache, ResultCounts};
pub use retrieve::RetrieveRequest;
pub use shape::aggregate_shape;
