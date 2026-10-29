# A-store — The store and its sync decisions

**Status:** accepted

## synced-uuid-binds — A synced UUID binds a store across clones

A signed owner claim from a removed root does not follow its filesystem path. A pull into an independently identified store refuses before copying data.

| Option | Lost on | Cost |
| --- | --- | --- |
| Synced immutable UUID *(chosen)* | — | Independently initialized stores cannot merge by pull. |
| Digest of the absolute root path | Recreation | A token follows a new store placed at the same path and differs across clones. |
| Issuer key fingerprint | Shared issuers | Two stores signed by one project issuer have the same fingerprint. |

## empty-complete-snapshot — An empty complete snapshot carries a replacement frontier

A zero-row result means either a complete empty inventory or a skipped unchanged input. Only the complete result clears current reads.

| Option | Lost on | Cost |
| --- | --- | --- |
| Explicit frontier in the run manifest *(chosen)* | — | The source distinguishes completeness from a skip. |
| Infer replacement from zero parts | Skipped input | An unchanged input clears a table without examining it. |
| Write a synthetic row | Data shape | A marker appears as source data or needs a hidden-row filter. |

## schema-lattice-has — The schema lattice has one promotion, and a key never widens

`Int64` with `Float64` is the only pairing that produces physically mixed Parquet, and the scan widens it with no per-column cast. A widened key past the exact-integer range merges distinct keys under the fold.

| Option | Lost on | Cost |
| --- | --- | --- |
| One promotion; keys excluded; refuse at the write *(chosen)* | — | A source flipping between text and number fails its batch until fixed. |
| Widen every clash to text | Read-time recoverability | Every query casts; a numeric aggregate becomes a string comparison. |
| Key widening as an operator obligation | Irreversibility | One out-of-range value silently merges distinct keys. |

Revisit: a second pairing that some source emits routinely and the scan resolves losslessly.

## coordination-probed-compare — Coordination is probed compare-and-swap with fences the storage enforces

A fence the writer checks before commit lets a holder paused between check and write commit after its successor. The storage evaluates the fence inside the conditional write instead.

| Option | Lost on | Cost |
| --- | --- | --- |
| Storage-enforced fences; per-table compaction lease *(chosen)* | — | One conditional write per commit, and a never-deleted lease object per pipeline and table. |
| Compare the fence in the writer before commit | Pause safety | A holder pausing between check and write commits after its successor. |
| An embedded consensus cluster | Operational cost | A second system to run for a low request rate. |

Consequences: a backend without demonstrated conditional writes runs single-writer by refusal, never by silent degradation.
Revisit: a backend whose access policy enforces a generation bound for every writer.

## replica-read-whole — A replica is a read-only whole-snapshot cache

A replica never answers from what is present and never fetches mid-query, so its answer is complete for the snapshot it names, or a refusal. Cursor and lease rows live in `machine.sqlite`, which no pull or rebuild touches.

| Option | Lost on | Cost |
| --- | --- | --- |
| Read-only, whole-Parquet, sensitive tables off *(chosen)* | — | A replica stores full snapshots; a sensitive read needs a round trip to the proxying face. |
| Hold a partition subset and check predicates at query time | Silent wrongness | A missed partition returns a short answer. |
| Replicate sensitive tables and mask at the consumer | Failure direction | A misconfiguration has already moved the bytes onto the machine. |

## bucket-index-commits — The bucket index commits by compare-and-set over a scoped union of the remote

The remote contributes an entry only where this writer has never written, so an owned entry absent locally drops out and retention reaches a consumer.

| Option | Lost on | Cost |
| --- | --- | --- |
| Compare-and-set over a scoped union, bounded rebase *(chosen)* | — | Every writer-owned tier needs an ownership arm; an unowned key never propagates deletion. |
| A blanket union of both indexes | Deletion propagation | The index grows forever against a shrinking tree. |
| Replace the remote index wholesale | Visibility of committed objects | A race loser unlists the winner's objects. |

Consequences: a contended bucket can leave a writer's objects durable and unlisted, visible in the refusal.
Revisit: a key shape whose ownership is not derivable from the key; the index outgrowing a single-object commit.

## canonical-metadata-sits — Canonical metadata sits inside an authenticated envelope

Metadata files carry names and values from rows, so a bound store holds zero plaintext canary bytes in them. Schema merge operates on decrypted canonical structures.

| Option | Lost on | Cost |
| --- | --- | --- |
| Versioned file envelope over canonical bytes *(chosen)* | — | Raw JSON readers need the project key and file codec. |
| Cleartext metadata beside encrypted data | At-rest scope | Schema and manifest names expose row structure and canary values. |
| Encrypt individual JSON fields | Zero plaintext | Keys and unselected values remain visible. |

## machine-catalog-seals — The machine catalog seals a SQLite snapshot

A bound store protects catalog row values on disk and keeps one linearizable compare-and-swap across processes.

| Option | Lost on | Cost |
| --- | --- | --- |
| Sealed SQLite snapshot under a file lock *(chosen)* | — | Each write copies the catalog in memory and rewrites its full encrypted file. |
| Plain SQLite file with encrypted row values | Zero plaintext | Index keys and SQLite pages still expose structure and old values. |
| A plaintext temporary database | At-rest scope | A crash leaves readable pages beside the sealed file. |

## sidecar-memory-mapped — A sidecar is memory-mapped only over plaintext

A sealed file cannot be memory-mapped, and the HNSW and full-text readers scale past memory only by mapping plaintext.

| Option | Lost on | Cost |
| --- | --- | --- |
| Mmap over plaintext; decrypt into anonymous memory when encrypted *(chosen)* | — | An encrypted project holds each open sidecar whole in memory, so its size cap binds. |
| A chunked AEAD format decrypted per page fault | Bespoke cipher format | A custom reader per builder and a cryptographic format to audit. |
| Decrypt to a private on-disk cache | At-rest scope | A stolen disk yields the cleartext graph. |

Revisit: a builder that reads through a caller-supplied page source, making per-page decryption a reader rather than a format.

## sidecar-names-id — Every sidecar names one id column

Keyed, composite-key and unkeyed tables alike need a sidecar, with ids stable across folds. A valid-time table repeats its key across lines, so it declares the column.

| Option | Lost on | Cost |
| --- | --- | --- |
| A declared unique `id_column` shared by every kind *(chosen)* | — | An unkeyed producer emits a unique row id, such as a content digest. |
| An encoded composite primary key | Unkeyed coverage | An append table has no key to encode. |
| File and row ordinal | Stability across folds | Every fold renumbers rows, so no sidecar extends incrementally. |

## binary-fixed-size — Binary and fixed-size vector columns take no promotion

An embedding stored as a JSON array costs several times its float16 size and a parse per vector read.

| Option | Lost on | Cost |
| --- | --- | --- |
| Typed binary and vector columns; widen half floats on read *(chosen)* | — | A read materializes float16 vectors at twice their stored width. |
| Hex text and JSON arrays in `Utf8` | Storage width | Five to eight times the float16 bytes, and a parse per vector read. |
| A variable-length float list | Validation-free read | Every row checks its dimension before a builder accepts it. |

## store-s-catalogs — The store's catalogs sit behind a port with SQLite in its own package

Cargo admits one `links = "sqlite3"` package per graph, so a store forcing a `libsqlite3-sys` major or `bundled` breaks a host linking its own.

| Option | Lost on | Cost |
| --- | --- | --- |
| Ports in core; SQLite adapter in its own package *(chosen)* | — | One more workspace package; a host on another `rusqlite` major writes its own adapter. |
| A `rusqlite` version range in the store package | Write path free of SQLite | Every host resolving the store resolves SQLite. |
| JSON catalogs beside the manifests | One conditional update | Lease and cursor rows lose their transactional update. |

## variant-column-keeps — A variant column keeps each row's scalar kind in one Struct

One attribute key arrives as a string from one sender and an integer from another. The Struct reads back each value with the type it was sent as, under one Arrow type.

| Option | Lost on | Cost |
| --- | --- | --- |
| Struct of per-kind fields and a kind tag *(chosen)* | — | Five nullable fields per value, and a raw query projects by `kind`. |
| Parquet `VARIANT` | Engine support | Unmeasured in the linked DuckDB and parquet releases. |
| A declaration key expanding to sibling typed columns | One column | Every table repeats six columns. |

Revisit: Parquet `VARIANT` support measured.

## sqlite-link-rule — The SQLite link rule reads normal dependencies alone

Under Cargo's resolver 2 a dev-dependency enters only test builds, and a build-dependency resolves apart. Neither forces SQLite into a profile.

| Option | Lost on | Cost |
| --- | --- | --- |
| Normal dependencies alone *(chosen)* | — | A test or build script may link its own SQLite, unseen by the rule. |
| Every dependency kind, exempting the adapter's own dev-dependency | Profile relevance | A test fixture linking SQLite reds a gate guarding the profile graphs. |

## keyed-table-covered — A keyed table is covered by a scheduled fold job

A pass fires only when something invokes `context compact`, so an uncovered keyed table accumulates runs without bound. Coverage is computable at validation from the declaration alone.

| Option | Lost on | Cost |
| --- | --- | --- |
| A scheduled, enabled `fold` job, targeted or targetless *(chosen)* | — | A targetless fold job covers every table, one a machine does not compact included. |
| A per-table `compact` schedule on the table block | One home for fold schedules | Two keys schedule one pass. |
| Refuse every uncovered keyed table | Adoption cost | A project with a hand-run `context compact` stops validating. |

## struct-list-map — Struct, list and map columns reconcile field by field

A record reads with its parent in one scan, and the lattice stays the one place a type widens.

| Option | Lost on | Cost |
| --- | --- | --- |
| Nested types, additive structs, path-named refusals *(chosen)* | — | A key outside a typed struct refuses its batch; a dynamic key set widens a struct. |
| Child tables per list | One scan | A join per nested read. |
| `Json` text throughout | One scan | Every read parses and casts. |

## manifest-carries-format — A manifest carries a format version whose major refuses

A minor adds defaulted fields an older build ignores; a major changes meaning. The version sits in the file it governs.

| Option | Lost on | Cost |
| --- | --- | --- |
| `format_version` in each manifest, major refuses *(chosen)* | — | A major bump strands older readers. |
| One store-wide version file | Version in the governed file | A synced manifest arrives without the file that interprets it. |
| No version; parse what parses | Silent misreading | A changed field reads under the old meaning. |

## pass-carries-untouched — A pass carries untouched parts and sidecar segments by reference

A `store.fold` pass that rewrites every part writes bytes in proportion to the table, so each landing over a growing history costs more than the last.

| Option | Lost on | Cost |
| --- | --- | --- |
| Parts and segments by reference, tiered merge *(chosen)* | — | A restated key rewrites its part; an older build refuses a `2.0` table. |
| Rewrite every part each pass | Bytes per byte landed | Write volume grows with history. |
| Hard links into each new snapshot directory | Object stores | A bucket has no link, so a push copies every part. |

Consequences: a mixed fleet upgrades every reader before a writer folds by reference.
Revisit: restatements that rewrite large parts on every pass.

## sidecar-s-builder — A sidecar's builder is recorded, not encoded in its path

The graph layout changes with its builder, and one path serves each declaration.

| Option | Lost on | Cost |
| --- | --- | --- |
| Builder recorded in the entry and the derived catalog *(chosen)* | — | An upgrade pays one full rebuild per sidecar at its next pass. |
| Builder and version in the sidecar path | One path per declaration | Two graphs over one column coexist, and a read needs a selector. |
| A committed, versioned graph format | Builder freedom | Every layout change carries a reader for each older layout. |

Consequences: writers of two builder versions rebuild each other's sidecars on every pass, so an upgrade replaces every writer first.
Revisit: a second builder declared over one column.

## tenant-value-bytes — A tenant value is its bytes

A tenant scope binds the partition column by byte equality, so the grant, the directory and the row agree without a rule each applies.

| Option | Lost on | Cost |
| --- | --- | --- |
| Exact bytes, no canonical form *(chosen)* | — | A producer sending two spellings of one tenant splits it, visibly. |
| Normalize at land | No rewritten value | A grant minted from the source value misses the stored one. |
| Refuse a non-canonical value | Adoption cost | A source emitting decomposed text stops landing. |

Revisit: a source whose tenant spelling varies between batches.

## sidecar-copies-tenant — A sidecar copies only the tenant, zone and availability instant

Any column copied into a sidecar sits outside the enforced relation. A filter beyond tenant and zone costs oversampling, never exposure.

| Option | Lost on | Cost |
| --- | --- | --- |
| Tenant, zone and availability instant *(chosen)* | — | A restricted read oversamples; a mask on the availability column leaves its sidecar copy. |
| Any declared filter column | Enforcement | A copied column bypasses masks and row policy on disk and in the bucket. |
| No copies at all | Recall | A tenant-scoped probe walks other tenants' nodes. |

Revisit: a declared filter whose oversampling misses recall targets.

## validity-interval-row — A validity interval is one row

A key can be valid over several intervals, and a bounded read stays one predicate over the declared pair.

| Option | Lost on | Cost |
| --- | --- | --- |
| One row per interval *(chosen)* | — | A key's rows grow with its intervals. |
| A list of intervals in one row | One predicate | Each read unnests the list before filtering. |
| A child interval table | One scan | Every bounded read joins. |

Revisit: a key whose interval count makes the row set dominate the table.

## exhausted-pull-refuses — An exhausted pull refuses and resumes

Pushes arriving faster than a re-fetch shrinks its shortfall exhaust a pull. A contended pull then converges across runs, each downloading less.

| Option | Lost on | Cost |
| --- | --- | --- |
| Typed refusal, verified objects kept *(chosen)* | — | An operator or schedule re-runs the pull. |
| Retry without bound | Termination | A pull racing a busy bucket holds its process indefinitely. |
| Publish the converged tables | No pointer ahead of data | A pointer reaches a snapshot whose sidecars are still moving. |

Revisit: pulls refusing under ordinary write rates.

## tombstone-signed-node — A tombstone is signed by its node's issuer key

A node id carries no key material, so an unsigned tombstone lets any writer claim another node's deletion. The issuer key adds no new key custody.

| Option | Lost on | Cost |
| --- | --- | --- |
| Issuer key, checked against the key-set ledger *(chosen)* | — | A node without an issuer key, or outside the ledger, propagates no deletion. |
| A per-node sync key | Key custody | A second key per machine to mint, rotate and record. |
| Unsigned tombstones | Verifiable deletion | A forged tombstone removes another node's entries. |

Revisit: a deployment whose writers hold no issuer key.

## replica-advertises-descriptor — A replica advertises a descriptor beside its catalog

A router needs a replica's holdings before sending a query, with no central service.

| Option | Lost on | Cost |
| --- | --- | --- |
| A local descriptor beside the catalog *(chosen)* | — | A router reads one file per replica. |
| A queryable central row | No central service | Every replica writes to shared state it otherwise never touches. |
| Probe the disk per query | Matches the disk | A read races a pull rewriting the files it probes. |

## availability-declared-column — Availability is a declared column resolved at landing

A backfill stamps `_ingested_at` with its commit instant, so a 2026 load of 2014 history answers a 2014 `as_of` read with nothing. The instant is the source's, not the engine's.

| Option | Lost on | Cost |
| --- | --- | --- |
| A declared column resolved at landing *(chosen)* | — | Each table declares the column or a lag. |
| Overriding the transaction stamp | Monotonic `_commit_seq` | A backdated `_ingested_at` breaks commit order and `as_of` reproducibility. |
| Filtering in user SQL | Enforcement | Every tool restates the predicate, and one omission leaks. |

Revisit: a source publishing a per-row availability stamp the engine verifies.

## restatement-vintage-row — A restatement is a vintage row

A keyed table keeps one survivor per key, so a revision overwrites the value an earlier reader saw. A past availability bound answers the same in every later year.

| Option | Lost on | Cost |
| --- | --- | --- |
| A vintage row in the table *(chosen)* | — | A revised series grows by one row per release. |
| Raising `retain_runs` | Answers in every later year | Expiry still collects the snapshot holding the first release. |
| A separate vintage table | One scan | Every bounded read joins, and two tables commit apart. |

Revisit: an estimate-revision source whose vintage rows outgrow the table's partition bounds.

## vintage-landing-reads — A vintage landing reads only its own keys' history

The content-hash comparison runs under the commit lock, so decoding every stored vintage makes a daily backfill quadratic in its days. Bloom filters stay off in an encrypted project, where Parquet keeps them in plaintext.

| Option | Lost on | Cost |
| --- | --- | --- |
| Pruned read of the stored parts *(chosen)* | — | A random-key table without valid time prunes only where it declares `bloom_filter`. |
| Decode the whole table per landing | Commit cost | A 100-day backfill of 100 entities decodes 495,000 stored rows. |
| A latest-vintage sidecar per table | No new structure | Every commit, fold, erasure and sync keeps a second file in step. |

Revisit: a random-key vintage table declaring no bloom filter whose landings decode its history.

## column-records-commit — A column records the commit that first landed it

On a table declaring `available_time`, the instant is the batch's earliest availability, so a late backfill keeps its fields visible at the horizons its rows answer.

| Option | Lost on | Cost |
| --- | --- | --- |
| A per-column availability instant in `schema.json` *(chosen)* | — | A schema written before the field exists reads every column as always present. |
| The commit instant alone | Backfill | A 2026 backfill hides every column of its 2014 rows. |
| Scan the earliest run carrying the column | Cost | Every horizon session reads run manifests to find a schema fact. |

## snapshot-corpus-declared — A snapshot corpus is a declared table pinned by snapshot id

A capture uses the ordinary write path. An unscored late capture never answers a horizon read.

| Option | Lost on | Cost |
| --- | --- | --- |
| Declared columns, pinned snapshot, zero threshold *(chosen)* | — | A late capture needs a judged score of 0 before any horizon read sees it. |
| A per-case leak threshold | Default safety | A case author can admit a partly leaked page. |
| Consumer-written leak rows beside the corpus | One home | The read joins a second table to decide admission. |

## adjustment-factors-apply — Adjustment factors apply at read time, back from each factor's effective date

A measurement table keeps raw values. Under `available_as_of`, each listed column multiplies by the factors available by the bound that take effect after the row's valid-time `from`.

| Option | Lost on | Cost |
| --- | --- | --- |
| Raw storage, factors applied per read *(chosen)* | — | Each bounded read joins the factor table. |
| Store corrected values | Point-in-time | A historical value encodes a correction published after its availability instant. |
| One adjusted copy per factor vintage | Storage | Every factor revision rewrites the measurement history. |

## push-tombstones-owned — A push tombstones only the owned keys its store held

Ownership reads the node id off a key, so two disks pushing under one id each lack the other's runs. A replacement host keeps its node's history with no liveness service.

| Option | Lost on | Cost |
| --- | --- | --- |
| A local record of held keys *(chosen)* | — | One dot-file per store, rewritten each push, sized by the node's keys. |
| Refuse a push under a node id another live disk holds | Replacement hosts | Telling a live sibling from a replaced host needs a node lease. |
| Every owned key a store lacks | Sibling disks | One disk erases every run the other pushed. |

Revisit: two disks under one node id overwriting one run state.

## lost-commit-round — A lost commit round counts only when no newer generation landed

In lockstep each commit round has one winner, so a fixed bound of 5 rounds exhausts every writer past the fifth.

| Option | Lost on | Cost |
| --- | --- | --- |
| Progress-counted retries under a round ceiling *(chosen)* | — | A writer beaten every round spends up to 64 rounds before it refuses. |
| A fixed bound on lost rounds | Busy buckets | Eight lockstep writers leave three refused. |
| A wall-clock bound | Reproducibility | The outcome depends on bucket latency. |

Revisit: deployments whose concurrent writers exceed 64.

## cold-store-boots — A cold store boots on metadata and fetches parts on first read

A container booting on an empty disk downloads every bucket object, and every deploy, eviction or scale-out repays it. The read face stays network-free.

| Option | Lost on | Cost |
| --- | --- | --- |
| Whole-part fetch into a bounded cache *(chosen)* | — | A first read of a cold table waits one object fetch per part. |
| Byte-range reads under the engine | Network-free read face | A range layer beneath the engine; the guard refuses `httpfs`. |
| Read-only bucket mount | POSIX write path | A privileged container; mount latency on every read. |

Revisit: cold-table first reads past the wait bound under ordinary bucket latency.

## bucket-collect — Collection reclaims a superseded snapshot's parts and keeps its manifest

A reader counts a run as folded only by its key in some chain snapshot's `includes_runs`, so a chain missing a folding manifest reads a returning copy of that run twice. Collect therefore keeps every snapshot's `_manifest.json` and reclaims its parts, sidecars and the run directories it folds. The bucket pointer's chain changes only at its head: `publish` drops a link only younger than the pass that drops it, so a link older than the window never moves. Collects serialize on a bucket lease, and a synced hold reaches the bucket before its receipt, so no collect records a held build; one recorded anyway keeps its objects. A record precedes deletion, which waits for retained generations, and lapses with its object; marker and chain then keep the key out.

| Option | Lost on | Cost |
| --- | --- | --- |
| Reclaim parts and folded runs, keep each `_manifest.json` *(chosen)* | — | One manifest per fold stays, every read walks the chain, a push holding a reclaimed run reads its chain, and every node upgrades before the first collect. |
| Unlisted objects and old generations only | Storage | Superseded snapshot directories and folded runs grow without bound. |
| Delete superseded snapshots and folded runs whole | Correct rows | A node offline past the record's age, or a restored disk, reads a folded run its chain no longer names. |
| An operator deletes by hand | Torn reads | A deletion beside a live push leaves a listed key with no object. |

Revisit: a chain walk dominates a cold read, or kept manifests a bucket's bytes.

## folded-index — A snapshot carries every run its chain folds

Kept manifests make a chain whole, and a read walking it grows with history. Each pass writes the set of folded runs as Parquet segments beside its snapshot, carrying its parent's by reference and merging 4 segments of one tier into one, so a pass writes few rows and a reader reads one snapshot. A reader trusts the set only where it names its own snapshot and every segment matches its digest, and otherwise walks, so a stale or damaged set costs time, never rows. A pass whose parent's chain holds no valid set and ends at a missing manifest writes none. The set raises only the minor, `1.1` on a self-contained snapshot and `2.1` on one carrying parts by reference, so an older reader of the same major ignores it and walks the kept manifests. A collect by such a reader deletes the segments, and reads walk until the next pass writes the set again.

| Option | Lost on | Cost |
| --- | --- | --- |
| Tiered Parquet segments carried by reference *(chosen)* | — | A segment per tier stays per snapshot, and retention keeps the carried ones. |
| One JSON file of every folded run per snapshot | Write cost | Each pass rewrites every run the chain ever folded. |
| A `commit_seq` high-water mark | Correct rows | A run merged from another node, or one carrying no `commit_seq`, falls outside the range. |

Revisit: reading a snapshot's segments dominates a cold read.

## run-digest — A folded run replays by the digest of its landing

A collection removes a folded run's manifest and parts, so a retry after a lost acknowledgement finds no manifest to compare its rows against, and a new landing reusing the key reads as folded and its rows vanish. Each landing records the SHA-256 of its producer rows, column types and position, and the folded-runs row carries it beside the run's row count. A re-landing whose key the folded runs name replays on an equal digest and conflicts otherwise. A row without a digest, or a key only a walked snapshot names, conflicts, since nothing tells a retry from a reuse. A staged commit records none: its rows never sit in memory together. A receive attempt whose run row never closed settles with the folded row's count.

| Option | Lost on | Cost |
| --- | --- | --- |
| Digest on the manifest and the folded-runs row *(chosen)* | — | One hash per landing, and a read of the pointer's snapshot and its index per landing; a session validates each index once. |
| Keep every folded run manifest | Collection | Manifests grow with history again. |
| Read a folded key as a replay | Correct rows | A new batch reusing a key folds into nothing. |

Revisit: the per-landing index read dominates a high-rate one-shot landing path.

## index-walk — Collect, push and a lazy pull read the head's index instead of the husks

Every collected snapshot keeps its manifest, so a walk from the pointer reads one manifest per snapshot the table ever held, and a cold pull downloads them all. The head's folded-runs index already names every run the chain folds, and the snapshot folding it. A lazy pull leaves each husk's manifest in the bucket once the head's index reads valid at home, and a walk fetches one only where no valid index answers. Collect walks the retained snapshots down to the first collected parent and takes older runs from the index, the oldest folding snapshot reading as the root. A push reads the manifest of the snapshot folding each candidate. A table holding a hold record walks whole, since a hold keeps a snapshot past any parent.

| Option | Lost on | Cost |
| --- | --- | --- |
| Read the head's index, fetch a husk on a fallback walk *(chosen)* | — | A full walk on a lazy store fetches each husk once; a snapshot folding no run is never reclaimed past the stop. |
| Keep pulling every husk | Cold start | A cold pull grows with history. |
| A list of chain ids on the pointer | Pointer size | The pointer grows with history, and every commit rewrites it. |

Revisit: a full walk on a lazy store runs often enough that fetching husks dominates its cost.

## unpushed-run-marked — An unpushed run is marked in the execution database

On a disposable disk a run commits locally before its push, so a failed push leaves the run on a disk the next release discards. The execution database is already durable.

| Option | Lost on | Cost |
| --- | --- | --- |
| A mark on the execution-database run row from open *(chosen)* | — | One query per command start; a run killed before commit turns `lost` unread. |
| A mark written after the local commit | Kill window | A process killed between commit and mark leaves a committed run unreported. |
| A marker object in the bucket | Failure correlation | The write recording the failure goes to the bucket that refused the push. |

## folded-runs-on-demand — A lazy store fetches folded run manifests when a reader lists runs

Within `retain_runs`, every run the pointer's snapshot folds keeps its `_manifest.json` in the bucket, so a cold pull grows with retained runs rather than with tables. An unbounded read never resolves a folded run; an `as_of` read before that snapshot, and every reader listing a table's runs, does.

| Option | Lost on | Cost |
| --- | --- | --- |
| Skip folded manifests; listings and earlier `as_of` reads fetch them through the part source *(chosen)* | — | A first listing of a cold table waits one fetch per folded run; a store without a part source refuses those reads. |
| Keep folded manifests eager | Boot cost | A cold pull waits for every retained run manifest. |
| Collect folded manifests at fold | `as_of` history | A read bounded before the snapshot loses the runs it folded. |

Revisit: a reader whose first listing of a cold table passes the miss-wait bound under ordinary bucket latency.
