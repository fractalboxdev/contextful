---
contract: store
owns:
  - replicate
---

# Replicas

A replica is a read-only store that downloads whole snapshots from another store's bucket.
Operators use this contract to refresh a replica and identify the snapshots available to a local query server.
The replica reports its local contents and writes nothing back to the source store.

## replicate

Replica refreshes, required snapshot files, local snapshot records and refused reads or writes.

- `write-refused` — A write verb against a replica raises `ReplicaWriteRefused`, naming the canonical store.
  *A-store*
- `partial-parquet` — If a replica lacks any of a snapshot's Parquet files, refresh raises `ReplicaPartialParquet` and leaves that snapshot unpublished.
  *A-store*
- `descriptor` — A replica's pull writes `replica.json` beside `derived.sqlite`. For each table, the file names every locally held snapshot whose parts and sidecar paths exist on disk. Push excludes this file.
  *A-store*
- `missing-index` — If a query requires a sidecar or partition absent from the replica, the query raises `ReplicaMissingIndex`, naming the refresh that supplies it.
  *A-store*
- `sensitive-refused` — A refresh requesting a replicate-off table raises `ReplicaSensitiveTable`. The consumer reads that table through the query proxy.
  *A-store*
