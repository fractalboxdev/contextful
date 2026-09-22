---
contract: run
owns:
  - declare
  - compile
  - transform
  - normalize
  - guard-secrets
  - land
  - backfill
  - seed
  - publish
---

# Pipelines and the write path

A pipeline declares desired state over one source and the tables it lands. This file holds
that declaration, the plan it compiles to, the stages a batch passes between a source and a
committed run, and what a published table carries. `land` is the one statement of stage order.

From declaration to a published table, and the contracts each step meets:

```mermaid
flowchart LR
  MF["contextful.toml, pipelines/*.toml, pipelines/*.json"] -->|declare| SPEC
  subgraph CONN["connector contract"]
    SRC["source"]
  end
  subgraph RUNC["run contract"]
    subgraph P31["31-pipeline"]
      SPEC["PipelineSpec · content_hash"]
      PLAN["plan · flat node list"]
      ST["secret guard, normalize, transform chain"]
      LAND["land · batch write, commit"]
    end
    subgraph R30["30-run"]
      RUN["run substrate"]
    end
  end
  subgraph STORE["store contract"]
    CH[("chunk plan · catalog")]
    CS[("context store")]
    MAN["snapshot manifest · contract identity, freshness, build"]
  end
  SPEC -->|compile| PLAN
  PLAN -->|"lowering"| RUN
  SPEC -->|"backfill, seed"| CH
  CH --> RUN
  SRC --> RUN
  RUN --> ST
  ST --> LAND
  LAND --> CS
  LAND -->|publish| MAN
```

## declare

The pipeline specification, its serializations, manifest discovery, the content hash, table entries and lifecycle verbs.

- `pipeline-spec` — A specification carries `id`, `source` and `tables`, plus the optional `destination`, `schedule`, `incremental`, `transforms`, `redaction`, `normalize`, `backfill`, `seed`, `queries` and `on_table_error`.
- `serialization` — TOML and JSON deserialize into one `PipelineSpec`; the JSON Schema derived from it is the contract, and `contextful schema export` writes it to `.contextful/schema/pipeline.json`.
- `source-block` — A `source` is a connector name beside a free-form JSON config object; a `destination` carries the same two fields and defaults to the store.
- `manifest-file` — Startup reads `contextful.toml` for project config and inline `[[pipeline]]` blocks, then `pipelines/*.toml` and `pipelines/*.json`; specifications are collected by `id`.
- `duplicate-id` — One `id` declared twice raises `PipelineDuplicateId`, naming each file and the line its declaration starts on.
  *because a silent pick between two declarations hides which one runs*
- `spec-invalid` — A manifest file the canonical type cannot deserialize raises `PipelineSpecInvalid`, naming the file, the key path and the value found.
  *because a partly read manifest runs a pipeline its author did not write*
- `content-hash` — `content_hash` is the sha256 of the specification's RFC 8785 canonical JSON with every optional field at its default elided, so an explicit default hashes as its absence.
  *because a replay pin that moves on a no-op edit strands pending work*
- `table-name` — A destination table is named `<pipeline id>_<table name>`, each non-alphanumeric character folded to `_` and each ASCII uppercase letter lowered.
- `unbound-table-name` — A job target or model reference naming a destination table in a spelling the fold does not produce raises `PipelineUnboundTableName`, printing the expected spelling.
  *P1*
- `table-entry` — A `tables` entry is a bare name or an object carrying that table's configuration; the two forms mix in one array.
- `order-by-default` — `order_by` defaults to the injected ingest stamp and is inert on a keyless table; key semantics are {{store.declare.unkeyed-union}}.
- `write-mode` — `write_mode` is `append`, the default, or `replace`. Under `append` a run adds rows and no key retires; under `replace` the landing run is the table's whole current state.
- `replace-unsupported` — `replace` beside a `monotonic` or `opaque-token` cursor, a backfill chunk plan or a seed ceiling raises `PipelineReplaceUnsupported`, naming the table.
  *P1*
- `config-key` — A source config key outside the set that source enumerates raises `PipelineUnknownConfigKey` before any I/O, naming the key and the accepted keys; request headers and the forwarded guest table are exempt.
  *P1*
- `lifecycle-verbs` — `plan` diffs desired state against the store with no side effect, `--json` emitting a structured diff; `apply` converges every pipeline or one; `run` fires one pipeline once; `serve` reconciles continuously.
- `apply-fires-nothing` — Applying a manifest fires nothing of itself; a second `apply` over unchanged sources is a no-op modulo elapsed schedules.
- `table-error` — `on_table_error` is abort, the default, halting the fire at the first failing table, or continue, recording the failure and returning success with the failed run ids.
- `table-error-exit` — Under continue, `run` exits non-zero naming the failed runs, `apply` prints a partial line, and a scheduled fire appends its failure count to its log line.
- `chunked-load` — `on_table_error` governs the single-pass live pull alone; a seed or backfill chunk plan halts the fire on failure under either setting.
- `incremental-field` — `incremental = "<field>"` names the stream clock on the pipeline. Declared, the source reports a `monotonic` cursor committing {{run.advance.watermark-shape}}; undeclared, it refetches in full.

unsettled: What retires a key a source stops serving under `append`: a run declaring itself complete state, or a per-connector delete signal? owner: pipeline affects: run.declare

unsettled: Does `on_table_error` take a per-table override, and a cap on failed tables before a fire halts anyway? owner: pipeline affects: run.declare

## compile

The plan a specification becomes at build time: node kinds, version and schema.

- `authoring-surface` — The authoring surface runs at build time only and emits a content-hashed plan; nothing from it executes where the engine serves, and no build profile embeds a scripting runtime.
  *A-run*
- `plan-node` — A plan is a flat node list with predecessor edges: `step` (connector, optional retry), `sleep` (duration string), `awaitEvent` (optional timeout), `branch` (predicate, label-to-node-id map) and `parallel` (node ids).
- `plan-version` — A plan's version is the leading 16 chars of the sha256 over the RFC 8785 canonical JSON of its `{id, nodes}`.
- `plan-schema` — The plan type is defined once in a schema library; its JSON Schema is the contract every language binds to, and the run path deserializes plan JSON against it.
- `inline-step-body` — A `step` body other than a connector reference raises `PipelineInlineStepBody`.
  *A-run*
- `control-flow` — A data-dependent conditional or loop in a workflow body raises `PipelineUndeclaredControlFlow`; branching travels as `branch` and fan-out as `parallel`.
  *A-run*
- `node-id-collision` — A node id repeated in one plan raises `PipelineNodeIdCollision`, naming the id and both positions.
  *A-run*
- `lowering` — A plan lowers node by node onto {{run.journal.substrate-port}}.

## transform

The declarative chain that rewrites a batch in place.

- `chain` — The chain is an ordered list of `select`, `rename`, `cast` and a single-column `filter`, declared once per pipeline and bound to the root table.
- `arity` — A chain operation emitting more rows than it consumed raises `PipelineTransformArity`.
  *A-run*
- `filter` — A filter dropping a root row drops that row's nested children with it.
  *because a child landing under a parent id no surviving row carries is a dangling reference*
- `column-missing` — A filter or cast naming a column the batch does not carry raises `PipelineTransformColumnMissing`, printing the column and the table.
  *because a cast over an absent column otherwise lands the batch untransformed*
- `cast` — A cast rewrites one column's type and keeps its name and position.
- `projection` — `select` fixes the outgoing column set by name, and `rename` maps an incoming name onto an outgoing one.

unsettled: Does the chain grow past these four operations, or does richer work stay post-landing SQL? owner: pipeline affects: run.transform

## normalize

Canonical nested form, late relational shredding, injected identity columns and recursion depth.

- `host-stage` — Type inference over deferred-typing JSON, struct flattening, list-to-child extraction and id assignment run in engine code; no connector reimplements them and no dataframe library participates.
- `normalized-form` — The canonical form is nested Arrow structs and lists; relational shredding is a late projection at the sink, so a source landing in two sinks normalizes once.
- `mode` — `native` keeps nesting up to the sink's capability and explodes the rest with a downgrade schema-diff event; `relational` flattens structs into parent-child column names and shreds lists into child tables joined by a foreign key.
- `mode-resolution` — Mode resolves per stream per sink: explicit declaration, then sink capability, then `native`.
- `mode-unknown` — A mode outside the two raises `PipelineNormalizeModeUnknown`, printing both spellings.
  *A-run*
- `identity-columns` — Normalize injects a content-hash row id on every table, a load id on the root, a parent id and list index on each child, and a root id on a child nested deeper than one level.
- `row-id` — The row id hashes the row's own content, so a re-run of one input emits byte-identical ids.
- `list-index-missing` — A relational child table emitted without the list index that makes its projection reversible raises `PipelineListIndexMissing`, naming the parent and the list.
  *A-run*
- `nesting-depth` — Recursion stops at the declared depth, default five levels, landing a deeper subtree as one deferred-typing JSON column.

unsettled: Where does a schema-diff event land, given that the store keeps only the reconciled schema? owner: pipeline affects: run.normalize

## guard-secrets

The write-time mask over credential-shaped spans in a pulled batch.

- `placement` — The secret guard runs at the one pull path streaming and backfill share, ahead of the recorded pull and the land path, so a replay reintroduces no credential.
- `matchers` — Matchers are linear-time and regex-free, covering AWS access-key ids, PEM private keys, GitHub and Slack tokens, and `keyword=<token>` assignments; the pattern catalogue lives in code under a precision and recall fixture test.
- `mask-span` — The replacement covers only the matched byte ranges, widened to character boundaries; overlapping spans merge under the higher-priority pattern.
- `mask-replacement` — The replacement is the fixed `[REDACTED:secret]` marker, never a shape-preserving transform; an assignment keeps its `key=` prefix.
- `mask-only` — The guard is on by default and blocks no run; each pull logs the count of masked cells per column.
- `coverage` — The guard reads pre-normalize string cells for plaintext shapes; encoded material and a credential split across two cells pass through.

unsettled: Is the credential pattern set host-owned, or extensible per deployment, and does a strict mode fail the pull? owner: pipeline affects: run.guard-secrets

## land

The stage order from pull to commit, the one destination, the ingest tally and containment of unreadable input.

- `stage-order` — A batch passes pull, the secret guard, the recorded pull, normalize, the transform chain, write-path redaction, shredding and batch write; the run then commits rows and position together through {{run.advance.commit-with-rows}}.
- `unknown-destination` — The local context store is the only destination, and synthesized artifacts write back through it; any other `destination` raises `PipelineUnknownDestination` at assembly, before any row moves.
  *A-topology*
- `no-host-arm` — The destination world declares no host arm, so a guest supplies no destination.
  *A-topology*
- `batch-write` — A landing table is created on first sight of its schema, {{store.reconcile.first-sight}}; each batch is written durably in its own call, optionally carrying its ordinal as the join key onto the run's request ledger.
- `irreconcilable-schema` — An arriving schema the store cannot reconcile fails the batch as {{store.reconcile.incompatible}}.
- `commit-visibility` — A commit makes a run's rows visible for one table in one step; a crash before it leaves a recoverable partial run.
- `ingest-tally` — A fire reports `fetched`, `kept`, `skipped`, `failed`, `dropped_low_quality` and a per-source breakdown; a non-zero `failed` exits non-zero.
- `unreadable-input` — Input a parser cannot read raises `PipelineUnreadableInput`, naming the path and the position inside it, permanent against the retry schedule and failing one table's pull.
  *A-connector*
- `partial-parse` — A reader stopping partway through a multi-part input raises `PipelinePartialParse` over the whole input and lands none of its parts.
  *A-connector*
- `parse-boundary` — A decode that can die runs outside the serving process, which bounds its wall clock and memory and makes it killable.
  *A-connector*
- `parse-crashed` — A non-zero exit or fatal signal from the decode process raises `PipelineParseCrashed` naming the input; no input ends the serving process.
  *A-connector*
- `table-failed` — A table whose pull fails raises `PipelineTableFailed` carrying the table, the error kind and the run id; the fire then follows `on_table_error`.
  *because a failure is answerable by name only when it carries its table and run*

```mermaid
flowchart LR
  subgraph RUNC["run contract"]
    P[pull] --> G[secret guard]
    G --> J[(recorded pull)]
    J --> N[normalize]
    N --> T[transform chain]
    T --> R[write-path redaction]
    R --> S{sink capability}
    S -->|native| W[batch write]
    S -->|relational| X[shred to child tables] --> W
  end
  subgraph STORE["store contract"]
    C["commit marker · rows + position"]
    K[(catalog cache)]
  end
  W --> C
  C --> K
```

unsettled: Which process carries the parse boundary, a child per input or one long-lived extractor, and what wall-clock and memory budget does one input receive? owner: pipeline affects: run.land

unsettled: Does the engine read a source's declared schema at planning time, or only the observed batch at write time? owner: pipeline affects: run.land

## backfill

Phases, chunk plans, fenced chunk leases and the rewind window.

- `phase` — The catalog records one phase per pipeline: `planning` computes the chunk plan with nothing fetched, `seeding` loads consumer-held history, `backfilling` runs chunks across ticks, `streaming` pulls the incremental delta.
- `chunk-plan` — Plan shape follows cursor kind: `monotonic` plans independent time-window or id-range chunks, `opaque-token` one sequential chunk walked in order, `snapshot-id` version ranges sequential per stream and parallel across streams.
- `chunk-parallelism` — A sequential chunk plan declared beside a parallelism above one raises `PipelineChunkParallelism`, naming the plan and the value.
  *because two workers walking one continuation token skip or duplicate pages silently*
- `chunk` — A chunk row carries pipeline id, chunk id, predicate, status, attempt count, start and completion instants and a cursor-committed flag; an interrupted plan resumes at the first chunk not done.
- `chunk-lease` — A worker leases a chunk row, recording node id and attempt, for a 300 s time-to-live renewed every 60 s, and writes under a path segmented by run, node, chunk and attempt.
  *because an unquantified lease cannot be tested against a paused worker*
- `fenced-commit` — A chunk commits in one catalog update conditioned on `attempt = :mine AND status != 'done'`; an update matching no row leaves that worker's files uncommitted.
  *because an expired holder otherwise commits over its successor*
- `commit-signal` — The catalog row, not directory presence, is the commit signal; files under a path with no done row are collected by a retention window.
- `attempt-isolation` — An expired lease lets a second worker claim the chunk, increment the attempt and write under its own path.
- `tick` — While a pipeline backfills, a tick dispatches up to the per-tick chunk cap or no-ops when the queue is full; the declared cadence resumes once the position catches up.
- `block` — `[pipeline.backfill]` declares `max_chunks`, the bound on a plan's chunk count; `chunk_size`, one clock-ranged chunk's width in cursor units; and `start_from`, the floor, default zero.
- `rewind` — A rewind returns every chunk overlapping the half-open range to pending and appends one reason row per rewound chunk to an audit log a catalog rebuild leaves untouched.
- `rewind-invalid` — An inverted, empty or scale-incomparable rewind window raises `PipelineRewindWindowInvalid`; a window overlapping no chunk is a no-op.
  *P4*

```mermaid
stateDiagram-v2
  [*] --> planning
  planning --> seeding: seed block declared
  planning --> backfilling: no seed block
  seeding --> backfilling: seed committed
  backfilling --> streaming: position caught up
  streaming --> backfilling: rewind window opened
```

unsettled: What is the per-tick chunk cap, and how long does the retention window keep files under a path with no done row? owner: pipeline affects: run.backfill

## seed

The bulk-load source mode, its ceiling, its scope and the parity it guarantees.

- `block` — `[pipeline.seed]` attaches a bulk-load source with a ceiling `below` expressed on the seeded table's `order_by` scale.
- `one-land-path` — Seeded rows travel {{run.land.stage-order}} unchanged; attribution survives only in the load id and a run recorded under the `seeding` phase.
- `declaration-missing` — A seeded table without `primary_key`, or with `order_by` left at the ingest stamp, raises `PipelineSeedDeclarationMissing` at plan and validate.
  *A-run*
- `ceiling-breached` — A seeded row whose ordering stamp reaches `below` raises `PipelineSeedCeilingBreached` naming the value; its chunk lands nothing, and the stamp is neither clamped nor dropped.
  *A-run*
- `ceiling-point` — The ceiling is evaluated on the root batch after normalize and the chain, before the write.
- `ceiling-unevaluable` — A batch missing the ordering column, or a stamp unorderable against the ceiling, raises `PipelineSeedCeilingUnevaluable`.
  *A-run*
- `scope` — Seed chunk and cursor state live under a `<table>#seed` scope apart from the live pipeline's.
- `connector-pin` — A seeding run is exempt from {{run.own.pinned-plan-changed}} and records its connector identity as provenance.
- `ceiling-binding` — On seed commit, an unset `monotonic` live position takes the ceiling when `below` parses as an integer; under the other cursor kinds the live position stays unset and starts from the source's beginning.
- `fingerprint` — Each run probes the seed source for a cheap fingerprint, a stat or a HEAD and never a download, and compares it with the one stamped at commit.
- `source-changed` — A differing fingerprint raises `PipelineSeedSourceChanged` naming both values; an equal one skips the load, and an unavailable one skips with a warning.
  *A-run*
- `commands` — `seed status` prints each seeded table's ceiling, commit state and fingerprint match; `seed reset <pipeline> [--table T]` clears the seed's chunk plan.
- `parity` — Over keys the vendor still returns, seeding then running live across an overlapping window yields a pure live backfill's row count and per-key winners; a key the vendor stopped returning keeps its seeded value.
- `parity-audit` — `validate` audits the seed ordering over every landed row, not over the deduped view.
- `compaction-cadence` — A seeded pipeline declares a scheduled fold covering each seeded table.
  *because without a snapshot the seeded and live rows for one key both survive the union read*

## publish

A published table's contract identity, build, freshness and holds, committed with its snapshot manifest.

- `manifest-commit` — A published table's contract identity, freshness and current build ride the snapshot manifest commit that publishes its data; no separate file is authoritative for any of them.
  *because separately written artifacts make a torn publication representable*
- `contract-identity` — A published table's identity is `contract_version` beside `schema_fingerprint`, taken over the declared columns, their types and the grain.
- `contract-mismatch` — A materialization whose columns, types or grain miss the declared contract raises `PipelineContractMismatch`, naming the column and the expectation.
  *P4*
- `staging` — A build materializes into staging and publishes through {{run.publish.manifest-commit}}; a refused build leaves the last published state serving.
  *P4*
- `history-logs` — `contract-history.jsonl`, `builds.jsonl` and `holds.jsonl` are append-only history derived from committed manifests; a log disagreeing with a manifest is regenerated from it.
- `build-entry` — A build entry carries build id, start and completion instants, a status of published, refused or partial, the contract identity, and the partition values it left unfilled.
- `freshness` — Freshness carries the newest publishing build id, its watermark, `max_lag`, the last build status and a withheld-cells flag; staleness is derived from watermark against `max_lag` and never stored.
- `hold` — A hold records build id, placing principal and expiry; collection skips a held build, and a hold confers no other authority.
- `manifest-section` — The manifest section carries `{contract_version, schema_fingerprint, build_id, last_built_at, watermark, max_lag, last_build_status, partitions_failed?, semantics_version?, fingerprint_recipe?}` of the newest publishing build, an absent optional key omitted.
- `semantics-version` — `semantics_version` advances when the engine adds an injected column, and `fingerprint_recipe` names the fingerprint's inputs, that column included.
- `disclosure-digest` — A build records a digest over its declared disclosure policy, set-valued fields sorted, in the manifest and in the build log.

```mermaid
flowchart LR
  subgraph RUNC["run contract"]
    B["build"] --> ST["materialize into staging"]
    ST --> CK{"columns, types, grain match the declared contract?"}
    CK -->|no| REF["PipelineContractMismatch · last published state keeps serving"]
    HOLD["hold · build id, principal, expiry"]
  end
  subgraph STORE["store contract"]
    MC["snapshot manifest commit · data, contract identity, freshness, build"]
  end
  CK -->|yes| MC
  MC --> LOGS["contract-history, builds, holds logs · derived from manifests"]
  HOLD -.->|"collection skips"| MC
```

## Shapes

A pipeline declaration:

```toml
[[pipeline]]
id = "meta-ads"
incremental = "updated_time"
on_table_error = "abort"

[pipeline.source]
name = "http"
config = { endpoint = "https://api.example.test/v19.0", format = "json" }

[[pipeline.tables]]
name = "insights"
primary_key = ["ad_id", "date_start"]
order_by = "date_start"
write_mode = "append"

[[pipeline.transforms]]
op = "cast"
column = "spend"
to = "float64"

[pipeline.backfill]
max_chunks = 512
chunk_size = "7d"

[pipeline.seed]
below = "2025-06-01T00:00:00Z"
source = { name = "file", config = { root = "exports/meta-ads", format = "jsonl" } }
```
