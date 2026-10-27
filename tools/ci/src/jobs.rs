//! The footprint build's parallel job count (`assurance.gate.parallel-jobs`), apart from
//! how the build's memory and processors are read.

/// Memory one build job may take: the SQL engine's C++ units peak near it at `-O3`.
pub const JOB_MEMORY: u64 = 3 << 30;

/// Parallel build jobs: `memory` divided by [`JOB_MEMORY`], at least one and at most `cpus`;
/// one when the memory is unread.
// mirrors: assurance.gate.parallel-jobs
pub fn parallel(memory: Option<u64>, cpus: u64) -> u64 {
    memory.map_or(1, |m| m / JOB_MEMORY).clamp(1, cpus.max(1))
}
