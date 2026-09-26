//! Pure domain types and the ports adapters implement.

pub mod attenuate;
pub mod claims;
pub mod connector;
pub mod coordinate;
pub mod disclosure;
pub mod enforce;
pub mod error;
pub mod exchange;
pub mod grant;
pub mod identify;
pub mod memory;
pub mod issue;
pub mod pipeline;
pub mod ports;
pub mod read;
pub mod revoke;
pub mod run;
pub mod store;
pub mod time;
pub mod topology;

pub use error::AuthorityError;
