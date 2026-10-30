//! The binary's one integration binary, one module per command group. A command group of
//! the data plane runs on a build linking it; the rest run on every profile.

#[cfg(feature = "data-plane")]
mod audit;
#[cfg(feature = "data-plane")]
mod author;
#[cfg(feature = "data-plane")]
mod component;
#[cfg(feature = "data-plane")]
mod component_absent;
#[cfg(feature = "data-plane")]
mod build;
mod catalog;
mod catalog_absent;
#[cfg(feature = "data-plane")]
mod context;
#[cfg(feature = "data-plane")]
mod erase;
#[cfg(feature = "data-plane")]
mod egress;
#[cfg(feature = "data-plane")]
mod disclosure;
#[cfg(feature = "data-plane")]
mod derive;
mod differential;
#[cfg(feature = "data-plane")]
mod protocol_differential;
#[cfg(feature = "data-plane")]
mod journal_differential;
mod merge_differential;
#[cfg(feature = "data-plane")]
mod document;
#[cfg(feature = "data-plane")]
mod empty_replace;
#[cfg(feature = "data-plane")]
mod drive;
#[cfg(feature = "data-plane")]
mod drive_absent;
#[cfg(feature = "data-plane")]
mod eval;
#[cfg(feature = "data-plane")]
mod entity_eval;
#[cfg(feature = "data-plane")]
mod explain;
#[cfg(feature = "data-plane")]
mod export;
#[cfg(feature = "iceberg-export")]
mod export_iceberg;
#[cfg(feature = "data-plane")]
mod export_iceberg_absent;
mod formal;
#[cfg(feature = "data-plane")]
mod holder;
#[cfg(feature = "data-plane")]
mod init;
#[cfg(feature = "data-plane")]
mod job;
mod delegate;
#[cfg(feature = "data-plane")]
mod maintenance;
#[cfg(feature = "data-plane")]
mod mcp;
#[cfg(feature = "data-plane")]
mod memory;
#[cfg(feature = "data-plane")]
mod object;
#[cfg(feature = "data-plane")]
mod object_absent;
#[cfg(feature = "data-plane")]
mod oauth;
#[cfg(feature = "data-plane")]
mod pin;
#[cfg(feature = "data-plane")]
mod pipeline;
mod profile;
#[cfg(feature = "data-plane")]
mod query;
#[cfg(feature = "data-plane")]
mod receive;
#[cfg(feature = "data-plane")]
mod receive_queued;
#[cfg(feature = "aws-sqs")]
mod receive_sqs;
#[cfg(all(feature = "data-plane", not(feature = "aws-sqs")))]
mod receive_sqs_absent;
#[cfg(feature = "data-plane")]
mod report;
#[cfg(feature = "data-plane")]
mod run;
#[cfg(feature = "data-plane")]
mod secrets;
#[cfg(feature = "data-plane")]
mod seed;
#[cfg(feature = "data-plane")]
mod serve;
#[cfg(feature = "data-plane")]
mod sync;
mod token;
#[cfg(feature = "data-plane")]
mod trace;
#[cfg(feature = "data-plane")]
mod visibility;
mod storage;
#[cfg(feature = "data-plane")]
mod worker;

/// A nested `cargo` for building an example from inside a test. Package metadata belongs to
/// this integration executable, not the nested build; build scripts track these variables, so
/// inheriting them invalidates native artifacts and the nested build recompiles the graph.
pub(crate) fn nested_cargo() -> std::process::Command {
    let mut cargo = std::process::Command::new(env!("CARGO"));
    for (name, _) in std::env::vars_os() {
        if name.to_str().is_some_and(|name| {
            name.starts_with("CARGO_PKG_") || name.starts_with("CARGO_CFG_") || matches!(name, "CARGO_MANIFEST_DIR" | "CARGO_MANIFEST_PATH" | "CARGO_MANIFEST_LINKS" | "OUT_DIR")
        }) {
            cargo.env_remove(name);
        }
    }
    cargo
}
