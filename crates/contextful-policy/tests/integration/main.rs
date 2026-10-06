//! The policy crate's one integration binary, one module per operation.

mod attenuate;
mod decide;
mod audit;
mod control_receipt;
mod enforce;
#[cfg(feature = "exchange")]
mod exchange;
mod explain;
mod issue;
mod keyset;
mod possession;
mod profile;
mod replica;
mod revoke;
mod support;
mod verify;
