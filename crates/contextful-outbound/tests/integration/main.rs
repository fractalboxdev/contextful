//! The runtime's one integration binary, one module per operation.

mod attach;
mod egress;
mod infer;
mod lease;
mod meter;
mod probe;
mod resolve;
mod support;

/// The process's proxy environment: shared by every test that builds an HTTP client, held
/// exclusively by the one test that configures a proxy (`assurance.test.global-state-lock`).
pub static PROXY_ENV: std::sync::RwLock<()> = std::sync::RwLock::new(());
