//! The store adapter: a project's store root on a filesystem, Parquet table parts, run
//! and snapshot manifests, the table pointer, the fold, the scan that resolves a table's
//! file list and relation, and, under the `read` feature, the read face executing over it.

pub mod build;
pub mod catalog;
pub mod commit_log;
pub mod error;
pub mod fold;
pub mod fulltext;
pub mod land;
pub mod ledger;
pub mod node;
pub mod parquet_io;
pub mod project;
#[cfg(feature = "read")]
pub mod read;
pub mod rows;
pub mod scan;
pub mod store;
pub mod vector;

pub use error::{ContextError, Result};
pub use store::Store;
