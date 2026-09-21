# A-store — The store and its sync decisions

**Status:** accepted

## The schema lattice has one promotion, and a key never widens

`store.reconcile` models one promotion, `Int64` with `Float64` to `Float64`, the only pairing that produces physically mixed Parquet; the scan's own type resolution widens it, with no per-column cast. Any other pair refuses at the write, naming the column, the stored type and the arriving one. A primary-key column takes no float promotion: reconciliation refuses the widening batch before any Parquet is written, `store.fold` refuses a key already reconciled to `Float64`, and a key value beyond the exact-integer range of `Float64` is emitted as a string.

| Option | Lost on | Cost |
| --- | --- | --- |
| One promotion; keys excluded; refuse at the write *(chosen)* | — | A source flipping between text and number fails its batch until the declaration or the source is fixed. |
| Widen every clash to text | Read-time recoverability | Every query casts; a numeric aggregate becomes a string comparison. |
| A variant column holding both physical types | Read-time recoverability | The projection differs per row, and no single Arrow type describes the column. |
| Fail the read instead of the write | Who receives the error | The rows have landed, and the refusal reaches a reader who cannot fix the source. |
| Key widening as an operator obligation | Irreversibility | One out-of-range value silently merges distinct keys under the fold. |

Consequences: every accepted column has one logical type across its files, and a keyed table's identity survives integers past the float range.
Revisit: a second pairing that some source emits routinely and the scan resolves losslessly.

## Coordination is probed compare-and-swap with fences the storage enforces

`topology.coordinate` rests on one linearizable conditional write behind the `Catalog` port; a backend without it refuses at open with `ConditionalWriteUnsupported`. `store.probe` tests atomic conditional writes with a live sentinel inside the configured prefix; an inconclusive probe raises `SyncProbeInconclusive`, and a declared `cas` push stops with `SyncCoordinationUnproven`. `store.lease` acquisition increments a fence the storage enforces on the cursor commit (`cursors/<pipeline_id>/<seq>.json`), the `_pointer.json` replace and the catalog update; losing to a higher fence raises `LeaseFenced`, a held lease raises `LeaseHeld` and retries next tick, and release nulls `holder` without deleting the object. `store.fold` compacts only under the table's compaction lease.

| Option | Lost on | Cost |
| --- | --- | --- |
| Storage-enforced fences; per-table compaction lease *(chosen)* | — | One conditional write per commit, and a never-deleted lease object per pipeline and table. |
| Compare the fence in the writer before commit | Pause safety | A holder pausing between check and write commits after its successor. |
| A compaction owner named in configuration | Divergent configuration | Two machines naming themselves both compact, unfenced. |
| Wall-clock expiry checked at commit | Clock trust | The paused machine has the least trustworthy clock. |
| An embedded consensus cluster | Operational cost | A second system to run for a low request rate. |

Consequences: a backend without demonstrated conditional writes runs single-writer by refusal, never by silent degradation.
Revisit: a backend whose access policy enforces a generation bound for every writer.

## A replica is a read-only whole-snapshot cache

`store.replicate` answers reads and holds no write path; a write verb refuses and names the canonical store. A replica holds every Parquet file of a snapshot or none, and a query needing an absent sidecar index or partition refuses, naming the refresh that supplies it; a replica never answers from what is present and never fetches mid-query. A table declaring sensitive columns replicates off by default, and a consumer reads them through the proxying face, which verifies the credential and records each read. `store.pull` writes a table's pointer after every object it reaches has landed and folds records into the rebuildable `derived.sqlite`; cursor and lease rows live in `machine.sqlite`, which no pull or rebuild touches.

| Option | Lost on | Cost |
| --- | --- | --- |
| Read-only, whole-Parquet, sensitive tables off *(chosen)* | — | A replica stores full snapshots; a sensitive read needs a round trip to the proxying face. |
| Accept local writes and push them on refresh | Merge soundness | Divergent writes need conflict resolution the store lacks. |
| Hold a partition subset and check predicates at query time | Silent wrongness | A missed partition returns a short answer. |
| Replicate sensitive tables and mask at the consumer | Failure direction | A misconfiguration has already moved the bytes onto the machine. |
| One catalog file for synced and machine-local rows | Local state | A rebuild or pull erases cursor and lease rows that exist nowhere else. |

Consequences: a replica's answer is complete for the snapshot it names, or a refusal.

## The bucket index commits by compare-and-set over a scoped union of the remote

`store.merge` commits the bucket index, a push's commit point, by compare-and-set over a scoped union: the remote contributes an entry only where this writer has never written, so an owned entry absent locally drops out and retention and collection reach a consumer. Ownership is readable from the key: run directories carry a node segment, request-ledger files carry the node in the filename. A lost race re-reads, re-merges and re-commits within the retry bound; exhaustion raises `SyncManifestRebaseExhausted`, reports every object already uploaded, and asks for a re-run.

| Option | Lost on | Cost |
| --- | --- | --- |
| Compare-and-set over a scoped union, bounded rebase *(chosen)* | — | Every writer-owned tier needs an ownership arm; a key shape falling to the unowned default never propagates deletion. |
| A blanket union of both indexes | Deletion propagation | The index grows forever against a shrinking tree. |
| Replace the remote index wholesale | Visibility of committed objects | A race loser unlists the winner's objects. |
| Replicated conflict-free types on the data plane | Necessity | The append-only layout already prevents most conflicts, and the machinery costs on every push. |
| Unbounded rebase retries | Termination | A writer losing every race holds its process indefinitely. |

Consequences: a contended bucket can leave a writer's objects durable and unlisted, visible in the refusal.
Revisit: a key shape whose ownership is not derivable from the key; retries exhausting under ordinary concurrency; the index outgrowing a single-object commit.
