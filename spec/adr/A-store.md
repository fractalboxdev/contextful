# A-store — The store and its sync decisions

**Status:** accepted

## A synced UUID binds a store across clones

`store.init` publishes a random version-4 UUID in `store-id` inside the store root. Owner mint on a root lacking an ID creates it once. Bucket sync carries it as an immutable object: an empty clone accepts the UUID, while a pull into an independently identified store refuses before copying data. Removing and reinitializing a root creates another UUID, so a signed owner claim from the removed root does not follow its filesystem path.

| Option | Lost on | Cost |
| --- | --- | --- |
| Synced immutable UUID *(chosen)* | — | Roots lacking an ID receive one before owner mint; independently initialized stores cannot merge by pull. |
| Digest of the absolute root path | Recreation | A token follows a new store placed at the same path and differs across clones. |
| Issuer key fingerprint | Shared issuers | Two stores signed by one project issuer have the same fingerprint. |

Consequences: a clone holds the original store identity; a new store at an old path does not.

## An empty complete snapshot carries a replacement frontier

A zero-row result has two meanings for a replacing table. A source can finish enumerating its inventory and find no rows, or it can skip unchanged input without reading it. The run records a replacement frontier only for the complete result. The marker lives in the run manifest, survives bucket sync as committed data, and remains effective when a fold publishes an empty snapshot. An incomplete or failed pull commits neither rows nor frontier. A bounded read before the marker still reaches earlier runs while retention keeps them.

| Option | Lost on | Cost |
| --- | --- | --- |
| Explicit frontier in the run manifest *(chosen)* | — | The source must distinguish completeness from a skip. |
| Infer replacement from zero parts | Skipped input | An unchanged input clears a table without examining it. |
| Write a synthetic row | Data shape | A marker appears as source data or needs a hidden-row filter. |

Consequences: a complete empty inventory clears current reads, while a skipped inventory leaves its earlier state readable.

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

Context: schema, manifest, pointer, counter, commit-log and node run-state files carry names and values from rows, and the at-rest cipher covers the store target. Criteria: zero plaintext canary bytes in a bound store; canonical values survive merge and replay; unencrypted stores keep their wire format.

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

## A manifest carries a format version whose major refuses

**Status:** accepted

Context: run and snapshot manifests gain fields under `store.lay-out.manifest-default`, and a reader meeting a layout it cannot interpret either refuses or reads it wrong. Criteria: a newer field never breaks an older reader; a changed meaning never reads silently; the version sits in the file it governs.

Decision: each manifest carries `format_version` as `<major>.<minor>`. A minor adds defaulted fields an older build ignores; a major changes meaning, and a build reading a newer major refuses the table before parsing the rest.

| Option | Lost on | Cost |
| --- | --- | --- |
| `format_version` in each manifest, major refuses *(chosen)* | — | Every manifest carries one more field, and a major bump strands older readers. |
| One store-wide version file | Version in the governed file | A synced manifest arrives without the file that interprets it. |
| No version; parse what parses | Silent misreading | A field whose meaning changed reads under the old meaning. |

Consequences: a mixed fleet refuses a newer major by name instead of folding it. The second major, `2.0`, marks a snapshot carrying a part or segment by reference; its migration is the section below.

## A pass carries untouched parts and sidecar segments by reference

**Status:** accepted; supersedes the whole-snapshot rewrite of `store.fold.pass`.

Context: a pass that rewrote every part and rebuilt every sidecar wrote bytes and held memory in proportion to the table, so a quarter-hour landing over a growing history slowed with age. Criteria: bytes written per byte landed stay bounded as history grows; a pass's memory follows the rows landed; readers and collection see one file list.

Decision: a part entry names its `origin` snapshot; a pass writes delta parts, rewrites only a part a delta row replaces or retention trims, and merges four parts of one size tier. A sidecar extends by a segment while no stored row changes. Collection keeps every file a retained snapshot names. A snapshot carrying a reference declares `2.0`; a `1.x` snapshot reads as self-contained, and its next pass migrates it.

| Option | Lost on | Cost |
| --- | --- | --- |
| Parts and segments by reference, tiered merge *(chosen)* | — | A partition holds several parts, a restated key rewrites its part, and an older build refuses a `2.0` table. |
| Rewrite every part each pass | Bytes per byte landed | Write volume grows with history. |
| Per-row deletion vectors over immutable parts | Readers | Every reader applies a mask the engine does not know. |
| Hard links into each new snapshot directory | Object stores | A bucket has no link, so a push copies every part. |

Consequences: a mixed fleet upgrades every reader before a writer folds by reference.
Revisit: restatements that rewrite large parts on every pass.

## A sidecar's builder is recorded, not encoded in its path

**Status:** accepted

Context: `store.index.paths` fixes one sidecar per column and model or tokenizer, and the graph layout changes with its builder. Criteria: one path per declaration; no reader opens bytes another builder wrote; bounded drift from repeated extension.

Decision: a sidecar's entry and `derived.sqlite` record its builder and builder version. The graph format stays uncommitted: a reader opens only its own builder's version and otherwise takes the exact scan. A pass whose current sidecar records another builder, or 16 segment extensions, rebuilds it in full into a new snapshot.

| Option | Lost on | Cost |
| --- | --- | --- |
| Builder recorded in the entry and the derived catalog *(chosen)* | — | An upgrade pays one full rebuild per sidecar at its next pass. |
| Builder and version in the sidecar path | One path per declaration | Two graphs over one column coexist, and a read needs a selector. |
| A committed, versioned graph format | Builder freedom | Every layout change carries a reader for each older layout. |

Consequences: an upgraded build reads by exact scan until the next pass rebuilds.
Revisit: a second builder declared over one column.

## A tenant value is its bytes

**Status:** accepted

Context: a tenant scope binds the outermost partition column by byte equality, and a canonical form applied at land rewrites what a producer sent. Criteria: the grant, the directory and the row agree without a rule each must apply; no landed value changes.

Decision: land checks a tenant value against no canonical form and keeps its exact bytes; two spellings a normalizer equates are two tenants.

| Option | Lost on | Cost |
| --- | --- | --- |
| Exact bytes, no canonical form *(chosen)* | — | A producer sending two spellings of one tenant splits it, visibly. |
| Normalize at land | No rewritten value | The stored value differs from the source's, and a grant minted from the source misses it. |
| Refuse a non-canonical value | Adoption cost | A source emitting decomposed text stops landing. |

Consequences: tenant hygiene belongs to the producer and the grant issuer.
Revisit: a source whose tenant spelling varies between batches.

## A sidecar copies only the tenant and zone

**Status:** accepted

Context: a filtered traversal reads a filter value per graph node, and any column copied into a sidecar sits outside the enforced relation. Criteria: no row value escapes enforcement; restricted reads keep recall.

Decision: a sidecar holds its `id_column` and indexed column alone, save the tenant column and zone label, which already name its partition and path. Every other predicate applies when candidates re-join through the enforced relation.

| Option | Lost on | Cost |
| --- | --- | --- |
| Tenant and zone only *(chosen)* | — | A restricted read oversamples instead of filtering inside the graph. |
| Any declared filter column | Enforcement | A copied column bypasses masks and row policy on disk and in the bucket. |
| No copies at all | Recall | A tenant-scoped probe walks other tenants' nodes. |

Consequences: a filter beyond tenant and zone costs oversampling, never exposure.
Revisit: a declared filter whose oversampling misses recall targets.

## A validity interval is one row

**Status:** accepted

Context: one pair of valid-time columns holds one interval, and a key can be valid over several. Criteria: a fold keeps every interval; a bounded read stays one predicate over the pair.

Decision: each interval is its own row. A keyed table declaring `valid_time` dedupes on its primary key and `from`, so a fold supersedes a repeated interval and keeps distinct ones.

| Option | Lost on | Cost |
| --- | --- | --- |
| One row per interval *(chosen)* | — | A key's rows grow with its intervals. |
| A list of intervals in one row | One predicate | Each read unnests the list before filtering. |
| A child interval table | One scan | Every bounded read joins. |

Consequences: `valid_as_of` stays one predicate over the declared pair.
Revisit: a key whose interval count makes the row set dominate the table.

## An exhausted pull refuses and resumes

**Status:** accepted

Context: pushes arriving faster than a pull's re-fetch shrinks its shortfall exhaust `store.pull.convergence`. Criteria: no pointer ahead of its data; a typed outcome; repeated work bounded by what is still missing.

Decision: exhaustion raises a typed refusal and writes no pointer, and every object already verified stays on disk, so the next pull fetches only the keys still differing.

| Option | Lost on | Cost |
| --- | --- | --- |
| Typed refusal, verified objects kept *(chosen)* | — | An operator or schedule re-runs the pull. |
| Retry without bound | Termination | A pull racing a busy bucket holds its process indefinitely. |
| Publish the converged tables | No pointer ahead of data | A pointer reaches a snapshot whose sidecars are still moving. |

Consequences: a contended pull converges across runs, each downloading less.
Revisit: pulls refusing under ordinary write rates.

## A tombstone is signed by its node's issuer key

**Status:** accepted

Context: a tombstone names its owner by node id, which carries no key material, so any writer can claim another node's deletion. Criteria: a deletion verifies to a key the project records; an unverifiable deletion removes nothing; no new key custody.

Decision: a push signs each tombstone with its node's issuer key over the key, owner and `deleted_at`. A merge or pull accepts one only when the signature verifies and the key-set ledger records the signer as verifying at `deleted_at`.

| Option | Lost on | Cost |
| --- | --- | --- |
| Issuer key, checked against the key-set ledger *(chosen)* | — | A node without an issuer key, or outside the ledger, propagates no deletion. |
| A per-node sync key | Key custody | A second key per machine to mint, rotate and record. |
| Unsigned tombstones | Verifiable deletion | A forged tombstone removes another node's entries. |

Consequences: retiring an issuer key ends the deletions it signs after retirement.
Revisit: a deployment whose writers hold no issuer key.

## A replica advertises a descriptor beside its catalog

**Status:** accepted

Context: a replica holds whole snapshots, and a router or reader needs to know which sidecars and parts it holds before sending a query. Criteria: no central service; the answer matches the disk; a missing sidecar refuses rather than degrading.

Decision: each replica pull writes `replica.json` beside `derived.sqlite`, naming per table each snapshot held with its parts and sidecar paths on disk. A read needing an entry the descriptor omits raises `ReplicaMissingIndex`, naming the pull that supplies it.

| Option | Lost on | Cost |
| --- | --- | --- |
| A local descriptor beside the catalog *(chosen)* | — | A router reads one file per replica. |
| A queryable central row | No central service | Every replica writes to shared state it otherwise never touches. |
| Probe the disk per query | Matches the disk | A read races a pull rewriting the files it probes. |

Consequences: a replica's holdings read without opening a snapshot manifest.

## Availability is a declared column resolved at landing

**Status:** accepted

Context: a backfill stamps `_ingested_at` with its commit instant, so a 2026 load of 2014 history answers an `as_of` read in 2014 with nothing and an unbounded read with values published after 2014. Criteria: the instant is the source's, not the engine's; `_commit_seq` stays monotonic; a landed row answers one way; enforcement holds the predicate.

Decision: `available_time` names a declared `timestamptz` column. Landing resolves it, by a lag fill or a date's following midnight in the declared zone, and writes the instant; `available_as_of` compares that column alone, beneath enforcement.

| Option | Lost on | Cost |
| --- | --- | --- |
| A declared column resolved at landing *(chosen)* | — | Each table declares the column or a lag. |
| An engine-stamped third clock | The source's instant | The engine never observes when a source published a value. |
| Overriding the transaction stamp | Monotonic `_commit_seq` | A backdated `_ingested_at` breaks commit order and `as_of` reproducibility. |
| A basis resolved at read time | One answer per row | One row answers differently as lag rules change. |
| Filtering in user SQL | Enforcement | Every tool and template restates the predicate, and one omission leaks. |

Consequences: `as_of` freezes the corpus so an eval reproduces; `available_as_of` sets the simulated present.
Revisit: a source publishing a per-row availability stamp the engine verifies.

## A restatement is a vintage row

**Status:** accepted

Context: a statistics agency revises a figure after first release, and a filer restates a period. A keyed table keeps one survivor per key, so a restatement overwrites the value an earlier reader saw, and only a snapshot inside `retain_runs` still answers with it. Criteria: a past availability bound answers the same in every later year; one scan serves a bounded read; a re-pull adds nothing.

Decision: a keyed table declaring `available_time` keys each row on its availability instant too. A read resolves one vintage per key and valid-time `from`; a declared content hash suppresses an unchanged re-pull, and a wall-clock row-age cutoff is refused.

| Option | Lost on | Cost |
| --- | --- | --- |
| A vintage row in the table *(chosen)* | — | A revised series grows by one row per release. |
| Raising `retain_runs` | Answers in every later year | Expiry still collects the snapshot holding the first release, and storage grows with every run. |
| A separate vintage table | One scan | Every bounded read joins, and two tables commit apart. |

Consequences: a fold and the expiry of `retain_runs` leave every vintage readable.
Revisit: an estimate-revision source whose vintage rows outgrow the table's partition bounds.

## A vintage landing reads only its own keys' history

Context: the content-hash comparison runs under the table's commit lock. Decoding every stored vintage there makes a daily backfill quadratic in its days and holds every concurrent writer behind history it never matches. Criteria: commit cost independent of table size; no new structure to keep in step.

Decision: the comparison decodes only row groups whose key bloom filters and footer bounds on key and valid-time columns admit a landing row, projecting the compared columns one record batch at a time. Run parts carry declared bloom filters, sized per part, except in an encrypted project, where Parquet would leave them in plaintext. A fold over a valid-time vintage table sorts by valid-time `from` before key.

| Option | Lost on | Cost |
| --- | --- | --- |
| Pruned read of the stored parts *(chosen)* | — | A random-key table without valid time prunes only where it declares `bloom_filter`. |
| Decode the whole table per landing | Commit cost | A 100-day backfill of 100 symbols decodes 495,000 stored rows. |
| A latest-vintage sidecar per table | No new structure | Every commit, fold, erasure and sync keeps a second file in step. |

Consequences: a valid-time vintage snapshot sorts by time, so a key lookup in it reads every row group's key bounds.
Revisit: a random-key vintage table declaring no bloom filter whose landings decode its history.

## A column records the commit that first landed it

`schema.json` carries `first_landed_at` per column, set once by the merge that first adds the column and never moved by a later merge; bucket sync carries it with the schema. On a table declaring `available_time` the instant is the earliest availability among that batch's rows carrying the column, so a backfill landed years later keeps its fields visible at the horizons its rows answer; elsewhere it is the commit instant. `read.horizon.late-columns` drops a column first landed after the horizon.

| Option | Lost on | Cost |
| --- | --- | --- |
| A per-column availability instant in `schema.json` *(chosen)* | — | A schema written before the field exists reads every column as always present. |
| The commit instant alone | Backfill | A 2026 backfill hides every column of its 2014 rows. |
| A separate append-only column log per table | One home | A second file the schema merge and sync must keep in step. |
| Scan the earliest run carrying the column | Cost | Every horizon session reads run manifests to find a schema fact. |

## A snapshot corpus is a declared table pinned by snapshot id

`[pipeline.tables.snapshot]` names the corpus's `url`, `captured_at`, `digest` and `source` columns and an optional `leak_score` column. A capture lands through the ordinary write path and commits as one store snapshot; an evaluation case pins it by that `snapshot` id. A document captured after the horizon answers a horizon read only while its `leak_score` holds 0, so an unscored late capture never answers.

| Option | Lost on | Cost |
| --- | --- | --- |
| Declared columns, pinned snapshot, zero threshold *(chosen)* | — | A late capture needs a judged score of 0 before any horizon read sees it. |
| A per-case leak threshold | Default safety | A case author can admit a partly leaked page. |
| Consumer-landed leak rows beside the corpus | One home | The read joins a second table to decide admission. |

## Adjustment factors apply at read time, back from each factor's effective date

A price table keeps raw values. Under `available_as_of` each listed numeric column multiplies by the product of the factor table's vintages available by the bound that agree on the factor table's key and take effect after the row's valid-time `from`. An unbounded read returns raw values. A re-pull matching its key's latest content hash stays on disk and drops at read.

| Option | Lost on | Cost |
| --- | --- | --- |
| Raw storage, factors applied per read *(chosen)* | — | Each bounded read joins the factor table. |
| Store back-adjusted prices | Point-in-time | A 2014 price encodes a split announced in 2020. |
| One adjusted copy per factor vintage | Storage | Every split rewrites the price history. |

## A push tombstones only the owned keys its store held

**Status:** accepted; amends the scoped-union decision above, whose merge read every owned key a store lacks as its deletion.

Context: ownership reads the node id off a key, and two disks pushing under one id each lack the other's runs. Criteria: a deletion propagates only from a store that held the key; a replacement host keeps its node's history; no liveness service.

Decision: a store records the keys of its node it held at its last push or pull, a pull extending the record under the push guard. A merge tombstones an owned remote entry only where that record lists it and the store lacks it; another owned entry passes through.

| Option | Lost on | Cost |
| --- | --- | --- |
| A local record of held keys *(chosen)* | — | One dot-file per store, rewritten each push, sized by the node's keys. |
| Refuse a push under a node id another live disk holds | Replacement hosts | Telling a live sibling from a replaced host needs a node lease no store keeps. |
| A base generation, its `gen-<N>.json` listing the held keys | Cold disks | A replaced disk keeps no base, and a missing copy erases it. |
| Every owned key a store lacks | Sibling disks | One disk erases every run the other pushed. |

Consequences: a store with no record whose earlier push left its push guard file reads as holding every owned key, so its pending deletions still propagate; a store that never pushed reads as holding none.
Revisit: two disks under one node id overwriting one run state.

## A lost commit round counts only when no newer generation landed

**Status:** accepted; amends the scoped-union decision above, whose retry bound counted every lost round.

Context: in lockstep each commit round has one winner, so a fixed bound of 5 rounds exhausts every writer past the fifth. Criteria: writers that progress all commit; a stuck bucket still refuses; a bound reproduces in a test.

Decision: a lost round whose re-read manifest carries a newer generation resets the retry count; `push_retries` bounds consecutive rounds meeting none, and a ceiling of 64 rounds bounds every push.

| Option | Lost on | Cost |
| --- | --- | --- |
| Progress-counted retries under a round ceiling *(chosen)* | — | A writer beaten every round spends up to 64 rounds before it refuses. |
| A fixed bound on lost rounds | Busy buckets | Eight lockstep writers leave three refused. |
| A wall-clock bound | Reproducibility | The outcome depends on bucket latency. |

Consequences: `SyncManifestRebaseExhausted` names a bucket that stops advancing, or a writer outrun 64 times.
Revisit: deployments whose concurrent writers exceed 64.
