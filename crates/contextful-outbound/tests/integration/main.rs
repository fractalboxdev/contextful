//! The runtime's one integration binary, one module per operation.

mod attach;
mod aws_secrets;
mod aws_sqs;
mod egress;
mod file_secrets;
mod infer;
mod lease;
mod meter;
mod oauth;
mod probe;
mod record;
mod resolve;
mod role;
mod rotate;
mod support;
mod telemetry;

/// The process's proxy environment: shared by every test that builds an HTTP client, held
/// exclusively by the one test that configures a proxy (`assurance.test.global-state-lock`).
pub static PROXY_ENV: std::sync::RwLock<()> = std::sync::RwLock::new(());
