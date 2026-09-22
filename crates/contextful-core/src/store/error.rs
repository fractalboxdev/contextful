//! The refusals of the `store` contract, one variant per error identifier.
//!
//! A variant's `Display` begins with its identifier, so a surface printing the error
//! names the refusal a caller greps for.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// An `as_of` precedes the oldest retained snapshot of a table whose history was collected. (`store.bound-time.as-of-unretained`)
    #[error("StoreAsOfUnretained: {0}")]
    StoreAsOfUnretained(String),
    /// A declared key source names a binding the process lacks. (`store.encrypt.key-unbound`)
    #[error("StoreEncryptionKeyUnbound: {0}")]
    StoreEncryptionKeyUnbound(String),
    /// An index is declared over a column the reconciled schema lacks. (`store.index.column-absent`)
    #[error("StoreIndexColumnAbsent: {0}")]
    StoreIndexColumnAbsent(String),
    /// An index is declared over a column redacted at write time. (`store.encrypt.redacted-index`)
    #[error("StoreIndexOverRedactedColumn: {0}")]
    StoreIndexOverRedactedColumn(String),
    /// A primary-key column takes the float promotion. (`store.reconcile.key-widening`)
    #[error("StoreKeyWidened: {0}")]
    StoreKeyWidened(String),
    /// A run manifest or reachable snapshot manifest fails to parse. (`store.lay-out.manifest-unreadable`)
    #[error("StoreManifestUnreadable: {0}")]
    StoreManifestUnreadable(String),
    /// A resolved node id is too long or outside the path-safe pattern. (`store.lay-out.node-id-shape`)
    #[error("StoreNodeIdInvalid: {0}")]
    StoreNodeIdInvalid(String),
    /// A node id is declared in a control-plane configuration applied to many machines. (`store.lay-out.node-id-shared`)
    #[error("StoreNodeIdShared: {0}")]
    StoreNodeIdShared(String),
    /// An `order_by` names a column neither declared nor injected. (`store.declare.order-by-unknown`)
    #[error("StoreOrderByUnknownColumn: {0}")]
    StoreOrderByUnknownColumn(String),
    /// A commit would expose snapshot data without its declared sidecars, or the reverse. (`store.fold.partial-snapshot`)
    #[error("StorePartialSnapshot: {0}")]
    StorePartialSnapshot(String),
    /// A producer column is spelled inside the reserved namespace and outside the optional set. (`store.reserve.column-name`)
    #[error("StoreReservedColumnName: {0}")]
    StoreReservedColumnName(String),
    /// A declared table collides with a reserved table namespace. (`store.reserve.table-name`)
    #[error("StoreReservedTableName: {0}")]
    StoreReservedTableName(String),
    /// Two observed types for one column have no supertype in the lattice. (`store.reconcile.incompatible`)
    #[error("StoreSchemaIncompatible: {0}")]
    StoreSchemaIncompatible(String),
    /// A table name resolves to no `schema.json` in the tree. (`store.lay-out.unknown-table`)
    #[error("StoreUnknownTable: {0}")]
    StoreUnknownTable(String),
    /// A declared valid-time column is not timestamp-typed. (`store.bound-time.valid-time-type`)
    #[error("StoreValidTimeNotTimestamp: {0}")]
    StoreValidTimeNotTimestamp(String),
    /// A valid-time-bounded read names a table declaring no pair. (`store.bound-time.valid-time-undeclared`)
    #[error("StoreValidTimeUndeclared: {0}")]
    StoreValidTimeUndeclared(String),
}
