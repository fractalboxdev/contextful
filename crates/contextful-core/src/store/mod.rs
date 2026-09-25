//! The `store` contract's pure domain: names and shapes of the tree, declarations, the
//! reserved namespaces, the type lattice, the time bounds and the relation a table
//! registers as. The Parquet and filesystem adapter is `contextful-context`.

pub mod bound_time;
pub mod commit_log;
pub mod declare;
pub mod error;
pub mod fold;
pub mod lease;
pub mod lay_out;
pub mod object;
pub mod reconcile;
pub mod relation;
pub mod reserve;
pub mod resolve;
pub mod sync;

pub use error::StoreError;
