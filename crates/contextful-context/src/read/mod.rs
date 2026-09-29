//! The read face: the embedded engine, the session's registered relations, statements
//! and templates, descriptions and file previews, and ranked retrieval.

pub(crate) mod engine;
pub mod face;
pub mod fault;
pub mod pool;
pub mod retrieve;

pub use engine::ENGINE;
pub use face::{Face, ReadOptions};
pub use fault::ReadFault;
pub use pool::{PoolCounts, SessionPool};
pub use retrieve::RetrieveRequest;
