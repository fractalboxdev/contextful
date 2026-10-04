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

## Canonical metadata sits inside an authenticated envelope

**Status:** accepted

Context: schema, manifest, pointer, counter and commit-log files carry names and values from rows, and the at-rest cipher covers the store target. Criteria: zero plaintext canary bytes in a bound store; canonical values survive merge and replay; unencrypted stores keep their wire format.

Decision: a bound store seals each metadata file under a fresh wrapped data key and reads it into memory before parsing. Schema merge operates on decrypted canonical structures and seals the merged result. A store without encryption retains canonical JSON or text on disk and in the bucket. A bound store refuses plaintext metadata rather than migrating it implicitly.

| Option | Lost on | Cost |
| --- | --- | --- |
| Versioned file envelope over canonical bytes *(chosen)* | — | Raw JSON readers need the project key and file codec. |
| Cleartext metadata beside encrypted data | At-rest scope | Schema and manifest names expose row structure and canary values. |
| Encrypt individual JSON fields | Zero plaintext | Keys and unselected values remain visible and each parser needs field rules. |

Consequences: sync authenticates metadata before merge; a key-bound store starts from an empty tree or an explicit migration.

## The machine catalog seals a SQLite snapshot

**Status:** accepted

Context: `machine.sqlite` holds machine-local leases, cursors and run rows; a bound store protects row values on disk while keeping the catalog's conditional updates across processes. Criteria: zero plaintext canary bytes, durable updates and one linearizable compare-and-swap.

Decision: each operation locks a stable sibling file, decrypts the authenticated snapshot into an in-memory SQLite connection and runs one transaction. A committed write serializes the database, seals it under a fresh wrapped data key and replaces the snapshot through a synced temporary file. A bound catalog refuses plaintext or a foreign key. An unencrypted catalog keeps SQLite's file-backed transactions.

| Option | Lost on | Cost |
| --- | --- | --- |
| Sealed SQLite snapshot under a file lock *(chosen)* | — | Each write copies the catalog in memory and rewrites its full encrypted file. |
| Plain SQLite file with encrypted row values | Zero plaintext | Index keys and SQLite pages still expose catalog structure and old values. |
| A plaintext temporary database | At-rest scope | A crash leaves readable pages beside the sealed file. |

Consequences: catalog size sets memory and write cost; a missing or invalid sealed snapshot refuses rather than rebuilding machine state.

## A sidecar is memory-mapped only over plaintext

**Status:** accepted

Context: `store.encrypt.cipher` seals every sidecar with AES-256-GCM per file, and a sealed file cannot be memory-mapped, while the HNSW and full-text readers scale past memory only by mapping plaintext. Criteria: the at-rest scope holds on local disk and in the bucket; no bespoke cipher format; an unencrypted project reads a sidecar larger than memory.

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

Decision: a sidecar declaration names one `id_column`, unique within each snapshot, defaulting to a single-column primary key on a table without valid time, and required otherwise, since a valid-time table repeats its key across lines. Vector and full-text sidecars share it, their candidate ids are its values, and `store.index.candidate-ids` re-joins on it. A fold meeting a repeated value refuses the pass.

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

## A variant column keeps each row's scalar kind in one Struct

**Status:** accepted; amends the lattice decision above, whose variant row lost on read-time recoverability because no single Arrow type described the column.

Context: one attribute key arrives as a string from one sender and an integer from another; the lattice refuses the pair, and a `Json` column holds no bytes kind and parses on every read. Criteria: every value reads back with the type it was sent as; one Arrow type describes the column; the linked engine and parquet releases read it.

Decision: a `variant` column is a Struct of per-kind fields `str`, `int`, `double`, `bool` and `bytes` plus a `kind` tag, so it rests on the Struct column type. Land checks that a row holds at most one non-null field. Parquet `VARIANT` stays rejected until DuckDB and parquet support for it is measured.

| Option | Lost on | Cost |
| --- | --- | --- |
| Struct of per-kind fields and a kind tag *(chosen)* | — | Five nullable fields per value, and a raw query projects by `kind`. |
| Parquet `VARIANT` | Engine support | Unmeasured in the linked DuckDB and parquet releases. |
| A declaration key expanding to sibling typed columns | One column | Every table repeats six columns, and the check spans them. |
| The long table left to each consumer | One check | Every consumer re-implements the at-most-one-non-null check. |

Consequences: a mixed-kind source lands without widening the lattice, and an `Int64` past the exact float range keeps its value. The accepted cost is a wider stored schema and a variant column that takes no promotion.

## A view is a SQL model the engine builds, published with a per-input frontier

**Status:** accepted; amends `run.publish.manifest-section` and `read.resolve-pin.resolved-echo`, whose watermark becomes a per-input frontier.

Context: `view` is a table key no package reads, so a derived relation reaches a caller only as SQL sent with every statement, and every read pays the rollup. Criteria: two figures compare only when their builds saw the same landings; a view never publishes a row a reader's restriction withholds; a refused build removes nothing.

Decision: `view` holds one `SELECT` over store tables and other views, admitted by `read.guard` at declaration. The engine builds it and publishes it as a model; a refused build keeps the prior build serving. Its watermark is a per-input frontier: per input table, the snapshot id and the committed runs that snapshot omits. A view over a table carrying a row policy or masks declares its own `policy`, else `ViewPolicyUndeclared`.

| Option | Lost on | Cost |
| --- | --- | --- |
| Engine-built model, per-input frontier, own policy *(chosen)* | — | Each build rescans its inputs, and a view author writes a policy for every restricted input. |
| SQL prepended to each statement | Read cost | Every panel pays the full rollup and version selection. |
| A single-instant watermark | Comparability | Two writers commit out of stamp order, so an instant names no landing set. |
| Input policies inherited by the view | Soundness | A join or aggregate changes which rows a restriction reaches. |

Consequences: `contextful.resolved` names the landings behind each figure. A build reads as the operator, so the declared policy is the only restriction a view carries.

## The SQLite link rule reads normal dependencies alone

**Status:** accepted; narrows the catalog-port decision above, whose link rule named no dependency kind.

Context: `topology.package.sqlite-adapter` refused any declaration of the binding outside the adapter, while its sibling link rules read normal dependencies alone and the check skips every entry carrying a kind. Criteria: a rule refuses what reaches a profile's link line; sibling rules read one dependency kind; clause and check agree.

Decision: the rule reads normal dependencies. Under Cargo's resolver 2 a dev-dependency enters only test builds and a build-dependency's features resolve apart from the normal graph, so neither forces a SQLite build into a profile.

| Option | Lost on | Cost |
| --- | --- | --- |
| Normal dependencies alone *(chosen)* | — | A test or build script may link its own SQLite, which the rule does not see. |
| Every dependency kind, exempting the adapter's own dev-dependency | Profile relevance | A test fixture linking SQLite reds a gate guarding the profile graphs. |

Consequences: the check and its clause agree, and a build script's SQLite answers to Cargo's own `links` check alone.

## A keyed table is covered by a scheduled fold job

**Status:** accepted

Context: `store.fold.triggers` fires a pass only when something invokes `context compact`, so a keyed table no schedule compacts accumulates runs without bound. Criteria: coverage computable at validation from the declaration alone; one home for fold schedules; a check that names the table.

Decision: a `[[job]]` block of kind `fold` carrying a `schedule`, with `enabled` absent or true, covers the table its `target` names, or every table when it names none. `pipeline validate` warns for each keyed table no such job covers; a seeded table refuses, because its union read carries two rows per key until a fold writes the snapshot.

| Option | Lost on | Cost |
| --- | --- | --- |
| A scheduled, enabled `fold` job, targeted or targetless *(chosen)* | — | A targetless fold job covers every table, one a machine does not compact included. |
| The pass triggers alone count as coverage | Computable at validation | Nothing fires the triggers without an invocation, so every table reads covered. |
| A per-table `compact` schedule on the table block | One home for fold schedules | Two keys schedule one pass, and the job union loses its single cadence. |
| Refuse every uncovered keyed table | Adoption cost | A project with a hand-run `context compact` stops validating. |

Consequences: a seeded pipeline carries its fold job in the same declaration, and disabling that job refuses the pipeline.

## Struct, list and map columns reconcile field by field

**Status:** accepted

Context: nested values landed as JSON text or child tables, paying a parse or a join per read. Criteria: a record reads with its parent in one scan; the lattice stays the one place a type widens; no landing drops a value silently.

Decision: the lattice adds `Struct`, `List` and `Map` with `Utf8` keys, recursive over every column type. A struct gains fields additively; items and map values reconcile by the scalar lattice; a kind change refuses at the write, naming its path. A nested column takes no key role, masks whole by `drop`, and reads as JSON arrays and objects. `native` normalize infers nested types on the store sink; a direct landing keeps `Json` unless typed.

| Option | Lost on | Cost |
| --- | --- | --- |
| Nested types, additive structs, path-named refusals *(chosen)* | — | A key outside a typed struct refuses its batch; a dynamic key set widens a struct. |
| Child tables per list | One scan | A join per nested read. |
| `Json` text throughout | One scan | Every read parses and casts. |
| Struct fields fixed at first sight | Silent drops | A later field refuses or vanishes. |

Consequences: a producer with an open key set declares `map<utf8, …>`, and only a union keeps a text form.
