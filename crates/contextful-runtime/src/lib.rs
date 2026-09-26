//! Host mediation and credential resolution for the run path's outbound requests, and
//! the one inference endpoint.

pub mod client;
pub mod infer;
pub mod secrets;

pub use client::{Client, HeaderValue, Response};
pub use secrets::{assemble, Resolver};
