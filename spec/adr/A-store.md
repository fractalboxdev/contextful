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

## A sidecar is memory-mapped only over plaintext

**Status:** accepted

Context: `store.encrypt.cipher` seals every sidecar with AES-256-GCM per file, and a sealed file cannot be memory-mapped, while the HNSW and Tantivy readers scale past memory only by mapping plaintext. Criteria: the at-rest scope holds on local disk and in the bucket; no bespoke cipher format; an unencrypted project reads a sidecar larger than memory.

Decision: in an unencrypted project a sidecar is plaintext and memory-mapped read-only. In an encrypted project the files stay sealed; a reader opens one into anonymous process memory, writes no cleartext to disk, and the resident bound applies to the decrypted bytes.

| Option | Lost on | Cost |
| --- | --- | --- |
| Mmap over plaintext; decrypt into anonymous memory when encrypted *(chosen)* | — | An encrypted project holds each open sidecar whole in memory, so its size cap stays binding. |
| A chunked AEAD format decrypted per page fault | Bespoke cipher format | A custom reader per builder and a cryptographic format of our own to audit. |
| Decrypt to a private on-disk cache | At-rest scope | A stolen disk yields the cleartext graph the cipher exists to withhold. |
| Plaintext sidecars in encrypted projects | At-rest scope | A stolen bucket credential admits nearest-neighbour search over the embedding space. |

Consequences: the memory-mapped path and its relaxed bound serve unencrypted projects alone, and an encrypted project's search scale is set by resident memory.
Revisit: a builder that reads through a caller-supplied page source, making per-page decryption a reader rather than a format.

## Every sidecar names one id column

**Status:** accepted

Context: `store.index.vector-by-fold` builds only under a single-column primary key, so an unkeyed append table (`store.declare.unkeyed-union`) or a composite key gets no sidecar. Criteria: keyed, composite-key and unkeyed tables alike; ids stable across folds; one declaration every kind shares.

Decision: a sidecar declaration names one `id_column`, unique within each snapshot, defaulting to a single-column primary key and required otherwise. Vector and full-text sidecars share it, their candidate ids are its values, and `store.index.candidate-ids` re-joins on it. A fold meeting a repeated value refuses the pass.

| Option | Lost on | Cost |
| --- | --- | --- |
| A declared unique `id_column` shared by every kind *(chosen)* | — | An unkeyed producer emits a unique row id, such as a content digest, and a repeat fails its pass. |
| An encoded composite primary key | Unkeyed coverage | An append table has no key to encode. |
| File and row ordinal | Stability across folds | Every fold renumbers rows, so no sidecar extends incrementally. |
| An id declaration per sidecar kind | One declaration | Two arms fuse candidates only through a common id, and two declarations drift. |

Consequences: an unkeyed embedding table gains a sidecar at the cost of one unique column the producer owns.

## Binary and fixed-size vector columns take no promotion

**Status:** accepted

Context: the lattice holds scalar types alone, so a digest lands as text and an embedding as a JSON array, at several times the float16 size and a parse per vector read. Criteria: storage width; a type the engine and a vector builder read without per-row validation; no masked output leaking structure.

Decision: the lattice adds `Binary`, `FixedSizeBinary(n)` and `FixedSizeList` of `Float32` or `Float16` at a fixed dimension, each with no promotion; a width, item or dimension change is `store.reconcile.incompatible`. The JSON row path renders binary as padded base64 and a vector as a number array. A binary column masks by `drop` or `hash` over its bytes, a vector column by `drop` alone. The engine reads `Float16` as `FLOAT` and a fixed list as `ARRAY`; storage keeps half width.

| Option | Lost on | Cost |
| --- | --- | --- |
| Typed binary and vector columns; widen half floats on read *(chosen)* | — | A read materializes float16 vectors at twice their stored width, and a JSON consumer decodes base64. |
| Hex text and JSON arrays in `Utf8` | Storage width | Five to eight times the float16 bytes, and a parse per vector read. |
| A variable-length float list | Validation-free read | Every row checks its dimension before a builder accepts it. |
| Half floats kept through the query face | Engine support | The engine has no half-precision type. |

Consequences: `read.embed.model-identifier` gains a typed vector column, and an Arrow batch lands without a JSON round trip.

## The store's catalogs sit behind a port with SQLite in its own package

**Status:** accepted

Context: `store.lay-out.components` puts two SQLite catalogs in every store, and Cargo admits one `links = "sqlite3"` package per graph; a store forcing a `libsqlite3-sys` major or `bundled` breaks a host linking its own. Criteria: a host resolves with its own SQLite build; the write path links no SQLite; a cursor compare-and-swap stays one conditional update.

Decision: the store reaches `derived.sqlite` and `machine.sqlite` through port traits in `contextful-core`, beside the `Catalog` port of `topology.coordinate.catalog-port`. One adapter package holds the SQLite implementation and is the sole package depending on `rusqlite`; it enables no link feature, and `contextful-cli` enables `bundled`. A host implements the ports over its own connection or depends on the adapter.

| Option | Lost on | Cost |
| --- | --- | --- |
| Ports in core; SQLite adapter in its own package *(chosen)* | — | One more workspace package, and a host on another `rusqlite` major writes its own adapter. |
| A `rusqlite` version range in the store package | Write path free of SQLite | Every host resolving the store resolves SQLite, and the range breaks at the next major. |
| JSON catalogs beside the manifests | One conditional update | Lease and cursor rows lose the transactional update `topology.coordinate.cursor-cas` rests on. |
| Loading the system SQLite at run time | Host resolves its own build | Two SQLite builds share one process with no link-time check. |

Consequences: a gate rule like `topology.package.store-write-engine-free` holds `contextful-context` free of `libsqlite3-sys`.
