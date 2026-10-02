//! The binary's one integration binary, one module per command group. A command group of
//! the data plane runs on a build linking it; the rest run on every profile.

#[cfg(feature = "data-plane")]
mod author;
#[cfg(feature = "data-plane")]
mod component;
#[cfg(feature = "data-plane")]
mod component_absent;
#[cfg(feature = "data-plane")]
mod build;
#[cfg(feature = "data-plane")]
mod context;
#[cfg(feature = "data-plane")]
mod derive;
mod differential;
#[cfg(feature = "data-plane")]
mod drive;
#[cfg(feature = "data-plane")]
mod drive_absent;
#[cfg(feature = "data-plane")]
mod eval;
#[cfg(feature = "data-plane")]
mod export;
mod formal;
#[cfg(feature = "data-plane")]
mod init;
#[cfg(feature = "data-plane")]
mod job;
#[cfg(feature = "data-plane")]
mod mcp;
#[cfg(feature = "data-plane")]
mod memory;
#[cfg(feature = "data-plane")]
mod object;
#[cfg(feature = "data-plane")]
mod object_absent;
#[cfg(feature = "data-plane")]
mod pipeline;
mod profile;
#[cfg(feature = "data-plane")]
mod query;
#[cfg(feature = "data-plane")]
mod run;
#[cfg(feature = "data-plane")]
mod serve;
#[cfg(feature = "data-plane")]
mod sync;
mod token;
