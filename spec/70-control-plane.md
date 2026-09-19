---
contract: surface
owns:
  - arm
  - reconcile
  - fire
  - dispatch
  - edit
  - apply
  - reside
---

# Cadence, dispatch and the operator plane

The control plane decides when work runs and where. It holds the schedule grammar and its
trigger, the reconciler that turns an applied snapshot into an armed set, the scheduler's
same-tick order, the bounded dispatch pool and its worker adapter, the operator's edit and
apply path over the control document, and the region a data plane resides in. The published
hostname and the read path's two hops belong to the topology contract.

## arm

| Clause | Statement | Why |
| --- | --- | --- |
| `surface.arm.schedule-grammar` | A schedule string is `every <n><s\|m\|h\|d>` or a five-field cron expression, both evaluated in UTC on the engine clock. The grammar carries no zone field. | — |
| `surface.arm.cron-dialect` | Cron fields are minute, hour, day-of-month, month and day-of-week, each taking `*`, lists, ranges and steps. When day-of-month and day-of-week are both restricted, a day matching either one matches. | because the OR reading is the one operators carry from Vixie cron; an AND reading silently changes their instants |
| `surface.arm.one-parser` | One parser evaluates schedules in the engine and, compiled to WASM, in the editor preview. It reproduces the golden next-five table under Shapes. | because two parsers can pass every clause here and still disagree about fire instants |
| `surface.arm.unreadable-schedule` | A schedule string the grammar cannot read, including `L`, `W`, `?`, `#` and `@` macros, raises `ScheduleUnreadable` for that entry alone, naming the diagnostic; every other entry of the document arms. | A-surface |
| `surface.arm.validate-before-arm` | An entry revalidates against the manifest guardrails before joining the armed set, and a guardrail refusal holds back that entry alone. | — |
| `surface.arm.next-fire` | An interval entry fires one interval after its last recorded fire, unanchored to the clock, and is due at the next evaluation when none is recorded. A cron entry fires at the next instant its expression names. | — |
| `surface.arm.missed-window` | At startup the gap since the last fire is logged and the declared policy applies: `fire-once` closes it with one catch-up fire, `skip` resumes at the next window. Entries default to `fire-once`, jobs to `skip`. | — |
| `surface.arm.unknown-history` | Fire history that cannot be read or parsed reads as unknown, not as never-fired, and leaves arming as it stood. | because a transient catalog fault read as never-fired fires every entry of a store at once |
| `surface.arm.no-dependency-edges` | The armed set carries no edge between entries and no success-triggers-start orchestration. Cross-entry ordering lives in the model graph. | — |
| `surface.arm.trigger-adapter` | The serve command's `--trigger` option or its environment equivalent selects the adapter. `in-process` ticks while the process is awake and is not durable; `external` waits on a platform cron, alarm or crontab calling the wake route, and is durable. | — |
| `surface.arm.adapter-printed` | The selected adapter and its durability print at startup and read identically on the health route. | — |
| `surface.arm.unknown-trigger` | An unrecognized trigger value raises `TriggerAdapterUnknown` at startup and downgrades onto no adapter. | P3 |
| `surface.arm.trigger-face-missing` | `external` selected on a deployment serving no HTTP face raises `TriggerFaceMissing` at startup. | P3 |
| `surface.arm.one-due-evaluation` | Both adapters reach the armed set through one due-ness function. The engine decides the armed set, the order and each next fire; an adapter decides when the engine looks. | — |
| `surface.arm.wake-route` | `POST /schedule/run-due` names nothing to execute. It enqueues one evaluation onto the scheduler task, and a wake arriving between two due instants fires nothing. | — |
| `surface.arm.wake-answer` | A wake answers within 25 s with what fired, what failed, what stays armed and the next due instant, naming each fire still in flight as pending. | because a platform request timeout otherwise ends the call before the caller learns anything |
| `surface.arm.wake-disconnect` | A caller disconnecting from a wake cancels no fire it enqueued. | — |
| `surface.arm.wake-authority` | The wake route takes an authorization covering the wake resource; one covering a single pipeline confers nothing. A refused response, a malformed answer or a failed entry fails the calling scheduled event. | — |
| `surface.arm.tick-interval` | The in-process adapter evaluates the armed set every 500 ms. | — |
| `surface.arm.external-resolution` | Under `external`, cadence resolution is bounded below by the platform's wake granularity plus container start, both recorded in {{topology.deploy.target-profile}}. | — |

## reconcile

| Clause | Statement | Why |
| --- | --- | --- |
| `surface.reconcile.control-source` | A `[control]` block in `contextful.toml` names `snapshot_dir` or a control server URL, plus `poll`; the serve command's `--control` option overrides it. A deployment declaring neither arms from its local manifest. | — |
| `surface.reconcile.poll-cadence` | `poll` takes a schedule string, and a `[control]` block declaring none polls every 30 s. | — |
| `surface.reconcile.snapshot-is-the-set` | Under a control source the applied snapshot is the whole pipeline schedule set: local pipeline schedules stay unarmed, an id a newer snapshot omits leaves the armed set, and the set is empty before a first snapshot. | — |
| `surface.reconcile.snapshot-keys` | A control snapshot store holds one `<store>/manifest@current` pointer whose body is an ASCII integer version and nothing else, and one immutable `<store>/manifest@v{N}.toml` per applied version. Producer and consumer read one definition of this key format. | — |
| `surface.reconcile.pointer-malformed` | A pointer body that is not wholly a version raises `ControlPointerMalformed`. | because an integer read out of leading bytes arms a version nobody applied |
| `surface.reconcile.version-gate` | A poll re-parses the named snapshot only when its version exceeds the armed version. | — |
| `surface.reconcile.diff` | The diff is a pure function over a desired and a running set keyed on pipeline id and schedule string, yielding sorted buckets `added`, `removed`, `retimed` and `unchanged`. An entry with no schedule joins no set. | — |
| `surface.reconcile.unchanged-keeps-next-fire` | An id in `unchanged` keeps its existing next fire. | — |
| `surface.reconcile.fail-static` | An unreadable pointer, an unparseable snapshot or a control plane answering `5xx` raises `ControlSnapshotUnreadable`, logs a diagnostic, and leaves the armed set in place running. | A-surface |
| `surface.reconcile.apply-fires-nothing` | An id new to a snapshot arms from the current instant without firing, and windows missed between two snapshots collapse into one fire. | — |
| `surface.reconcile.retime-does-not-interrupt` | A retimed entry's in-flight run completes under its earlier arming, and the new cadence governs its next fire. | — |
| `surface.reconcile.beat-order` | One beat reads the pointer, fetches that version's document, derives the scheduled set, diffs it against the armed cursor, persists the cursor, then dispatches. The cursor is the reconciler's only state between beats. | — |
| `surface.reconcile.loopback-only` | A control URL whose host is not a loopback address raises `ControlSourceNotLoopback` and arms nothing. | A-surface |
| `surface.reconcile.loopback-connection` | The poll client follows no redirect, ignores proxy environment variables, and pins `localhost` to the loopback addresses; a name merely resolving to loopback is not loopback. | — |

unsettled: Does a daemon reach a non-loopback control source, carrying bearer authentication, TLS and producer signing over version and content hash, and does it learn of a new snapshot by poll or by push? owner: control affects: surface.reconcile

## fire

| Clause | Statement | Why |
| --- | --- | --- |
| `surface.fire.job-block` | A `[[job]]` block declares a name, a `schedule`, a kind, an optional target, and an enabled flag, true when unstated; a disabled block joins no armed set. Job names occupy a namespace apart from pipeline ids. | — |
| `surface.fire.job-kinds` | The scheduler fires a closed union of kinds — `sweep`, `build`, `fold`, `rebuild-catalog`, `sync-push`, `validate` — through an exhaustive match, beside the implicit pipeline-run kind. Each runs in process through its command verb's path. | — |
| `surface.fire.job-kind-unknown` | A block naming a kind outside the union, an argument vector or a host command raises `JobKindUnknown` at validation. | A-surface |
| `surface.fire.jobs-are-operator-local` | Job blocks live in the daemon's own configuration and enter no control document. Under a control source the reconciler governs entry cadence and the daemon's operator governs job cadence. | — |
| `surface.fire.watermark` | A job persists no run record. Its schedule memory is a `job_fire` watermark, one row per job name overwritten on each successful fire, which survives a restart. | — |
| `surface.fire.stage-order` | Entries due in one tick fire in the order `validate`, `rebuild-catalog`, `sweep`, pipeline runs, `fold`, `build`, `sync-push`. The order breaks same-tick ties only: nothing not due is pulled forward and nothing waits. | — |
| `surface.fire.failure-continues` | A failed entry is logged and evaluation continues; one failure ends neither the remaining due entries nor the process. | — |
| `surface.fire.fold-target` | A `fold` target names the on-disk table `<pipeline>_<table>`, each non-alphanumeric character folded to `_`. An omitted target covers every table declaring a primary key. | — |
| `surface.fire.target-unbound` | A `fold` target naming no produced table, or a `build` target naming no declared model, raises `JobTargetUnbound` at validation. | P1 |
| `surface.fire.build-job-coverage` | A document publishing a model that no enabled `build` job covers draws one validation warning per model. A targetless `build` job covers every model. | because a missing build job shows no run-time symptom: every job succeeds while the published watermark stands still |
| `surface.fire.one-shot-cycle` | `cycle` evaluates the armed set once and exits, over the daemon's due-ness test, watermark and stage order. Every kind defaults to `fire-once`, and an entry never fired is due at once, a cron entry included. | — |
| `surface.fire.cycle-control-source` | A configured control source that does not resolve under `cycle` raises `CycleControlSourceUnresolved`. | P3 |
| `surface.fire.cycle-exit-status` | `cycle` exits zero when nothing failed, nothing due included, reporting each entry's next window; it exits non-zero after evaluating the rest when any entry failed. `--all` fires every enabled entry and still advances the watermark. | — |

## dispatch

| Clause | Statement | Why |
| --- | --- | --- |
| `surface.dispatch.one-instance-per-unit` | The reconciler starts one durable orchestrator instance per due dispatchable unit. A unit is dispatchable when it starts from the store alone. | — |
| `surface.dispatch.not-a-head` | Where entries are landing steps of one dependent run, the run is the unit and cadence rides its head entry. A due id that is a step raises `DispatchUnitNotAHead` and starts nothing. | A-surface |
| `surface.dispatch.fire-pool` | Due work enters a pool bounded by a configured count of fires in flight, floored at one. | — |
| `surface.dispatch.exclusion-keys` | A per-source key serializes fires against one upstream, and two fires of one entry do not overlap. The maintenance kinds hold a store-global key excluding every entry writing the tree. Both keys are catalog leases. | — |
| `surface.dispatch.deferred-stays-due` | Work deferred by a key or by the pool bound stays due and is reconsidered on the next tick. A due maintenance job defers new entry fires until the pool drains. | — |
| `surface.dispatch.cadence-lease` | The reconciler holds the deployment's cadence lease, a {{topology.coordinate.lease-row}} granted per {{topology.coordinate.cadence-lease-ttl}} and renewed per {{topology.coordinate.cadence-lease-renewal}} | — |
| `surface.dispatch.fallback-cron` | The deployment's own cron fires work as {{topology.coordinate.cadence-fallback}}, and a fire whose fence has moved commits nothing, per {{topology.coordinate.fenced-commit}} | because a cron firing beside a live reconciler double-fires every entry |
| `surface.dispatch.fallback-covers-empty` | The fallback covers each empty control-plane state: nothing applied, a document scheduling only chain steps, and a control plane answering `5xx`. | — |
| `surface.dispatch.step-result` | A checkpointed step returns a pointer, an object-store key or a catalog row id, under a 1 MiB step-result cap. | — |
| `surface.dispatch.state-outside-container` | A worker writes its output to the shared object store and returns the key. A container carries no state across a step boundary. | — |
| `surface.dispatch.step-budget` | A step fits its target's per-step budget as {{topology.deploy.target-profile}} records it. The planning call splits a wider upstream into batched steps, and heavier compute delegates to a worker target. | — |
| `surface.dispatch.in-band-tool-error` | A step driving the engine over the tool protocol judges the tool result, not transport status: a `200` carrying a protocol error or a result flagged `isError` raises `StepToolError`. | P2 |
| `surface.dispatch.worker-adapter` | A worker target implements `submit(job, idempotencyKey)` returning a handle, a completion callback, `cancel(job)` that is best-effort and safe after completion, and a heartbeat. Each submit carries the step's attempt number, which rescheduling increments. | — |
| `surface.dispatch.callback-auth` | A completion callback posts a result pointer to the relay route under an HMAC over run id, step, attempt and timestamp, and the result lives at an attempt-scoped key. | — |
| `surface.dispatch.callback-skew` | The relay accepts a callback timestamp within 300 s of its own clock. | — |
| `surface.dispatch.callback-rejected` | A callback whose attempt is below the step's current attempt, or whose timestamp falls outside {{surface.dispatch.callback-skew}}, raises `DispatchCallbackRejected` and changes no step. | because a partitioned worker's late result otherwise overwrites its successor's, and a captured callback replays indefinitely |
| `surface.dispatch.idempotent-submit` | `submit` under a repeated idempotency key returns the existing job and starts no duplicate. | — |
| `surface.dispatch.dial-out` | A dial-out worker opens an outbound long-lived connection and pulls work, holding no public address and no inbound port. A push target is dispatched to through its own API. | — |
| `surface.dispatch.heartbeat-beat` | A worker heartbeat runs every 15 s. | — |
| `surface.dispatch.heartbeat-lapse` | A heartbeat lapse past 60 s reschedules that worker's in-flight work onto another matching worker under the next attempt number. | — |
| `surface.dispatch.tagged-routing` | A worker registers tags, and a step declares `requires` as a strict constraint and `prefer` as affinity. The engine matches `requires`, breaks ties by `prefer` then by least load, and a strict tag falls back onto no other worker. | — |
| `surface.dispatch.worker-credentials` | A dial-out worker resolves credential references against its own local store, and the orchestrator holds no material. Worker transport is WSS or HTTPS with optional mutual TLS. | — |
| `surface.dispatch.worker-identity` | A worker's own credential is scoped to one tenant and its tags and is revocable from the control plane. Each dispatched job records worker id, tags and outcome in the audit chain. | — |
| `surface.dispatch.one-progress-writer` | Dispatch gives a delegate no progress channel of its own toward a subscriber; {{run.project.delegated-progress}} | — |

unsettled: Is the fire pool's bound one number per deployment or one per exclusion key? owner: control affects: surface.dispatch

## edit

| Clause | Statement | Why |
| --- | --- | --- |
| `surface.edit.single-writer` | The configuration document is the one writer of configuration: entry definitions, connector references with their configuration, schedules, placement targets and ordered transforms all move through it. | — |
| `surface.edit.read-only-records` | Run history and status, the memory record, the audit log, secret material and credential issuance are read-only in the operator surface. | — |
| `surface.edit.merge-boundary` | The surface separates the irreversible last-write-wins area from the mergeable area, showing an operator which fields merge before they type. | — |
| `surface.edit.crdt-at-edit-time` | The document is a CRDT persisted per store under conditional replacement; concurrent edits reload and reapply on contention. Daemons and replicas read the materialized snapshot and link no edit-time library. | — |
| `surface.edit.schedule-spec` | The editor mutates one structured schedule specification, and the manifest schedule string is its compile target. Manual emits no schedule, interval emits the interval form, daily, weekly and monthly emit plain cron, and raw cron passes through. | — |
| `surface.edit.round-trip-lift` | A stored cron lifts into the structured editor when recompiling reproduces the expression exactly, and the raw-cron editor states which side of that line an expression fell on. | — |
| `surface.edit.schedule-footer` | A visible footer carries the humanized schedule, the next five fire instants, the compiled string and an approximate daily rate. Cron previews name the zone they render in; interval previews count from the current instant. | — |
| `surface.edit.connector-configuration` | Adding a connector picks it and a pinned version from the registry, renders its configuration from the connector's declared schema, records the operator's grant or narrowing of each requested capability, and binds credentials as references. | — |
| `surface.edit.secret-in-document` | A credential value typed into a configuration field raises `SecretMaterialInDocument`; the document holds references and the backend holds material. | because a value in the document reaches every daemon and replica that reads a snapshot |
| `surface.edit.connector-probe` | A dry-run action exercises a connector's health check or first-page pull against its declared grants and commits no schedule. | — |
| `surface.edit.connector-upload` | An artifact uploaded through the operator surface raises `ConnectorUploadRefused`; the surface references registered connectors by id and version. | because publishing into the registry runs a separate signed path |

## apply

| Clause | Statement | Why |
| --- | --- | --- |
| `surface.apply.claims-a-version` | An apply validates the document through the engine, claims an immutable `<store>/manifest@v{N}.toml`, and advances that store's pointer by compare-and-swap. The version counter is per store. | — |
| `surface.apply.engine-owns-state` | The engine holds the control-plane state model, assigns every version and materializes every manifest. A surface adapter reaches it through the authenticated store-scoped API and writes it by no other path. | — |
| `surface.apply.version-race` | An apply whose compare-and-swap loses raises `ManifestVersionConflict`, reloads the winning version and reapplies its pending edits onto it, overwriting no applied version. | A-surface |
| `surface.apply.validation` | A document failing engine validation raises `ApplyValidationRefused` and claims no version. | because a snapshot store holding a version daemons refuse to arm stalls every daemon reading it |
| `surface.apply.admin-capability` | Every edit and apply carries an admin capability held as a server-side secret, never in the browser; the credential-issuance routes take the same capability. | — |
| `surface.apply.attributed` | Every apply is attributed to the verified identity that made it and lands in the append-only audit log. | — |
| `surface.apply.owner-storage` | The hosted configuration owner writes to object storage with strong conditional replacement; filesystem storage serves a single-process local path. | — |
| `surface.apply.owner-unconfigured` | An owner with no storage configured or no credential raises `ConfigOwnerUnconfigured`, answered `503`; no local writer substitutes for the store-scoped API. | P3 |
| `surface.apply.weak-conditional-backend` | A configuration owner or catalog backend whose conditional replacement is not linearizable raises `ConditionalWriteUnsupported` at startup or open. | A-surface |
| `surface.apply.uninitialized-store` | An edit or apply against a store that has taken no explicit guarded import, an empty store included, raises `StoreNotInitialized`, answered `409`. | P3 |
| `surface.apply.retention` | The owner collects no superseded version. Retention of applied versions is operator policy, and an older version stays readable at its key. | — |

unsettled: Can a local control plane validate and claim a version on its own, or does every apply route through a hosted one? owner: control affects: surface.apply

## reside

| Clause | Statement | Why |
| --- | --- | --- |
| `surface.reside.data-plane-is-the-operators` | Columnar files, the manifest and the catalog database live in a bucket the deploying organization controls. No vendor-operated data path exists. | — |
| `surface.reside.control-plane-holds-no-rows` | An optional hosted control plane stores manifests and schedules and no row of any table. | — |
| `surface.reside.region-allow-set` | A residency policy carries a set of regions. Placement in a region is permitted when the set is empty or names that region, and the check reads nothing beyond the set and the candidate region. | — |
| `surface.reside.region-entries` | A residency allow-set holds at most 16 entries. | — |
| `surface.reside.region-mismatch` | A configured resource resolving to a region the policy omits raises `EnforceRegionMismatch` at startup, and the runtime serves nothing. | P3 |
| `surface.reside.air-gapped` | A single-node daemon runs against local storage with no external store, and a multi-node cluster runs against an on-premises catalog; neither reaches the public internet. | — |

unsettled: How is a residency region declared differently by two sites detected, given that each resolves its own configuration? owner: control affects: surface.reside

## Shapes

A daemon's own configuration, the control wiring and its operator-local jobs:

```toml
[control]
snapshot_dir = "./.contextful/control"   # or url = "http://127.0.0.1:8787/control"
poll         = "every 30s"

[[job]]
name     = "nightly-fold"
schedule = "0 3 * * *"
kind     = "fold"
target   = "meta_ads_insights"

[[job]]
name     = "hourly-push"
schedule = "every 1h"
kind     = "sync-push"
```

The wake route's answer:

```json
{
  "fired": ["field-notes", "nightly-fold"],
  "failed": [],
  "pending": ["hourly-push"],
  "armed": 14,
  "next_due": "<instant, RFC 3339 UTC>"
}
```

The golden next-five table the one schedule parser reproduces, from Mon 01 Jun 2026 00:00 UTC,
counting instants strictly after it:

| Expression | Next five instants (UTC) |
| --- | --- |
| `0 3 * * *` | Mon 01 Jun 03:00, Tue 02 Jun 03:00, Wed 03 Jun 03:00, Thu 04 Jun 03:00, Fri 05 Jun 03:00 |
| `*/15 * * * *` | Mon 01 Jun 00:15, 00:30, 00:45, 01:00, 01:15 |
| `30 4 * * 1-5` | Mon 01 Jun 04:30, Tue 02 Jun 04:30, Wed 03 Jun 04:30, Thu 04 Jun 04:30, Fri 05 Jun 04:30 |
| `0 12 13 * 5` | Fri 05 Jun 12:00, Fri 12 Jun 12:00, Sat 13 Jun 12:00, Fri 19 Jun 12:00, Fri 26 Jun 12:00 |
| `0 0 29 2 *` | Tue 29 Feb 2028, Sun 29 Feb 2032, Fri 29 Feb 2036, Wed 29 Feb 2040, Mon 29 Feb 2044, each 00:00 |
