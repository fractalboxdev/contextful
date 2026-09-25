//! The `connector` contract's domain: credential references and their templates, the
//! lease wire, the allowlist, address vetting and origin rules, and the provider port.
//! The resolver and the mediated client are `contextful-runtime`.

pub mod attach;
pub mod error;
pub mod lease;
pub mod reference;
pub mod resolve;

pub use error::ConnectorError;
