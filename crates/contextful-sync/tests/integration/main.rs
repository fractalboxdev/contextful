//! The sync package's one integration binary, one module per operation.

mod clean_room;
mod collect;
mod converge;
mod edge;
mod generation;
mod husks;
mod lazy;
mod lease;
mod pull;
mod push;
mod redistribute;
mod run_state;
mod s3;
mod s3_retry;
mod support;
