//! The policy crate's one integration binary, one module per operation.

mod attenuate;
mod decide;
mod audit;
mod enforce;
#[cfg(feature = "exchange")]
mod exchange;
mod issue;
mod keyset;
mod possession;
mod profile;
mod revoke;
mod support;
mod verify;
