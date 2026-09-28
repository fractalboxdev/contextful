# A-run — Runs, pipelines and derive decisions

**Status:** accepted

## A plan is compiled, content-hashed data with no embedded runtime

A journaled step replays correctly only against the definition that produced it. The authoring surface is a build-time compiler emitting a serialized, content-hashed plan, a run pins against the plan hash, and no profile links a script runtime. `run.compile` accepts a step body only as a connector reference, and control flow is declared as `branch` and `parallel` nodes, so data selects an arm and never which arms exist. `run.transform` never emits more rows than it consumed.

| Option | Lost on | Cost |
| --- | --- | --- |
| Compiled, content-hashed plan *(chosen)* | — | Every definition change is a compile; an expression no connector offers needs a new connector. |
| An embedded script runtime for step bodies | Determinism and footprint | Replay runs a different definition, and every profile carries tens of megabytes of runtime. |
| A restricted expression language evaluated at run time | Determinism | The replayed definition is whatever evaluates on replay. |
| Pipelines as Rust code in the tree | Authoring ergonomics | Every agent-authored definition is a source change and rebuild. |
| Row growth inside the chain | Journal granularity | Output stops mapping onto input, breaking cursor and dedup guarantees. |

Consequences: row-expanding work such as transcription lives in `run.select` and its siblings, reading from the store rather than from a vendor mid-pull.

## An incremental position is bound to a named field and a seed is bounded

A cursor value means nothing without the field it was measured on. `run.advance` stores the field name with each position and refuses, naming both, when the declared field differs; a rename resets explicitly. A row with no orderable clock value, or a stream switching between text and numeric positions, fails the pull terminally and spends no attempt. `store.merge` resolves a cursor by its declared kind, never by recency. `run.seed` requires `primary_key` and an event-time `order_by`, fails a whole chunk on a stamp at or past `below`, and refuses a committed seed whose source fingerprint moved.

| Option | Lost on | Cost |
| --- | --- | --- |
| Bind the position to its field; bound and fingerprint the seed *(chosen)* | — | A field rename or an export top-up needs an explicit reset. |
| Reset the position silently on a rename | Silent wrongness | A window re-lands or is skipped with no record. |
| Resolve cursor copies last-write-wins | Record skipping | A stale machine moves the position backwards or past unread rows. |
| Clamp or drop an out-of-bound seed stamp | Auditability | The engine fabricates event time or loses history. |
| Reload automatically when the fingerprint moves | Cost control | Every touch of the export spends hours and vendor quota. |

Consequences: a source supplying no fingerprint skips the load and warns each run that a top-up goes unnoticed.

## External signals are single-valued catalog writes

Every external signal lands as one catalog write whose value is fixed once observed, and every mismatch is a named outcome. `run.suspend` returns the written value for an identical re-resolution, raises `AwakeableAlreadyResolved` (409) for a different payload, evaluates deadlines lazily against the injected instant and raises `AwakeableTimedOut` (410) late. `run.cancel` writes a requested-at instant raced against every await on a 500 ms poll; a stop matching no pending, running or waiting row raises `CancelTargetNotInFlight`. `canceled` is a terminal status excluded from upstream health.

| Option | Lost on | Cost |
| --- | --- | --- |
| Catalog write, first value wins, sticky transitions, named mismatch *(chosen)* | — | A payload correction needs a new suspension; stop latency is one poll interval; every status classifier carries one more arm. |
| Last write wins on a token | Single-valuedness | Replay reads a payload the first resumption never saw. |
| An eager timer or control socket | Footprint | Every deploy target owes a durable timer and a second channel. |
| Queue an unmatched stop, or answer empty success | Legibility | A stale request halts an unrelated fire, or an operator waits on an unmarked run. |
| Fold cancellation into failure | Downstream health | Deliberate stops fill the failure signal and hold models at last-good. |

Consequences: an unpolled suspension stays nominally pending until something reads it.
Revisit: a deploy target where the requesting process and the run share no catalog; the poll's catalog read becomes measurable under high fire concurrency.

## The derive work set is an anti-join against marker rows

The tier's own output table is the single record of what is done, failed or settled, and the work set is recomputed from it every tick. `run.select` reads the parent table and drops every parent already holding a content row or a settled marker before the row cap. `run.emit` records a unit that produced nothing as one marker row with status, attempts, last error and `retryable`. Status separates `ok`, `empty`, `unavailable` (the default) and `failed`; `retryable = false` settles a unit whatever the attempt ceiling.

| Option | Lost on | Cost |
| --- | --- | --- |
| Anti-join each tick against output rows and in-table markers *(chosen)* | — | The scan is linear in the parent table; every consumer filters marker rows; two columns carry one unit's fate. |
| A watermark over the parent table, with or without a backfill window | Stranding | A gap passed once stays underived. |
| A dead-letter queue or sibling marker table | Visibility to the anti-join | Failed units are re-selected every tick. |
| One empty status for every silence | Honesty of landed values | A bot check lands as a statement about the recording. |
| Permanence encoded as an attempt count | Permanence | Raising the ceiling revives every security refusal. |

Consequences: a marker written with no permanence value re-attempts its unit once.
Revisit: the parent scan dominates tick cost on a real archive; a re-derive signal for changed parent rows becomes necessary.

## A derive engine is a machine-bound argv child with a cleared environment

A manifest requests an engine by name; the machine's configuration defines what that name executes, and row data never becomes syntax. `run.bind` refuses an unbound name or a command key in a manifest; a `[derive.<name>]` block states argv chain or host list, environment allowlist, pins and bounds, and the adapter, not the operator's zone key, declares locality. `run.exec` spawns an argument array with no shell, a cleared environment plus allowlist, and wall-clock and output bounds, killing the process group on deadline.

| Option | Lost on | Cost |
| --- | --- | --- |
| Machine-local binding, argv child, cleared environment, group kill *(chosen)* | — | A pipeline is not self-contained; pipes become chain steps; the operator enumerates every variable a tool reads. |
| Command carried in the manifest | Who may introduce a command | Any manifest author, including an agent, gains argv execution. |
| A shell string, or escaped row data | Data becoming syntax | A filename containing `$()` reads as command text. |
| Inherit the environment minus a denylist | Ambient reach | Every daemon credential reaches every spawned tool. |
| Operator-declared locality as the row value | Recoverability of placement | A config edit relabels a vendor engine as on-device. |

Consequences: a manifest half is inert on an unprepared machine; a long legitimate chain dies at its deadline.
Revisit: operators routinely wrap steps in pinned scripts to recover shell features.

## Nested Arrow is canonical and the relational shape is a reversible projection

`run.normalize` holds the canonical form as nested Arrow structs and lists, normalized once in host code whatever the number of sinks. Relational shredding is a late projection at the sink: a struct flattens into parent-child column names, a list shreds into a child table joined by foreign key, and every child row carries its list index. A child table emitted without that index raises `PipelineListIndexMissing`, naming parent and list.

| Option | Lost on | Cost |
| --- | --- | --- |
| Nested canonical, relational as a late reversible projection *(chosen)* | — | A flat-only sink pays a downgrade on every write; a filter binding the root table lands parentless children. |
| Relational as the storage default | Reversibility | Rebuilding nesting permutes lists, and two sinks normalize twice. |
| Raw JSON, shred nothing | Queryability | Every query re-infers types, too late for a schema-diff event. |
| Nested canonical with an optional list index | Reversibility | The loss surfaces only as a silent permutation. |

Consequences: `native` keeps nesting up to the sink's declared capability and emits a downgrade schema-diff event past it.
Revisit: orphaned children from root-level filtering become a reported data-quality problem; a deployment is relational at every sink.

## The run substrate is three storage ports, and the file tree is one adapter

Status: accepted. The journal, its blobs and the awakeable registry write the file tree directly, so a host keeping its own transactional store holds replay state twice, the second copy outside its retention and export. Three ports in the domain package carry the substrate: a journal store (create pending, read, replace if pending, record, release, rows and retire per execution), a blob store (put by sha256, get, sweep over a reference set and a grace) and an awakeable store. The file tree is the default adapter. Every `run.journal` and `run.suspend` clause holds per adapter, so `blob-write` states converging writers, and a staged rename is the file adapter's means.

Criteria: one home for replay state per host, which decided it; an I/O-free domain package; one conformance suite per port.

| Option | Lost on | Cost |
| --- | --- | --- |
| Three storage ports, file tree as default adapter *(chosen)* | — | Every adapter runs one conformance suite; the sweep takes the union of journal and awakeable references. |
| The file backend with a movable root | One home for replay state | Recorded values land again as plaintext beyond the host's deletion and export. |
| One combined port for journal, blobs and awakeables | Adapter reuse | An object store backs blobs only by faking a journal. |
| Ports over lock, rename and exclusive create | I/O-free domain | Claim logic stays bound to filesystem semantics. |

Consequences: a journal store apart from the catalog shares no transaction with owner retirement, which `run.journal` leaves unsettled.

## A blob put may wait on its adapter's write lock

Status: accepted. `blob-write` asks concurrent writers of one hash to converge without waiting, which the file adapter meets with a staged rename. SQLite holds one write lock per file, so the SQLite adapter's put waits behind any other connection's write transaction, and a lock held past the busy wait fails it. `blob-write` therefore requires convergence and no error between writers of one hash, and leaves waiting to the adapter.

Criteria: one transactional home for journal rows and blobs, which decided it; no failure between converging writers.

| Option | Lost on | Cost |
| --- | --- | --- |
| Narrow `blob-write` to convergence without error *(chosen)* | — | A put stalls behind a foreign write transaction on the file. |
| Blobs as staged files beside the SQLite file | One transactional home | A blob and its referencing row commit apart, and the store is two things to back up. |
| A put that skips an existing hash without a write | Sweep grace | A re-put renews no age, so the sweep takes a blob a new row is about to name. |

Consequences: a foreign connection holding the file's write lock past the busy wait fails every put behind it with a storage failure.
Revisit: a host runs a long write transaction on the machine file beside the run stores.

## A host opens an execution through one handle keyed on a declared scope

Status: accepted. `run_with` is the only way to open an execution and binds it to a native plan, a source pulled to exhaustion, one destination commit and an owner keyed on pipeline and table, so a derive step or a host job restates owner claim, pin check and retirement. The engine opens an `Execution` for a scope, a content-hashed plan reference and pins; the handle records steps, commits a cursor, suspends and closes, and `run_with` is its client with unchanged behavior. The catalog keys owners on a scope: live table, backfill chunk or host-declared id. Each catalog adapter migrates its owner rows, a table owner keeping its key byte for byte.

Criteria: one implementation of owner claim, pin mismatch and retirement, which decided it; pending owners resume across the migration.

| Option | Lost on | Cost |
| --- | --- | --- |
| One handle over a scope-keyed owner *(chosen)* | — | One owner-table migration per catalog adapter; pins generalize to a plan reference plus host-supplied identities. |
| Hosts compose journal and keeper directly | One implementation of owner rules | Every host restates claim, pin and retirement, and each copy drifts. |
| A host job as a synthetic pipeline and table | Honesty of the record | Run history lists tables that hold no rows. |
| A second, host-only owner table | One owner rule | Retirement and marker reconciliation run twice. |

Consequences: `ExecutionPinMismatch` under a host scope names the plan reference in place of the pipeline hash.

## An unclosed execution recovers by lease takeover, and one keeper per engine renews leases

Status: accepted. A crashed process runs no destructor, so a status written when a handle drops covers only the clean exit, and one event reads two ways. Dropping an `Execution` unclosed records nothing: its keeper registration ends, the owner lease lapses and the next open under the scope resumes the pending owner, taking over pending claims. One keeper thread per engine holds a deadline heap of registered executions, renews each lease, feeds each token its poll and sleeps until the earliest deadline; open registers, close or drop deregisters.

Criteria: one outcome per event, which decided it; wakeups scale with deadlines, not with open executions.

| Option | Lost on | Cost |
| --- | --- | --- |
| Drop records nothing; lease takeover; one keeper per engine *(chosen)* | — | A dropped handle holds its scope until the lease lapses; a stalled renewal delays the rest. |
| Drop records `failed` | One outcome per event | A crash and a drop close differently, and a batchless failure releases an owner the replay needs. |
| A keeper thread per execution on a 10 ms tick | Wakeup cost | 20 open executions spend 20 threads and 2,000 wakeups a second. |
| Renewal only at step boundaries | Liveness | A long step reads as an orphan and its claim passes mid-effect. |

Consequences: the stop channel and its cadence stay as `run.cancel` states them.
Revisit: a renewal lags measurably under a large deadline heap.
