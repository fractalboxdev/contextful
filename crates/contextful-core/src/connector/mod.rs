//! The `connector` contract's domain: credential references and their templates, the
//! lease wire, the limiter wire and permit pool, the allowlist, address vetting and origin
//! rules, artifact pins and the forwarded guest table, and the provider port.
//! The resolver and the mediated client are `contextful-runtime`.

pub mod attach;
pub mod error;
pub mod lease;
pub mod meter;
pub mod package;
pub mod probe;
pub mod reference;
pub mod resolve;

pub use error::ConnectorError;
