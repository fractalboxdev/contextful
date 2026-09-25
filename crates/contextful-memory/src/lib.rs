//! Memory synthesis and the direct write over the store.

pub mod claims;
pub mod synthesize;
pub mod write;

use contextful_context::read::ReadFault;
use contextful_context::ContextError;
use contextful_core::memory::MemoryError;

/// What a synthesis pass or a direct write refuses with.
#[derive(Debug, thiserror::Error)]
pub enum MemoryFault {
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error(transparent)]
    Read(#[from] ReadFault),
    #[error(transparent)]
    Store(#[from] ContextError),
    /// The credential holds no grant for the named action on the named table.
    #[error("{0}")]
    Denied(String),
    /// The manifest does not declare what the call names.
    #[error("{0}")]
    Undeclared(String),
    /// The inference endpoint failed to answer.
    #[error("the inference endpoint: {0}")]
    Inference(String),
    #[error("{path}: {source}")]
    Io { path: std::path::PathBuf, source: std::io::Error },
}
