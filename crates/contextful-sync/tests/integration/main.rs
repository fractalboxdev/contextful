//! The sync package's one integration binary, one module per operation.

mod converge;
mod pull;
mod push;
#[cfg(feature = "s3-sync")]
mod s3;
mod support;
