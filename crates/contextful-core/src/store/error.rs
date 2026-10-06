//! The refusals of the `store` contract, one variant per error identifier.
//!
//! A variant's `Display` begins with its identifier, so a surface printing the error
//! names the refusal a caller greps for.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// A row-retention column is absent or not declared Timestamp (`store.declare.retain-rows`).
    #[error("StoreRetentionColumnInvalid: {0}")]
    StoreRetentionColumnInvalid(String),
    /// An `as_of` precedes the oldest retained snapshot of a table whose history was collected. (`store.bound-time.as-of-unretained`)
    #[error("StoreAsOfUnretained: {0}")]
    StoreAsOfUnretained(String),
    /// A declared key source names a binding the process lacks. (`store.encrypt.key-unbound`)
    #[error("StoreEncryptionKeyUnbound: {0}")]
    StoreEncryptionKeyUnbound(String),
    /// An index is declared over a column the reconciled schema lacks. (`store.index.column-absent`)
    #[error("StoreIndexColumnAbsent: {0}")]
    StoreIndexColumnAbsent(String),
    /// An indexed column or `id_column` carries a type the sidecar does not read. (`store.index.column-type`)
    #[error("StoreIndexColumnType: {0}")]
    StoreIndexColumnType(String),
    /// A table's sidecars resolve no single `id_column`. (`store.index.id-column-unresolved`)
    #[error("StoreIndexIdColumnUnresolved: {0}")]
    StoreIndexIdColumnUnresolved(String),
    /// One `id_column` value names two rows of a staged snapshot. (`store.index.id-unique`)
    #[error("StoreIndexIdNotUnique: {0}")]
    StoreIndexIdNotUnique(String),
    /// An index is declared over a column redacted at write time. (`store.encrypt.redacted-index`)
    #[error("StoreIndexOverRedactedColumn: {0}")]
    StoreIndexOverRedactedColumn(String),
    /// Two sidecar declarations resolve to one path. (`store.index.path-collision`)
    #[error("StoreIndexPathCollision: {0}")]
    StoreIndexPathCollision(String),
    /// A `primary_key` names a column neither declared nor injected. (`store.declare.key-unknown`)
    #[error("StoreKeyUnknownColumn: {0}")]
    StoreKeyUnknownColumn(String),
    /// A struct, list or map column names a key, ordering, clustering, partition or
    /// valid-time role. (`store.declare.nested-key`)
    #[error("StoreNestedKeyColumn: {0}")]
    StoreNestedKeyColumn(String),
    /// A primary-key column takes the float promotion. (`store.reconcile.key-widening`)
    #[error("StoreKeyWidened: {0}")]
    StoreKeyWidened(String),
    /// A commit, pointer replace or catalog update losing its condition to a higher fence. (`store.lease.stale-fence`)
    #[error("LeaseFenced: {0}")]
    LeaseFenced(String),
    /// A run manifest or reachable snapshot manifest fails to parse. (`store.lay-out.manifest-unreadable`)
    #[error("StoreManifestUnreadable: {0}")]
    StoreManifestUnreadable(String),
    /// A resolved node id is too long or outside the path-safe pattern. (`store.lay-out.node-id-shape`)
    #[error("StoreNodeIdInvalid: {0}")]
    StoreNodeIdInvalid(String),
    /// A node id is declared in a control-plane configuration applied to many machines. (`store.lay-out.node-id-shared`)
    #[error("StoreNodeIdShared: {0}")]
    StoreNodeIdShared(String),
    /// An init meets a `contextful.toml` declaring another project, or a project with no name. (`store.init.name-conflict`)
    #[error("StoreProjectConflict: {0}")]
    StoreProjectConflict(String),
    /// A project name is not path-safe `/`-separated segments. (`store.init.name-shape`)
    #[error("StoreProjectNameInvalid: {0}")]
    StoreProjectNameInvalid(String),
    /// A command given no `--project` finds no `contextful.toml` naming a project from its working directory up. (`store.init.undiscovered`)
    #[error("StoreProjectUndiscovered: {0}")]
    StoreProjectUndiscovered(String),
    /// An `order_by` names a column neither declared nor injected. (`store.declare.order-by-unknown`)
    #[error("StoreOrderByUnknownColumn: {0}")]
    StoreOrderByUnknownColumn(String),
    /// A `partition_by` column is typed binary or vector. (`store.index.partition-type`)
    #[error("StorePartitionColumnType: {0}")]
    StorePartitionColumnType(String),
    /// A commit would expose snapshot data without its declared sidecars, or the reverse. (`store.fold.partial-snapshot`)
    #[error("StorePartialSnapshot: {0}")]
    StorePartialSnapshot(String),
    /// A producer column is spelled inside the reserved namespace and outside the optional set. (`store.reserve.column-name`)
    #[error("StoreReservedColumnName: {0}")]
    StoreReservedColumnName(String),
    /// A declared table collides with a reserved table namespace. (`store.reserve.table-name`)
    #[error("StoreReservedTableName: {0}")]
    StoreReservedTableName(String),
    /// A run id already committed on its node lands again, as other than its replay. (`store.lay-out.run-conflict`)
    #[error("StoreRunConflict: {0}")]
    StoreRunConflict(String),
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
    /// A cursor whose kind takes no lease reached `cursors/`. (`store.lease.cursor-kind`)
    #[error("LeaseCursorKindMismatch: {0}")]
    LeaseCursorKindMismatch(String),
    /// A run or pass found an unexpired lease another node holds. (`store.lease.held`)
    #[error("LeaseHeld: {0}")]
    LeaseHeld(String),
    /// A bucket lease was attempted under the reserved node id `local`. (`store.lease.local-node`)
    #[error("LeaseNodeIdLocal: {0}")]
    LeaseNodeIdLocal(String),
    /// A release was attempted against a lease another node holds. (`store.lease.not-held`)
    #[error("LeaseNotHeld: {0}")]
    LeaseNotHeld(String),
    /// A query needs a sidecar or partition the replica lacks. (`store.replicate.missing-index`)
    #[error("ReplicaMissingIndex: {0}")]
    ReplicaMissingIndex(String),
    /// A replica holds a strict subset of a snapshot's data files. (`store.replicate.partial-parquet`)
    #[error("ReplicaPartialParquet: {0}")]
    ReplicaPartialParquet(String),
    /// A refresh requested a replicate-off table. (`store.replicate.sensitive-refused`)
    #[error("ReplicaSensitiveTable: {0}")]
    ReplicaSensitiveTable(String),
    /// A write verb was invoked against a replica. (`store.replicate.write-refused`)
    #[error("ReplicaWriteRefused: {0}")]
    ReplicaWriteRefused(String),
    /// `cas` is declared against a backend the probe did not demonstrate. (`store.probe.unproven`)
    #[error("SyncCoordinationUnproven: {0}")]
    SyncCoordinationUnproven(String),
    /// An S3 or R2 endpoint's `[sync]` binds a credential key to no reference, or to an unset variable. (`store.endpoint.credential-unbound`)
    #[error("SyncCredentialUnbound: {0}")]
    SyncCredentialUnbound(String),
    /// A cursor was resolved by recency instead of through its commit. (`store.merge.cursor-recency`)
    #[error("SyncCursorConflict: {0}")]
    SyncCursorConflict(String),
    /// A plain `http://` endpoint names a host off loopback. (`store.endpoint.plaintext`)
    #[error("SyncEndpointInsecure: {0}")]
    SyncEndpointInsecure(String),
    /// An endpoint's scheme names no bucket adapter this build links. (`store.endpoint.unsupported-scheme`)
    #[error("SyncEndpointUnsupported: {0}")]
    SyncEndpointUnsupported(String),
    /// A generation manifest already holds another commit's bytes under the number a push committed. (`store.push.generation-conflict`)
    #[error("SyncGenerationConflict: {0}")]
    SyncGenerationConflict(String),
    /// A pull names a generation the bucket holds no generation manifest for. (`store.pull.generation-absent`)
    #[error("SyncGenerationAbsent: {0}")]
    SyncGenerationAbsent(String),
    /// A generation pull meets a local file the generation does not list. (`store.pull.generation-diverged`)
    #[error("SyncGenerationDiverged: {0}")]
    SyncGenerationDiverged(String),
    /// A bucket or generation manifest carries a format newer than the reader parses. (`store.push.format-unsupported`)
    #[error("SyncManifestFormatUnsupported: {0}")]
    SyncManifestFormatUnsupported(String),
    /// The bucket manifest commit lost its race past the retry bound. (`store.merge.exhausted`)
    #[error("SyncManifestRebaseExhausted: {0}")]
    SyncManifestRebaseExhausted(String),
    /// A downloaded object's digest differs from its entry. (`store.pull.digest-mismatch`)
    #[error("SyncObjectDigestMismatch: {0}")]
    SyncObjectDigestMismatch(String),
    /// A key resolved outside the configured prefix. (`store.push.prefix-escape`)
    #[error("SyncPrefixEscape: {0}")]
    SyncPrefixEscape(String),
    /// `prefix` and `prefix_from` are declared together. (`store.push.prefix-overspecified`)
    #[error("SyncPrefixOverspecified: {0}")]
    SyncPrefixOverspecified(String),
    /// The variable named by `prefix_from` is unset at startup. (`store.push.prefix-unbound`)
    #[error("SyncPrefixUnbound: {0}")]
    SyncPrefixUnbound(String),
    /// The conditional-write probe met an unsupported method, a forbidden response or a transport error. (`store.probe.inconclusive`)
    #[error("SyncProbeInconclusive: {0}")]
    SyncProbeInconclusive(String),
    /// A pull exhausted its retries against keys moving beneath it. (`store.pull.unconverged`)
    #[error("SyncPullDidNotConverge: {0}")]
    SyncPullDidNotConverge(String),
    /// A second push of one store started on a machine already pushing it. (`store.push.in-flight`)
    #[error("SyncPushInFlight: {0}")]
    SyncPushInFlight(String),
    /// A tombstone names an entry another node owns. (`store.merge.tombstone-owner`)
    #[error("SyncTombstoneForeign: {0}")]
    SyncTombstoneForeign(String),
}
