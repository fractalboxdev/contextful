//! The native sources compiled into the binary (`connector.package.built-in-registry`).

pub mod decode;
pub mod http;

/// The names resolving to compiled-in sources.
pub const BUILT_IN: [&str; 1] = [http::NAME];
