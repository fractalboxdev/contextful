//! The bounds of the read face's session pool.

/// Resolved sessions one read face pools, oldest evicted first (`read.cache.pool-entries`).
pub const SESSION_POOL_ENTRIES: usize = 16;

/// Idle connections one pooled session keeps (`read.cache.pool-connections`).
pub const SESSION_POOL_CONNECTIONS: usize = 4;
