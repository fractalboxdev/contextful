//! The derive tier's domain: configuration and binding, the outstanding set, the rows a
//! unit lands as, the cue grammar, and the exec chain's bounds and identity. The driver
//! that runs a chain is `contextful-connectors`. A host task is compiled code the embedding
//! binary registers under a name before build.

pub mod config;
pub mod cues;
pub mod emit;
pub mod engine;
pub mod exec;
pub mod task;
