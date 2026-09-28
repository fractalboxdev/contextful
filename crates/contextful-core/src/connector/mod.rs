//! The `connector` contract's domain: credential references and their templates, the
//! lease wire, the limiter wire and permit pool, the allowlist, address vetting and origin
//! rules, artifact pins and the forwarded guest table, the data fence around model-bound
//! values, and the provider port.
//! The resolver and the mediated client are `contextful-outbound`.

pub mod attach;
pub mod error;
pub mod infer;
pub mod lease;
pub mod meter;
pub mod package;
pub mod probe;
pub mod reference;
pub mod resolve;

pub use error::ConnectorError;
