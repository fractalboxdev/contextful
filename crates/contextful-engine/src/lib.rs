//! The run path's execution core: the execution handle, the runner, the journal over its store ports and their
//! adapters, the local catalog, awakeables, cancellation, the command source and the live run projection.

pub mod awake;
pub mod cancel;
pub mod catalog;
pub mod command;
pub mod conformance;
pub mod drive;
pub mod execution;
pub mod fsutil;
pub mod guard;
pub mod journal;
pub mod project;
pub mod runner;
pub mod scheduler;
pub mod stop;
pub mod stores;
pub mod worker;

pub use catalog::LocalCatalog;
pub use execution::Execution;
pub use journal::Journal;
pub use runner::{Engine, EngineError, RunSpec};
