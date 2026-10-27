# A-run — Runs, pipelines and derive decisions

**Status:** accepted

## disposable-execution-state — Disposable execution state shares one remote recovery database

A disposable disk loses paid-call records, callbacks and pending exports, so execution replacement needs every recorded value in one remote database behind the existing storage ports.

| Option | Lost on | Cost |
| --- | --- | --- |
| One remote execution database behind existing ports *(chosen)* | — | Every recorded operation crosses the database boundary; the operator owns database availability and a coordinated corpus backup. |
| Local execution files beside remote corpus objects | Replacement safety | A disposable disk loses paid-call records, callbacks and pending exports. |
| Independently restored execution stores | Consistency | A restored owner can name a missing recorded value or an acknowledged outbox. |

## d1-execution-state — D1 execution state uses a private binding and durable callback guards

D1's batch transaction ends with the request, so it holds no database lock across a provider call. A durable guard row blocks transfer of its callback's rows until release; expiry could admit a second writer while the effect is ambiguous, and a key alone proves no deduplication.

| Option | Lost on | Cost |
| --- | --- | --- |
| D1 metadata, R2 content, per-callback durable guards *(chosen)* | — | A Worker bridge and explicit reconciliation of R2 objects and of guards over unproven effects. |
| PostgreSQL behind Hyperdrive | Native D1 profile | An external database remains mandatory. |
| D1 pre-dispatch checks alone | Fencing | Owner transfer can race between the check and the external call. |
| One global guard | Availability | One orphan blocks every execution. |

## d1-coordinated-groups — D1 coordinated groups commit through versioned sender rows

The sender's state machine stays; only its persistence changes. A batch's first statement takes a fresh write token, and each later statement predicates on it, so a lost race changes no row.

| Option | Lost on | Cost |
| --- | --- | --- |
| Versioned sender rows, token-chained batch, R2 overflow *(chosen)* | — | Every acknowledgement rewrites the whole sender; state past the staging bound refuses. |
| One row per event in D1 | Statement count | A group's events exceed one batch's statements, so staging is no longer one commit. |
| Machine-local SQLite ledger with R2 sync | Replacement safety | A crash between commit and sync forgets an acknowledged group. |

## registered-bodies-pin — Registered bodies pin the executing host and public semantic dependencies

Configuration names registered code and cannot authenticate a digest supplied beside that name. Credential values have no field in provider semantics.

| Option | Lost on | Cost |
| --- | --- | --- |
| Verified host artifact plus declared public semantics *(chosen)* | — | Any host rebuild refuses pending work unless the recorded build remains available. |
| Manifest-supplied implementation digest | Authority | Untrusted configuration can keep a digest while replacing code. |
| Executable digest alone | Dependency identity | Runtime prompt, model or policy changes escape the pin. |

## plan-compiled-content — A plan is compiled, content-hashed data with no embedded runtime

A journaled step replays correctly only against the definition that produced it. Data selects a declared arm and never which arms exist.

| Option | Lost on | Cost |
| --- | --- | --- |
| Compiled, content-hashed plan *(chosen)* | — | Every definition change is a compile; an expression no connector offers needs a new connector. |
| An embedded script runtime for step bodies | Determinism and footprint | Replay runs a different definition, and every profile carries tens of megabytes of runtime. |
| Row growth inside the chain | Journal granularity | Output stops mapping onto input, breaking cursor and dedup guarantees. |

## incremental-position-bound — An incremental position is bound to a named field and a seed is bounded

A cursor value means nothing without the field it was measured on.

| Option | Lost on | Cost |
| --- | --- | --- |
| Bind the position to its field; bound and fingerprint the seed *(chosen)* | — | A field rename or an export top-up needs an explicit reset. |
| Reset the position silently on a rename | Silent wrongness | A window re-lands or is skipped with no record. |
| Resolve cursor copies last-write-wins | Record skipping | A stale machine moves the position backwards or past unread rows. |

## external-signals-single — External signals are single-valued catalog writes

A signal's value is fixed once observed, so replay reads what the first resumption saw. Deadlines evaluate lazily, so no target owes a durable timer.

| Option | Lost on | Cost |
| --- | --- | --- |
| Catalog write, first value wins, sticky transitions, named mismatch *(chosen)* | — | A payload correction needs a new suspension; stop latency is one poll interval. |
| Last write wins on a token | Single-valuedness | Replay reads a payload the first resumption never saw. |
| An eager timer or control socket | Footprint | Every deploy target owes a durable timer and a second channel. |
| Fold cancellation into failure | Downstream health | Deliberate stops fill the failure signal and hold models at last-good. |

Revisit: a deploy target where the requesting process and the run share no catalog; the poll's catalog read becomes measurable under high fire concurrency.

## derive-work-set — The derive work set is an anti-join against marker rows

The tier's own output table is the single record of what is done, failed or settled, and each tick recomputes the work set from it.

| Option | Lost on | Cost |
| --- | --- | --- |
| Anti-join each tick against output rows and in-table markers *(chosen)* | — | The scan is linear in the parent table; every consumer filters marker rows. |
| A watermark over the parent table | Stranding | A gap passed once stays underived. |
| Permanence encoded as an attempt count | Permanence | Raising the ceiling revives every security refusal. |

Revisit: the parent scan dominates tick cost on a real archive; a re-derive signal for changed parent rows becomes necessary.

## derivation-key-decides — A derivation key decides whether a settled unit is current

An engine id alone never re-derives a corrected parent row. A unit's old rows answer until its new rows or marker commit, so no read sees an empty unit.

| Option | Lost on | Cost |
| --- | --- | --- |
| Derivation key compared each tick *(chosen)* | — | Every consumer filters to the latest key; each tick hashes parent rows. |
| Compare the engine id alone | Freshness | A corrected parent row never re-derives. |
| Delete the unit's rows, then re-derive | Empty reads | The unit answers nothing until the engine returns. |

Revisit: hashing parent rows dominates tick cost on a real archive.

## derived-unit-follows — A derived unit follows its parent's bytes and lifetime

A media path can stay constant while its bytes change, and a deleted parent leaves derived rows in another table. Freshness and no orphaned citations decide.

| Option | Lost on | Cost |
| --- | --- | --- |
| Byte digest at selection and parent-aware fold *(chosen)* | — | Every local selection reads file bytes; a fold reads the parent key set. |
| Key on the path alone | Freshness | Changed media under one path keeps its old passages. |
| Delete rows as soon as a parent write lands | Commit isolation | A parent commit edits another table and every sidecar. |

## passage-s-modality — A passage's modality names its output

Output kind decides retrieval and sidecar selection. A passage keeps its source medium through its parent link.

| Option | Lost on | Cost |
| --- | --- | --- |
| Label the output passage *(chosen)* | — | Readers seeking the source medium follow the parent reference. |
| Copy the parent's medium | Retrieval type | A transcript would claim to be a video row. |
| Add a video value to the output domain | Type fidelity | A text-only passage would be routed as video. |

## chained-derive-pipelines — Chained derive pipelines follow their source tables

Declaration order and independent schedules give no landing order. One tick carries a new parent row through its children.

| Option | Lost on | Cost |
| --- | --- | --- |
| Infer dependencies from source tables and run parent-first *(chosen)* | — | A tick serializes dependent runs and reports a parent failure beside child work. |
| Require authors to set `after` on each child | Completeness | A missing manual edge leaves a child one tick behind. |
| Let each derive pipeline run independently | Freshness | Child selection can precede its parent's landing. |

## selection-stays-bounded — Selection stays bounded and countable before a tick pays

Selection memory stays independent of archive size, and the outstanding count is known before any engine spend.

| Option | Lost on | Cost |
| --- | --- | --- |
| In-memory up to a row ceiling, streamed above it; a counting dry run *(chosen)* | — | Two selection paths agree on one anti-join; the ceiling is a chosen figure. |
| Always scan in memory | Memory bound | A large archive exhausts the process before the first unit runs. |
| No dry run; read the run record after | Cost control | The first tick spends engine quota before anyone sees its size. |

## derive-engine-machine — A derive engine is a machine-bound argv child with a cleared environment

A manifest requests an engine by name; the machine's configuration defines what that name executes. Row data never becomes syntax.

| Option | Lost on | Cost |
| --- | --- | --- |
| Machine-local binding, argv child, cleared environment, group kill *(chosen)* | — | A pipeline is not self-contained; the operator enumerates every variable a tool reads. |
| Command carried in the manifest | Who may introduce a command | Any manifest author, including an agent, gains argv execution. |
| A shell string, or escaped row data | Data becoming syntax | A filename containing `$()` reads as command text. |

Revisit: operators routinely wrap steps in pinned scripts to recover shell features.

## nested-arrow-canonical — Nested Arrow is canonical and the relational shape is a reversible projection

Normalizing once in host code serves any number of sinks. The list index makes shredding reversible.

| Option | Lost on | Cost |
| --- | --- | --- |
| Nested canonical, relational as a late reversible projection *(chosen)* | — | A flat-only sink pays a downgrade on every write; a root-table filter commits parentless children. |
| Relational as the storage default | Reversibility | Rebuilding nesting permutes lists, and two sinks normalize twice. |
| Nested canonical with an optional list index | Reversibility | The loss surfaces only as a silent permutation. |

Revisit: orphaned children from root-level filtering become a reported data-quality problem; a deployment is relational at every sink.

## table-commit-owns — A table commit owns its schema-diff evidence

The reconciled schema describes only the latest shape, and a live projection can drop events. The commit marker publishes rows, cursor and schema evidence together.

| Option | Lost on | Cost |
| --- | --- | --- |
| Events in each table's run manifest *(chosen)* | — | Run history reads table manifests to answer schema changes. |
| Events in the terminal run row | Commit coupling | A table can commit before the terminal row exists. |
| Events in the live projection | Durability | A full observer channel drops the explanation. |

## secret-guard-has — The secret guard has one host-owned catalogue

A manifest edit cannot change what the host considers a secret. A catalogue change carries code and a fixture review.

| Option | Lost on | Cost |
| --- | --- | --- |
| Host-owned catalogue and mask-only posture *(chosen)* | — | A new vendor prefix requires a host release; an unrecognized shape can pass. |
| Deployment extensions | Testable coverage | A local prefix can mask ordinary data without the catalogue fixture. |
| Strict pull refusal | Ingest continuity | One matched cell strands an entire source batch. |

Consequences: a prefix-only scan conflates long hyphenated public identifiers with credentials, so bare candidates take a narrower grammar than tagged families.

## decoder-child-owns — A decoder child owns one input and two bounds

The bounds protect the serving process while one input fails by name. A child carries no parser state into another input.

| Option | Lost on | Cost |
| --- | --- | --- |
| Fresh bounded child per input *(chosen)* | — | Process startup per input; an unusually large input fails. |
| Long-lived extractor | Failure isolation | A crash loses the extractor's other in-flight inputs. |
| In-process decoder | Serving isolation | A fatal parser fault ends the serving process. |

## observed-batch-fixes — An observed batch fixes its landing types

A source without schema discovery still runs. A source reporting no rows offers no guessed columns.

| Option | Lost on | Cost |
| --- | --- | --- |
| Type each batch at staging *(chosen)* | — | A late schema mismatch spends the pull before refusal. |
| Require source schema during planning | Connector breadth | A source without discovery cannot run. |
| Fix types from the first batch alone | Compatibility | A later type change lands under a stale guess. |

## run-substrate-storage — The run substrate is three storage ports, and the file tree is one adapter

A host keeping its own transactional store would otherwise hold replay state twice, the second copy outside its retention and export. One home for replay state per host decided it.

| Option | Lost on | Cost |
| --- | --- | --- |
| Three storage ports, file tree as default adapter *(chosen)* | — | Every adapter runs one conformance suite. |
| The file backend with a movable root | One home for replay state | Recorded values persist as plaintext beyond the host's deletion and export. |
| One combined port for journal, blobs and awakeables | Adapter reuse | An object store backs blobs only by faking a journal. |

## blob-put-may — A blob put may wait on its adapter's write lock

SQLite holds one write lock per file, so its put waits behind another connection's write transaction. One transactional home for journal rows and blobs decided it.

| Option | Lost on | Cost |
| --- | --- | --- |
| Narrow `blob-write` to convergence without error *(chosen)* | — | A put stalls behind a foreign write transaction on the file. |
| Blobs as staged files beside the SQLite file | One transactional home | A blob and its referencing row commit apart. |
| A put that skips an existing hash without a write | Sweep grace | A re-put renews no age, so the sweep takes a blob a new row is about to name. |

Revisit: a host runs a long write transaction on the machine file beside the run stores.

## host-opens-execution — A host opens an execution through one handle keyed on a declared scope

A derive step or host job would otherwise restate owner claim, pin check and retirement. One implementation of those rules decided it.

| Option | Lost on | Cost |
| --- | --- | --- |
| One handle over a scope-keyed owner *(chosen)* | — | One owner-table migration per catalog adapter. |
| Hosts compose journal and keeper directly | One implementation of owner rules | Every host restates claim, pin and retirement, and each copy drifts. |
| A host job as a synthetic pipeline and table | Honesty of the record | Run history lists tables that hold no rows. |

## superseded-owner-retires — A superseded owner retires by operator verb or applied declaration, never at open

A pending owner pinned to a plan no declaration produces refuses every fire. No recorded work is discarded without an operator decision or a declaration that cannot resume it.

| Option | Lost on | Cost |
| --- | --- | --- |
| Operator verb plus applied retirement of superseded owners *(chosen)* | — | Apply reads the run catalog; plan and sweep jobs recover through the verb alone. |
| Re-release the superseded declaration | Recovery without a revert | Every declaration change waits on interrupted executions. |
| Retire at open on any pin mismatch | Operator decision | A transient pin drift discards recorded paid calls. |

## unclosed-execution-recovers — An unclosed execution recovers by lease takeover, and one keeper per engine renews leases

A crashed process runs no destructor, so a status written on drop covers only the clean exit and one event reads two ways.

| Option | Lost on | Cost |
| --- | --- | --- |
| Drop records nothing; lease takeover; one keeper per engine *(chosen)* | — | A dropped handle holds its scope until the lease lapses; a stalled renewal delays the rest. |
| Drop records `failed` | One outcome per event | A crash and a drop close differently. |
| A keeper thread per execution on a 10 ms tick | Wakeup cost | 20 open executions spend 20 threads and 2,000 wakeups a second. |

Revisit: a renewal lags measurably under a large deadline heap.

## host-registered-derive — A host-registered derive task is compiled code, versioned into the derivation key

Configuration selects only engine-named code, and a task version change re-selects every settled unit. A unit spanning several tables is whole once its marker commits.

| Option | Lost on | Cost |
| --- | --- | --- |
| Registered compiled task, version in the key, marker last *(chosen)* | — | A new task is a host release; retained versions occupy the table. |
| A task as a command in configuration | Who may introduce code | A manifest author gains execution. |
| Task version outside the derivation key | Freshness | A version bump leaves every settled unit stale. |

## model-declared-sql — A model is a declared SQL table whose build publishes through its snapshot manifest

A reader comparing two figures needs the build each came from. No representable torn publication decided it.

| Option | Lost on | Cost |
| --- | --- | --- |
| Snapshot of a store table, section in its manifest *(chosen)* | — | A refused build leaves no committed trace. |
| A `view` key on the table block | Read cost | Every read pays the rollup, and no build id exists to echo. |
| Build metadata in a side file beside the snapshot | Torn publication | A crash publishes data with another build's identity. |

## coordinated-exports-activate — Coordinated exports activate a complete immutable group in a receiver namespace

Output tables commit independently, and a downstream projection needs one completed frontier across every member. Atomic visibility and stale-replay refusal decide.

| Option | Lost on | Cost |
| --- | --- | --- |
| Immutable group barrier and fresh rebuild namespace *(chosen)* | — | The receiver retains accepted delivery identities. |
| Independently activate each member | Atomic visibility | Interruption exposes mixed generations. |
| Reset an existing export cursor | Recovery safety | An older checkpoint can reverse tombstones or reuse identities. |

## sweep-freezes-transaction — A sweep freezes one transaction-time bound and carries state through its own outputs

Availability time separates origins; transaction time fixes the corpus. Origins then differ only in their availability bound.

| Option | Lost on | Cost |
| --- | --- | --- |
| One frozen `as_of`, outputs read at the preceding commit *(chosen)* | — | A carried sweep runs serially. |
| Each origin resolves its own `as_of` at open | Comparable origins | A landing between two origins changes one origin's corpus. |
| Transaction time set to the origin | Backfilled history | A history loaded after every origin answers none. |

## build-stale-landings — A build is stale by the landings it has not read

The newest input commit against the clock reads a quiet input stale forever. A rebuild over the current frontier always clears staleness.

| Option | Lost on | Cost |
| --- | --- | --- |
| A sequence frontier per input *(chosen)* | — | A run merged below `through` reads stale until a rebuild. |
| The newest commit instant against the clock | Clearable | A quiet input reads stale, and no rebuild fixes it. |
| A single-instant watermark | Comparability | Writers commit out of stamp order, so an instant names no landing set. |

## minor-contract-bump — A minor contract bump admits only what an old reader still reads

A reader written against an earlier minor of the same major reads every later build unchanged.

| Option | Lost on | Cost |
| --- | --- | --- |
| Semver over the column diff *(chosen)* | — | The check reads the prior schema. |
| Any fingerprint change needs a major | Additive growth | A new nullable column breaks every reader's pin. |
| Any change under a minor bump | Reader safety | A dropped column publishes and its readers fail at query time. |

## orchestrator-role-port — The orchestrator role is a port, and a provider's workflow engine fills it at the unit grain

A step otherwise crosses two engines. One semantics, proved once, holds inside each provider step.

| Option | Lost on | Cost |
| --- | --- | --- |
| Port at the substrate, one provider step per pull or chunk *(chosen)* | — | Two fillers to conform; a step pays a provider round trip. |
| The provider owns replay below the commit pointer | One semantics | Replay differs per target, and the journal proofs cover one target. |
| One provider step per page | Proofs holding on managed targets | A backfill of tens of thousands of pages overflows an instance. |

Revisit: a provider offering a sub-chunk grain.

## awakeable-resumes-credentialed — An awakeable resumes on the credentialed face, beside the worker relay

The relay's deployment key names no caller, so the relay cannot hand the registry a verified subject.

| Option | Lost on | Cost |
| --- | --- | --- |
| The awakeable path on the served face, beside the relay *(chosen)* | — | Two listeners answer one path shape, each for its own tokens. |
| The registry merged into the relay | Verified caller subject | Every awakeable caller holds the worker key, which names nobody. |
| The token as the only credential | Leak resistance | A token copied from a log resumes a run. |

## bound-hit-rides — A bound hit rides its failure and is listed on the run record

An attempt that succeeds after a bound hit otherwise leaves no trace. The list adds no retry layer and no catalog column.

| Option | Lost on | Cost |
| --- | --- | --- |
| A named bound on the failure, listed on the row *(chosen)* | — | Each hit rewrites the run row once. |
| The run's error columns alone | Recovered hits | A hit followed by a successful attempt leaves nothing. |
| One projection event per hit | Durability | A restart forgets it. |

## meter-reservation-holds — A meter reservation holds no wait of its own

A blocking HTTP acquire cannot select on a run's cancellation token, so the retry sleep performs the wait.

| Option | Lost on | Cost |
| --- | --- | --- |
| A bounded call, the wait in the retry sleep *(chosen)* | — | A stop waits out at most the owed report and one acquire. |
| An interruptible transport selecting on the token | Blocking transport | An async client inside a blocking adapter, saving at most one call. |
| A sleep on denial inside the reservation | One wait per retry | A second sleep stacked on the schedule. |

## batch-key-answers — A batch key answers within a declared retention window

A key answering forever promises deduplication the store cannot bound. No default chooses a window for the sender.

| Option | Lost on | Cost |
| --- | --- | --- |
| A declared window, a late key refused *(chosen)* | — | A late resend needs a new key. |
| A late key lands as a fresh batch | No row lands twice | A resend after the window duplicates committed tables. |
| A default window | Sender choice | The engine picks a retry horizon it cannot know. |

## received-batches-spooled — Received batches spooled behind a landing commit as one group

A batch landing its own run pays a commit, a part and a push. A malformed body fails no other batch.

| Option | Lost on | Cost |
| --- | --- | --- |
| Leader lands what spooled behind the lock *(chosen)* | — | A lone writer's synced spool costs about a quarter of its throughput; a refused group lands twice. |
| A linger timer | Latency | A lone writer waits out the timer on every batch. |
| One run per batch | Throughput | Commits, parts and pushes grow with batches. |

## terminal-item-s — A terminal item's outcome is a row of a reserved relation

An isolated item's run ids are digests no reader inverts, and its receipt sits in a catalog cursor no read face exposes.

| Option | Lost on | Cost |
| --- | --- | --- |
| Reserved `_items` relation appended on receipt commit *(chosen)* | — | A crash between receipt commit and append leaves that item without a row. |
| Job and entity fields on the attempt's `_runs` row | One row per run phase | A per-entity question folds every attempt's rows. |
| Append before the receipt commits | No false terminal | A refused commit leaves a row claiming an unreached outcome. |

## backfill-commits-checkpoints — A backfill commits at checkpoints and walks one page per pull

A backfill window outgrows one commit's staging bound, and a watermark walk outgrows one pull. A next-URL walk names pages by URLs the journal never holds, so it commits once.

| Option | Lost on | Cost |
| --- | --- | --- |
| Checkpoints for position-addressed sources *(chosen)* | — | A run publishes several commits; a vendor unordered by its clock loses rows a checkpoint passed. |
| One commit under a 1 GiB staging bound | Completion | A larger window fails every fire. |
| A next-URL position counting pages | Correctness | A resumed walk re-follows a moved listing and duplicates or skips a row. |

## run-record-folds — The run record folds inside its own append

Nothing else folds `_runs`, so its listing, every watermark and every push grow by run. A run's row never fails on the record's upkeep.

| Option | Lost on | Cost |
| --- | --- | --- |
| A pass inside the append, its failure dropped *(chosen)* | — | The append that fires the trigger pays one pass and its collection. |
| Operator `context compact _runs` alone | No operator command | The record grows until someone runs it. |
| A scheduled pass in `serve` | Every writer folds | A one-shot `run` never folds its record. |

## iceberg-overwrite-commit — An Iceberg export replaces its table through a hand-written overwrite commit

The Iceberg library on the workspace's Arrow major exposes `fast_append` but no overwrite, and keeps its commit builder and action trait crate-private. A reader names one table, so each export replaces the rows under that name. Name continuity and the dependency graph decide.

| Option | Lost on | Cost |
| --- | --- | --- |
| Overwrite built from the library's public manifest writers and the REST table update *(chosen)* | — | About 150 lines no library upgrade maintains; a commit costs about 3 s at the catalog, flat from 1 to 4,000 files. |
| Wait for a library release with overwrite | Availability | No released version offers one. |
| Append into a fresh table per export | Name continuity | REST has no atomic swap; a drop then rename leaves a window with no table. |
| Append every version and let readers deduplicate | Reader semantics | Every query carries a deduplication its writer must know. |

## iceberg-audience — An Iceberg namespace serves one credential, and caller SQL needs a single-audience warehouse

A reader of the export runs outside the read face, so the grant applied at export time is the only one it holds. A catalog query token reads every namespace in its warehouse. Isolation decides.

| Option | Lost on | Cost |
| --- | --- | --- |
| One credential per namespace; caller SQL only on a single-audience warehouse *(chosen)* | — | A warehouse per audience that writes its own SQL; fixed query templates for the rest. |
| One shared table filtered in the route | Isolation | One route defect exposes every audience's rows. |
| Caller SQL over a shared warehouse | Isolation | The caller names another audience's namespace and the token admits it. |
