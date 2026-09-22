---
contract: run
owns:
  - journal
  - advance
  - suspend
  - retry
  - own
  - cancel
  - record
  - project
---

# Durable runs

A run is the unit of work on the run path. It opens against a compiled plan, pulls from a
source through recorded steps, lands rows through the sequence in `31-pipeline.md`, moves an
incremental position forward, and closes on a terminal status every surface reads. One
executor answers for the journal, retry and cancellation across pipelines and the derive tier.

One run, the durable state around it, and the contracts it meets:

```mermaid
flowchart LR
  PLAN["plan reference · content-hashed"] --> RUN["runner"]
  SRC["source · connector contract"] -->|"batches, pulled"| RUN
  RUN <-->|"record · replay"| J[("journal + blob store")]
  AW["POST /awake/:token"] -->|"resume payload"| J
  RUN --> LAND["land path · 31-pipeline"]
  LAND --> MARK["run commit marker · store contract"]
  MARK --> CUR[("catalog cursor cache")]
  STOP["stop mark on the run row"] -->|"polled every 500 ms"| RUN
  RUN --> REC[("run record · reserved store table")]
  RUN -. "events after durable change" .-> HUB["live projection · wire snapshot"]
  REC -->|"terminal status reconciles"| HUB
  HUB --> SUB["run-stream subscribers"]
```

## journal

Recording a step's value once, resolving it on replay, and collecting what a replay can no longer reach.

- `step-output` — A journaled step's value is recorded exactly once and its effect runs at least once: a crash between effect and write re-enters the effect, and every later call under the key returns the recorded value.
  *because exactly-once effects need the vendor's cooperation, and the recorded value is the one fact a replay can rely on*
- `entry-key` — An entry is keyed by `(execution_id, step_label, input_hash)`. The first caller claims the key with a pending row before entering the effect; a racing caller waits for the recorded value instead of computing.
  *because two racers computing one key issue two billed calls and spend two meter permits*
- `claim-takeover` — A pending claim whose run owner lease has expired, {{run.record.owner-lease}}, is taken over by the next caller under that key.
- `idempotency-key` — Every outbound request a step makes carries an idempotency key derived from its entry key, identical on every re-entry of that effect.
  *because a vendor honoring the key turns an at-least-once effect into one billed call*
- `inline-cutoff` — A value of 1 MiB or smaller is stored inline in its row as bytes; a larger value lands in a content-addressed blob named by its sha256, and the row holds the reference.
- `blob-write` — A blob writer stages a private temporary file and renames it over the destination; concurrent writers of one hash converge on one file without waiting or erroring.
- `missing-blob` — A row whose blob reference resolves to no file raises `BlobMissing` carrying the reference, never an empty value in place of the recorded one.
  *P6*
- `collection` — Retiring an execution owner deletes its journal rows in the retiring transaction.
  *because recorded work is needed only while a replay can reach it, and an uncollected journal grows with total ingest*
- `blob-sweep` — A mark-and-sweep pass every 24 h deletes each blob that no journal row or pending awakeable references and that is older than 1 h.
  *because the grace window covers a blob written ahead of the row that names it*
- `effect-boundary` — A body replays faithfully when every observable side effect passes through a recorded step, a cursor commit or an awakeable; the work between them is pure.
- `recorded-batch` — A journaled pull records the batch as the source handed it over, after {{run.guard-secrets.placement}} and ahead of the land path.
- `replay-lands` — A journal hit hands the recorded batch to the land path, so a resumed run lands bytes identical to an uninterrupted one, batch ordinal included.
- `ledger-settles-first` — The outbound request ledger settles durably before the entry recording its batch commits.
- `redacting-source` — A pipeline declaring write-path redaction over a source that journals its pulls raises `JournalRedactionConflict` at manifest validation and again at run open, before the first pull.
  *A-authority*
- `opt-out` — Pull journaling defaults on. A source whose pre-pull cursor cannot name the content it reads opts out through one constant, pinned against each source's declaration by a test. An empty pull is never journaled.
- `escape-hatch` — Two escape hatches exist and no third: `journal.unsafe(label, effect)` for an idempotent read, and a source declaring that it journals no pull. Each carries its idempotency argument at a greppable call site.
- `substrate-port` — The run-path substrate is one interface: start or resume a run for a content-hashed plan reference, record a step output, commit a cursor, suspend on an awakeable with a timeout, attach a retry schedule, describe capabilities.
  *A-run*
- `plan-pin` — A run resolves the plan reference it started against for its whole life.
- `unwired-capability` — Reaching for a capability the running profile does not wire raises `CapabilityUnwired` at the first reach, before any half-finished work.
  *A-topology*
- `machine-state` — Journal rows, execution owners and awakeables are machine-local state that a catalog rebuild leaves untouched and the file tree never reconstructs.

```mermaid
sequenceDiagram
  participant A as caller A
  participant B as racing caller B
  participant J as journal
  participant V as vendor
  A->>J: claim (execution_id, step_label, input_hash) as pending
  B->>J: same key
  J-->>B: wait for the recorded value
  A->>V: effect, idempotency key derived from the entry key
  V-->>A: response
  A->>J: record value, inline up to 1 MiB, else a sha256-named blob
  J-->>B: recorded value
  Note over A,V: a crash before the write re-enters the effect under the same idempotency key
  Note over A,J: a pending claim whose owner lease expired passes to the next caller
```

unsettled: Does the inline-versus-blob cutoff stay one number across every step kind? owner: run-path affects: run.journal

unsettled: How do fan-out bodies express an explicit join, and what does a partially-failed fan-out record? owner: run-path affects: run.journal

## advance

Committing an incremental read position under its declared kind, and the boundary a poll re-reads.

- `cursor-kind` — A cursor declares `monotonic` for a timestamp, autoincrement or watermark, `opaque-token` for a vendor continuation token, or `snapshot-id` for a log sequence number or version id. An undeclared kind resolves to `opaque-token`.
- `concurrency-by-kind` — A `monotonic` position is concurrent-safe and commits the highest value observed. An `opaque-token` or `snapshot-id` position moves only under a single-writer lease. Last-write-wins governs no cursor.
  *because two writers resolving one continuation token by recency skip or duplicate records with no count showing it*
- `engine-owned` — A connector moves its position only by committing through the engine; that commit is a recorded step and replays as a resolved read.
- `commit-with-rows` — A table's new position rides the run commit marker, {{store.lay-out.run-manifest}}, that publishes the rows behind it; the catalog cursor is a cache of the newest committed marker.
  *because rows and position committed in two stores leave a crash window that re-lands a batch*
- `cursor-bytes` — A position is opaque bytes the connector owns, and its kind is a manifest fact the engine reads. Each table carries its own position.
- `inclusive-boundary` — A polled load admits a row whose clock value is at or after the stored position, re-landing the boundary instant's rows on every poll.
  *because a clock coarser than the row grain otherwise drops a sibling sharing the boundary value, permanently and invisibly*
- `declared-key-required` — The boundary re-land is idempotent on a table declaring a key; on a keyless table the repeats accumulate in its union view.
- `watermark-shape` — A watermark position serializes as `{"field": "<name>", "at": <value>}`, naming the column it was measured against.
- `field-rename` — Opening a position whose stored field differs from the declared `incremental` field raises `CursorFieldMismatch` before any request leaves the host.
  *A-run*
- `frontier` — The frontier counts every fetched row, landed or not. A committed position moves forward or holds; an empty or older window never rewinds it.
- `unorderable-position` — A row with no orderable value in the clock field, or a stream switching between text and numeric positions mid-pass, raises `CursorPositionUnorderable`, terminal for the pull.
  *A-run*
- `turning-incremental-on` — Enabling `incremental` on a pipeline that holds a position starts from none, re-landing the source's current window once.
- `skip-unchanged` — A snapshot-shaped source with `skip_unchanged = true` records its input's raw-byte digest as `{ sha256, rows }`; a matching digest returns zero batches, holds the position and closes a zero-row success. Undeclared, it is false.
- `zero-row-commit` — A commit landing zero rows adds and replaces nothing, so a replacing table keeps its last non-empty state across a skip or an empty pull.

unsettled: What bounds allowed lateness for an out-of-order source, and does a lateness window hang on the position or on the table? owner: run-path affects: run.advance

## suspend

Durable suspension on an external callback, its deadline and its resumption.

- `awakeable` — An awakeable suspends a run durably: the engine mints an opaque single-use token and persists a `pending` row beside the journal; an external party resumes by posting the token back.
  *A-run*
- `resume-is-a-step-output` — A resume payload is recorded as the awaited step's output under key `sha256("awakeable:" + token)`, so a run that resumes and then crashes reads it back without suspending again.
- `deadline` — A deadline is the creation instant plus a time-to-live, both caller-supplied RFC3339 Zulu strings; the core reads no wall clock.
- `timeout-is-sticky` — Expiry is evaluated against an injected instant; a suspension past its deadline becomes `timed_out` and stays so under any later instant.
- `idempotent-resolution` — Resolving a token again with the identical payload returns the recorded value.
- `conflicting-resolution` — A second resolution with a different payload raises `AwakeableAlreadyResolved` and leaves the recorded value untouched.
  *A-run*
- `expired-token` — Resolving a token past its deadline raises `AwakeableTimedOut`.
  *A-run*
- `unknown-token` — A token with no row raises `AwakeableUnknown` and allocates no state.
  *P2*
- `resume-route` — `POST /awake/:token` answers `200` with the recorded payload, `404` for {{run.suspend.unknown-token}}, `409` for {{run.suspend.conflicting-resolution}} and `410` for {{run.suspend.expired-token}}. `GET /awake/:token` reports state without resuming. Both authenticate before touching the registry.
- `payload-offload` — A payload above {{run.journal.inline-cutoff}} lands in the journal's blob store, and the pending row references it.
- `survives-restart` — The awakeable registry persists beside the journal; a restart drops no pending callback.

```mermaid
sequenceDiagram
  participant R as run
  participant E as engine
  participant X as external party
  R->>E: await awakeable, creation instant + time-to-live
  E->>E: mint single-use token, persist pending row
  E-->>X: token
  X->>E: POST /awake/:token with payload
  alt no row for the token
    E-->>X: 404 · AwakeableUnknown
  else past the deadline
    E-->>X: 410 · AwakeableTimedOut
  else resolved earlier with another payload
    E-->>X: 409 · AwakeableAlreadyResolved
  else first or identical payload
    E->>E: record payload as the awaited step output
    E-->>X: 200 · recorded payload
    E-->>R: resume
  end
```

unsettled: Is resuming a suspension bound to a verified caller identity, or is possession of the single-use token the whole authority? owner: run-path affects: run.suspend

## retry

Classifying a failure, the single retry layer, and the delay before the next attempt.

- `failure-taxonomy` — One tagged failure type crosses every port: `Transient`, `Permanent`, `SchemaIncompatible`, `AuthExpired`, `RateLimited`, `Config`, `Storage`, `UnknownConnector`, `SecretNotFound` and `Canceled`. A caller branches on the tag, never on a message.
  *P2*
- `schedule` — A schedule attaches to a step and carries a fixed or exponential backoff with base and factor, a total attempt count including the first, a per-delay clamp, a jitter ceiling and `retry_on`.
- `default-policy` — The default schedule is exponential from a 100 ms base with factor 2, to 5 attempts in total, each delay clamped at 30 s, plus seeded jitter below 250 ms.
  *because zero jitter synchronizes every run that hit one shared throttle*
- `single-attempt` — A schedule of 1 attempts is the no-retry schedule.
- `retryable-classes` — `Transient` and `RateLimited` retry; every other tag stands as the step's answer whatever budget remains. `retry_on` narrows that pair.
  *P6*
- `unretryable-tag` — A `retry_on` naming any tag outside `Transient` and `RateLimited` raises `RunRetryTagUnretryable` at plan compile.
  *P6*
- `step-failed` — A non-retryable failure, or a step exhausting its schedule, raises `StepFailed` carrying the step label and the underlying tag.
  *P6*
- `deterministic-verdict` — A refusal whose check is pure over static input is terminal and consumes no attempt budget.
  *P6*
- `retry-after` — A server-supplied `Retry-After` supersedes the computed delay up to 300 s; a longer one closes the step as `RateLimited` and the run is rescheduled instead of sleeping.
  *because an unclamped upstream delay holds a run slot for as long as the vendor asks*
- `one-layer` — The step's schedule is the only retry layer: an adapter, a credential mint and a meter acquire inside a step return their failure to it.
  *because nested retry layers multiply attempts and amplify a vendor outage*
- `decision-is-pure` — The retry decision is a pure function of the one-based attempt that just closed and the classified failure, jitter derived from an injected seed. The runner owns the sleep and the attempt counter.
- `partial-failure` — A run that landed some batches and then failed closes `partial_failure`, naming how many landed; the journal holds the resume point.
- `pressure-is-local` — Rate-limit pressure against one source paces only that source's runs.
- `bound-hit-is-transient` — A connector striking one of its declared resource bounds answers `Transient`, and every bound decision is recorded on the run record.

unsettled: Does the engine offer a compensation combinator for partial-failure rollback, or does compensation stay author-written recorded steps? owner: run-path affects: run.retry

unsettled: Where does an in-flight schedule's attempt counter persist, so a crash mid-backoff resumes the budget instead of restarting it? owner: run-path affects: run.retry

## own

The execution owner a scope holds and the connector build it pins while pending.

- `execution-owner` — One durable execution owner exists per live table and per backfill chunk, holding the connector identity with its component world, the pipeline `content_hash` and the input hash.
- `execution-id-keys-the-journal` — Recorded work keys on the execution id; a catalog run id is the provenance of one attempt, so attempts under one owner resolve the same recorded values.
- `retirement` — Retiring an owner and caching its committed position share one catalog transaction; a later fire receives a fresh execution id even at a byte-identical position. A completed backfill chunk retires the same way.
- `marker-reconciles` — At run open, a commit marker newer than the catalog's cached position retires the pending owner that produced it before any replay.
  *because a crash between marker and catalog otherwise replays a recorded pull into rows already committed*
- `pinned-plan-changed` — While an owner is pending, a changed connector identity, component world or pipeline `content_hash` raises `ExecutionPinMismatch` before replay, terminal and non-retryable, naming the pipeline and both identities.
  *A-connector*
- `pin-recovery` — Restoring the recorded build and resuming to completion clears a pending owner; an explicit chunk rewind retires the owners its window covers.
- `scope-independence` — A finishing table releases nothing another table's unfinished execution holds, and a seeding scope carries its own source identity.
- `pin-release` — `success`, and a failure that landed zero batches, release the owner; every other status holds it.
- `admission-pin` — A run pins its connector identity at admission and a replay resolves the artifact from that pin; a connector rebuilt later reaches no in-flight or replayed run.
  *A-connector*
- `backpressure` — A source yields a stream of batches the runner pulls; the run path holds no unbounded buffer between source and destination.
- `one-commit-per-run` — One run produces one atomic commit per table; a crash mid-run leaves parts under that run's own directory, and a resumption continues from the last recorded step.

```mermaid
flowchart TD
  O["run open"] --> M{"commit marker newer than the cached position?"}
  M -->|yes| RET["retire the pending owner that produced it"]
  M -->|no| P{"pending owner?"}
  RET --> FRESH["fresh execution id"]
  P -->|no| FRESH
  P -->|yes| PIN{"connector identity, world, content_hash unchanged?"}
  PIN -->|no| ERR["ExecutionPinMismatch · terminal"]
  PIN -->|yes| REPLAY["replay recorded steps under the owner"]
  REPLAY --> CLOSE{"close status"}
  FRESH --> CLOSE
  CLOSE -->|"success, or a failure landing zero batches"| REL["release the owner"]
  CLOSE -->|"any other status"| HOLD["hold the owner"]
```

## cancel

Stopping work in flight at either grain, the one token every await observes, and what a stop leaves behind.

- `catalog-channel` — A stop is written onto the run row as a requested-at instant, a scope and an optional reason; the catalog is the only channel.
- `two-grains` — A `run`-scoped stop halts one run and the fire continues; a `pipeline`-scoped stop halts every in-flight run of the pipeline and every table or chunk the fire had left. An unrecognized stored scope reads as `run`.
- `one-token` — Each run holds one cancellation token that every await selects on: the pull, a retry sleep, an awakeable wait, a meter acquire and a subprocess wait.
  *because a stop observed only at the pull leaves sleeps and child processes running after the record reads canceled*
- `poll-interval` — The token is fed by one catalog read before the run's first await and then one every 500 ms, the cancellation arm evaluated ahead of the work arm.
- `land-path-uncut` — The land path carries no stop check; a run whose bytes are home finishes landing and records the stop it did not fulfill.
- `abandoned-work` — Abandoned work surfaces as the `Canceled` tag through the ordinary failure path, which settles the request ledger, closes the record and leaves the position alone.
  *A-run*
- `child-reaped` — A stop signals a running subprocess chain's process group, and the record is written `canceled` only after the group is reaped.
  *because a record reading canceled while a child still runs misstates what the machine is doing*
- `storage-blip` — A failed poll read warns and keeps polling.
- `not-in-flight` — Only a `pending`, `running` or `waiting` row accepts a mark; a stop matching none raises `CancelTargetNotInFlight`, answering `409` over HTTP and exiting non-zero at the terminal.
  *A-run*
- `re-mark` — Marking an already-marked row overwrites it with the newer request.
- `distinct-terminal-status` — `canceled` is a terminal status apart from `failed`, and upstream health observations skip it.
  *A-run*
- `resumable-remains` — A stopped run's recorded pull keeps its owner and replays next attempt; a stopped pull that recorded nothing releases it. A stopped chunk returns to pending, its attempt count unchanged. A stop advances no position.
- `authority-from-the-record` — A stop authorizes against the pipeline read off the run record, never off the request.

```mermaid
sequenceDiagram
  participant O as operator
  participant C as run row in the catalog
  participant R as run
  participant G as subprocess group
  O->>C: stop · requested-at, scope, reason
  alt row not pending, running or waiting
    C-->>O: CancelTargetNotInFlight · 409
  end
  loop every 500 ms
    R->>C: read the stop mark
  end
  C-->>R: cancellation token fires on every await
  R->>G: signal the process group
  G-->>R: reaped
  R->>C: record canceled · Canceled tag, ledger settled, position unchanged
```

## record

The durable run record, its statuses, its owner lease and windowed history over it.

- `reserved-table` — The durable run record is a reserved store table holding two append-only rows per run, one at plan and one at commit; the latest row per run and phase wins. The local catalog is a cache in front of it.
- `columns` — A run row carries run id, pipeline id, site id, status, owner, start and end instants, row and byte counts, error kind and message, connector id, version and hash, trace id and phase.
- `status-set` — A run status is `pending`, `running`, `waiting`, `success`, `partial_failure`, `failed` or `canceled`, spelled identically on the record and the wire snapshot; every surface classifying a run covers all seven.
  *because two vocabularies for one lifecycle leave a suspended or partly failed run without a reading*
- `row-at-open` — A row is written at run open, before the first pull, so a zero-row `success`, a `failed` row carrying its error kind, and no row at all read apart.
- `time-travel` — Run history inherits the store's transaction-time bound, so one as-of selector rewinds it with every other table.
- `owner-lease` — A `running` or `waiting` row carries an owner of process id, boot id and a lease expiry, renewed every 10 s with a 30 s time-to-live.
  *because a second process sharing the catalog needs a liveness signal to tell a live run from an orphan*
- `orphan-reap` — Startup marks `partial_failure` only a non-terminal row whose owner lease has expired; a row held by a live process is left alone.
- `writing-site` — The writing site is an engine-injected provenance column beside the ingestion stamp and run id; a row written without one reads null and renders unattributed.
- `site-id-length` — A site id matches letters, digits, dot, underscore and hyphen, from 1 chars to 64 chars.
  *because it is interpolated into run directories and request-ledger filenames*
- `site-id-unresolved` — A site id comes from the manifest or a named environment variable; an unset variable, or both sources declared, raises `SiteIdUnresolved` at startup.
  *P3*
- `history-window` — History is read through a window, an inclusive start-instant lower bound plus a row ceiling, both applied in the storage adapter's `WHERE` clause, newest first.
- `describe-window` — The describe surface answers with 5 rows by default and clamps a caller's ceiling at 500 rows.
- `bound-spelling` — A window lower bound is `YYYY-MM-DD` or a UTC RFC3339 instant ending `Z`; any other spelling raises `HistoryBoundSpelling` at every caller-facing surface.
  *because a bound read under a guessed zone silently moves the window*
- `truncation-flag` — A history response echoes its window and flags truncation when the pipeline recorded more runs inside it than the ceiling returned.
- `empty-window` — An empty window answers `[]` and raises nothing.
- `history-export` — History leaves the machine as a process listing and an NDJSON export over one run projection; the export's first line carries store, window, count and truncation flag, then one run per line. It resolves no bucket.
- `export-ceiling` — The export answers with 500 rows by default and clamps at 5000 rows, each pipeline's window taking the full ceiling before the merged result is clipped once.
- `error-cap` — A recorded error is masked over credential-shaped spans and capped at 2 KiB with an ellipsis marker, on the one projection every surface serves.
- `failure-publishes` — The bucket push runs on the failure arm as on the success arm, for every failure past run open.
- `per-table-accounting` — A table failing inside a multi-table fire leaves one row with its error kind, a step-failure and a run-failure event, and a position where the next run resumes, under either `on_table_error` setting.
- `counts-at-destination` — Row and byte counts are measured at the destination, not at the source.

unsettled: What does a run-record manifest carry for a catalog rebuild to restore history instead of resetting it? owner: run-path affects: run.record

## project

The best-effort live view of a run and the subscription that carries it off the machine.

- `best-effort` — Live run state is a best-effort projection of events the runner emits after durable state changes; execution never reads it.
- `wire-snapshot` — The wire snapshot carries run id, workflow id, a status from {{run.record.status-set}}, an ordered step list, an application metadata map, optional start and end instants, an optional tagged error and a version. Every deploy target emits it identically.
- `step-view` — A step view carries its label, a status of `pending`, `running`, `completed`, `failed` or `retrying`, its attempt count, optional instants, an optional output reference with byte count, and an optional tagged failure.
- `stopped-step` — A stopped run reports its executing step `failed` with the `Canceled` tag and leaves the run-level error unset.
- `emission-never-blocks` — Progress emission is no journal step: events reach an in-process observer over a bounded channel that drops when full and never blocks the runner.
- `terminal-slot` — A terminal transition bypasses the lossy channel through a per-run slot that is never dropped; a projection reading non-terminal behind a terminal run record reconciles from the record.
  *because a dropped terminal event otherwise leaves a finished run reading running forever*
- `coalescing` — Snapshot broadcasts coalesce to at most one per 100 ms, keeping only the latest; a terminal transition flushes immediately. The clock is injected.
- `metadata-too-large` — A metadata write whose serialized size exceeds 16384 B raises `RunMetadataTooLarge` naming size and bound, and the snapshot stands unchanged.
  *because a silently clipped map reads as the complete metadata*
- `version` — The snapshot version is `(epoch, counter)`, the epoch being the hub's start instant, compared lexicographically; a client discards any delta at or below the highest version it holds.
  *because a bare counter restarts at zero with the process and freezes every client's view*
- `reducer-is-total` — The reducer is total: nothing overwrites a terminal step or run status, a new step label appends, an existing one merges field-wise, and a stale delta is a no-op.
- `outputs-by-reference` — A step output appears as a post-redaction reference and a byte count, never inline; fetching the payload is a separately authorized request.
- `connect` — A subscriber receives the folded snapshot and its update receiver under one lock, missing and duplicating no update.
- `broadcast-ring` — The per-run broadcast holds 256 entries; a subscriber that overruns it resynchronizes to the latest snapshot.
- `unauthenticated-upgrade` — The run-stream socket authenticates on the HTTP request before the upgrade; a missing or invalid credential raises `RunStreamUnauthorized` with `401`, and an unseen run answers `404`.
  *A-read*
- `read-only-socket` — A subscription is read-only; a stop travels as an authenticated route.
- `restart-discards` — A restart discards in-memory snapshots; a subscriber recovers history from the durable record.
- `delegated-progress` — A heavy step delegated to another host reports progress by posting to the awakeable callback route, and the orchestrating run folds it into its own snapshot.

unsettled: Does the hub keep a bounded ring of recent deltas so a late joiner can animate the catch-up? owner: run-path affects: run.project

unsettled: How is a per-run channel evicted in a long-lived process, given that a channel is allocated on a run's first event? owner: run-path affects: run.project

## Shapes

Run statuses:

```mermaid
stateDiagram-v2
  [*] --> pending
  pending --> running
  running --> waiting: awakeable
  waiting --> running: resumed
  running --> success
  running --> partial_failure
  running --> failed
  running --> canceled
  waiting --> canceled
  success --> [*]
  partial_failure --> [*]
  failed --> [*]
  canceled --> [*]
```

The three cursor serializations, and the unchanged-input digest:

```json
{"kind": "monotonic",    "position": {"field": "updated_at", "at": "2026-03-01T00:00:00Z"}}
{"kind": "opaque-token", "position": "eyJwYWdlIjo0N30="}
{"kind": "snapshot-id",  "position": {"lsn": "0/1A2B3C4D"}}
{"kind": "monotonic",    "position": {"sha256": "c1d9…", "rows": 20418}}
```
