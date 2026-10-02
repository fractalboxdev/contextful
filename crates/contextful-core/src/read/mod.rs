//! The read face's pure domain: content tokens and the relevance floor, the ranking
//! legs and their fusion, the embedding port, the response projection and its cell
//! encoding, the statement guard over the engine's own parse, templates, and the
//! handshake's linked-backend identity.

pub mod cache;
pub mod embed;
pub mod error;
pub mod face;
pub mod filter;
pub mod guard;
pub mod pin;
pub mod rank;
pub mod respond;
pub mod template;
pub mod tokens;

pub use error::{ReadError, Refusal};
