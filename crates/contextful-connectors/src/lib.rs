//! The native sources compiled into the binary (`connector.package.built-in-registry`).

/// The shared record decoders (`connector.source.body-format`).
pub use contextful_decode as decode;
pub mod derive;
pub mod http;

/// The names resolving to compiled-in sources.
pub const BUILT_IN: [&str; 2] = [http::NAME, derive::NAME];
