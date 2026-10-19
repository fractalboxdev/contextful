---
contract: store
owns:
  - replicate
---

# Replicas

A replica is a read-only store that refreshes whole snapshots from another store's bucket
and advertises what it holds, so a read face serves near its readers without writing back.

## replicate

A consuming replica of the bucket: the refresh diff, what it holds whole, what it declines, and what it rebuilds.

- `write-refused` — A write verb against a replica raises `ReplicaWriteRefused`, naming the canonical store.
  *A-store*
- `partial-parquet` — A replica holding a strict subset of a snapshot's Parquet raises `ReplicaPartialParquet` at refresh and leaves that snapshot unpublished.
  *A-store*
- `descriptor` — A replica's pull writes `replica.json` beside `derived.sqlite`, naming per table each snapshot it holds with that snapshot's parts and sidecar paths present on disk; no push carries it.
  *A-store*
- `missing-index` — A query needing a sidecar or a partition the replica lacks raises `ReplicaMissingIndex`, naming the refresh that supplies it.
  *A-store*
- `sensitive-refused` — A refresh requesting a replicate-off table raises `ReplicaSensitiveTable`; the consumer reads through the proxying face.
  *A-store*
