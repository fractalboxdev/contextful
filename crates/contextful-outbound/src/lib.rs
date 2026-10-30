//! Host mediation, credential resolution and shared-quota metering for the run path's
//! outbound requests, and the one inference endpoint.

#[cfg(feature = "aws-secrets-manager")]
pub mod aws_secrets;
#[cfg(feature = "aws-sqs")]
pub mod aws_sqs;
pub mod client;
pub mod egress;
pub mod file_secrets;
pub mod infer;
pub mod limiter;
pub mod oauth;
pub mod probe;
#[cfg(feature = "transport-ureq")]
pub mod role;
pub mod secrets;
#[cfg(feature = "sigv4")]
pub mod sigv4;
pub mod telemetry;
#[cfg(feature = "transport-ureq")]
pub mod ureq_transport;

pub use client::{Client, HeaderValue, Response};
pub use egress::{Intent, Outcome, PreSendHook, Transport};
pub use limiter::{Limiter, Meter};
pub use secrets::{assemble, Resolver};
