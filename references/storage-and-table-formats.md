# Storage and table formats

Parquet parts under a manifest, snapshot commit on an object store, two clocks per
row, and a bucket that doubles as the sync medium. The lease and fencing sources
that govern who may commit live in [leases-and-coordination.md](./leases-and-coordination.md).

## Tables on object storage

### Delta Lake

Armbrust, M., et al. "Delta Lake: High-Performance ACID Table Storage over Cloud
Object Stores." *PVLDB* 13(12):3411–3424, 2020.
<https://people.eecs.berkeley.edu/~matei/papers/2020/vldb_delta_lake.pdf>

- **Priority:** must-read
- **Informs:** `store.fold`, `store.lay-out`, `store.push`, `store.merge`, `store.probe`, `run.advance`
- **Question:** How does a table commit on a store with no atomic rename, travel in
  time by snapshot selection rather than by row timestamp, and record the source
  offset (`txn(appId, version)`) inside the same log commit so a crash between
  snapshot and cursor never re-lands a batch? The paper is the same architecture as
  `store` — Parquet parts, a log compacted into checkpoints, optimistic commit by
  put-if-absent — and bears on three open questions: whether the catalog is a
  rebuildable cache or the commit point (a per-table pointer object committed by
  conditional PUT answers both), whether one `as_of` returns the same rows across a
  fold, and where the cursor commits. Its checkpoint and retention rules are the
  published precedent for the fold's run-retention window, and its log-store
  discussion for S3's historical lack of put-if-absent is the argument behind
  `store.probe`.

### Analyzing and Comparing Lakehouse Storage Systems

Jain, P., Kraft, P., Power, C., Das, T., Stoica, I., Zaharia, M. "Analyzing and
Comparing Lakehouse Storage Systems." *CIDR*, 2023.
<https://www.cidrdb.org/cidr2023/papers/p92-jain.pdf>

- **Priority:** should-read
- **Informs:** `store.fold`, `store.merge`
- **Question:** Merge-on-read or copy-on-write for run files read on top of a
  snapshot, and what concurrency control each of Delta, Hudi and Iceberg uses for
  it. LHBench is a ready fold-cost baseline.

### Exploiting Cloud Object Storage for High-Performance Analytics

Durner, D., Leis, V., Neumann, T. "Exploiting Cloud Object Storage for
High-Performance Analytics." *PVLDB* 16(11):2769–2782, 2023.
<https://www.vldb.org/pvldb/vol16/p2769-durner.pdf>

- **Priority:** optional
- **Informs:** `read.cache`, `store.replicate`
- **Question:** What request size and concurrency saturate object-store bandwidth
  per dollar? The measured models turn the cache locality claims and the replica's
  whole-Parquet download rule into numbers.

### Small Materialized Aggregates

Moerkotte, G. "Small Materialized Aggregates: A Light Weight Index Structure for
Data Warehousing." *VLDB*, pp. 476–487, 1998.
<https://www.vldb.org/conf/1998/p476.pdf>

- **Priority:** optional
- **Informs:** `store.index`
- **Question:** Under which sort order do zone maps prune, and how stale may they run
  before a rebuild pays? The origin of zone maps, and the reason clustering by
  `cluster_by` makes them effective.

### S3 conditional writes

Amazon Web Services. "Conditional writes." *Amazon S3 User Guide*.
<https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html>

- **Priority:** should-read
- **Informs:** `store.probe`, `store.push`, `store.lease`
- **Question:** Which preconditions does the commit rest on? `If-None-Match: *`
  creates only when absent and `If-Match: <etag>` replaces only the version read.
  The probe exercises exactly these two, and reads back the result because some
  S3-compatible backends answer success while ignoring the precondition.

## Time

### Temporal Data Management

Jensen, C. S., Snodgrass, R. T. "Temporal Data Management." *IEEE TKDE*
11(1):36–44, 1999. <https://www2.cs.arizona.edu/~rts/pubs/TKDEJan99.pdf>

- **Priority:** must-read
- **Informs:** `store.bound-time`, `read.revise`, `read.recall`, `read.settle`
- **Question:** Does folding discard transaction-time history, and what type must an
  instant be? A bitemporal store keeps transaction time append-only, so a fold that
  drops it is a departure to record, not a detail. Instant comparison is correct only
  under one normalized offset and precision: RFC 3339 text does not sort lexically
  (`…00.5Z` sorts before `…00Z`, and `+01:00` reorders), which argues for a native
  UTC timestamp column over text.

## Replication and local-first

### Local-first software

Kleppmann, M., Wiggins, A., van Hardenberg, P., McGranaghan, M. "Local-first
software: you own your data, in spite of the cloud." *Onward!*, 2019.
<https://dl.acm.org/doi/10.1145/3359591.3359737>

- **Priority:** should-read
- **Informs:** `topology.compose`, `store.pull`, `store.replicate`, `surface.edit`
- **Question:** Which of the seven local-first ideals does the engine-on-the-machine,
  bucket-as-sync-medium topology keep, and where does it depart — single-writer
  compaction ownership, a hosted control plane — as a recorded choice?

### A Conflict-Free Replicated JSON Datatype

Kleppmann, M., Beresford, A. R. "A Conflict-Free Replicated JSON Datatype."
*IEEE TPDS* 28(10):2733–2746, 2017.
<https://www.cl.cam.ac.uk/~arb33/papers/KleppmannBeresford-CRDT-JSON-TPDS2017.pdf>

- **Priority:** optional
- **Informs:** `surface.edit`, `surface.apply`
- **Question:** What convergence does the edit-time configuration document guarantee
  before `surface.apply` materializes it? A merge converges yet can still produce a
  document that fails validation, so apply validates the merged result, never the
  inputs.
