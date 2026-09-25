//! Host mediation and credential resolution for the run path's outbound requests.

pub mod client;
pub mod secrets;

pub use client::{Client, HeaderValue, Response};
pub use secrets::{assemble, Resolver};
