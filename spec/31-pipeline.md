---
contract: run
owns:
  - declare
  - compile
  - transform
  - normalize
  - guard-secrets
  - land
  - export
  - backfill
  - seed
  - model
  - publish
---

# Pipelines and the write path

A pipeline declares desired state over one source and the tables it lands. This file holds
that declaration, the plan it compiles to, the stages a batch passes between a source and a
committed run, and what a published table carries. `land` is the one statement of stage order.

From declaration to a published table, and the contracts each step meets:

```mermaid
flowchart LR
  MF[("pipeline manifests")] -->|"declare"| SPEC
  subgraph CONN["connector contract"]
    SRC["connector source"]
  end
  subgraph RUNC["run contract"]
    SPEC["PipelineSpec"]
    PLAN["plan"]
    RUN["run substrate"]
    ST["staging area"]
    LAND["land path"]
  end
  subgraph STORE["store contract"]
    CH[("chunk plan")]
    CS[("context store")]
    MAN[("snapshot manifest")]
  end
  SPEC -->|"compile"| PLAN
  PLAN -->|"lower"| RUN
  SPEC -->|"backfill, seed"| CH
  CH -->|"chunks"| RUN
  SRC -->|"batches"| RUN
  RUN -->|"stage batch"| ST
  ST -->|"staged batch"| LAND
  LAND -->|"batch write, commit"| CS
  LAND -->|"publish"| MAN
```

## declare

The pipeline specification, its serializations, manifest discovery, the content hash, table entries and lifecycle verbs.

- `pipeline-spec` — A specification carries `id`, `source` and `tables`, plus the optional `destination`, `schedule`, `incremental`, `transforms`, `redaction`, `normalize`, `backfill`, `seed`, `queries` and `on_table_error`.
- `serialization` — TOML and JSON deserialize into one `PipelineSpec`; the JSON Schema derived from it is the contract, and `contextful schema export` writes it to `.contextful/schema/pipeline.json`.
- `source-block` — A `source` is a connector name beside a free-form JSON config object; a `destination` carries the same two fields and defaults to the store.
- `manifest-file` — Startup reads `contextful.toml` for project config and inline `[[pipeline]]` blocks, then `pipelines/*.toml` and `pipelines/*.json`; specifications are collected by `id`.
- `manifest-missing` — `pipeline validate` reading no manifest file raises `PipelineManifestMissing`, naming the declaration path.
  *because a validation over a mistyped path otherwise checks nothing and exits zero*
- `duplicate-id` — One `id` declared twice raises `PipelineDuplicateId`, naming each file and the line its declaration starts on.
  *because a silent pick between two declarations hides which one runs*
- `spec-invalid` — A manifest file the canonical type cannot deserialize raises `PipelineSpecInvalid`, naming the file, the key path and the value found.
  *because a partly read manifest runs a pipeline its author did not write*
- `content-hash` — `content_hash` is the sha256 of the specification's RFC 8785 canonical JSON with every optional field at its default elided, so an explicit default hashes as its absence.
  *because a replay pin that moves on a no-op edit strands pending work*
- `table-name` — A destination table is named `<pipeline id>_<table name>`, each non-alphanumeric character folded to `_` and each ASCII uppercase letter lowered.
- `table-name-collision` — Two tables whose destination names fold to one spelling, within one pipeline or across pipelines, raise `PipelineTableNameCollision` at validation, naming both declarations.
  *because two tables folding to one name land in one store table and interleave their rows*
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
- `table-error` — `on_table_error` is abort, the default, halting the fire at the first failing table, or continue, landing every other table and reporting the failed run ids through {{run.declare.table-error-exit}}.
- `table-error-exit` — Under continue, `run` exits non-zero naming the failed runs, and a scheduled fire appends its failure count to its log line.
- `chunked-load` — `on_table_error` governs the single-pass live pull alone; a seed or backfill chunk plan halts the fire on failure under either setting.
- `incremental-field` — `incremental = "<field>"` names the stream clock on the pipeline. Declared, the source reports a `monotonic` cursor committing {{run.advance.watermark-shape}}; undeclared, it refetches in full.
- `incremental-pointer` — An `incremental` value opening with `/` is an RFC 6901 pointer read from each fetched row, so a nested clock such as `/commit/committer/date` orders the stream and names its watermark.

unsettled: What retires a key a source stops serving under `append`: a run declaring itself complete state, or a per-connector delete signal? owner: pipeline affects: run.declare

unsettled: Does `on_table_error` take a per-table override, and a cap on failed tables before a fire halts anyway? owner: pipeline affects: run.declare

## compile

The plan a specification becomes at build time: node kinds, version and schema.

- `authoring-surface` — The authoring surface runs at build time only and emits a content-hashed plan; nothing from it executes where the engine serves, and no build profile embeds a scripting runtime.
  *A-run*
- `plan-node` — A plan is a flat node list with predecessor edges: `step` (connector, optional retry), `sleep` (duration string), `awaitEvent` (optional timeout), `branch` (predicate, label-to-node-id map), `parallel` (node ids) and `join` (parallel node id, `allow_partial`).
- `plan-version` — A plan's version is the leading 16 chars of the sha256 over the RFC 8785 canonical JSON of its `{id, nodes}`.
- `plan-schema` — The plan type is defined once in a schema library; its JSON Schema is the contract every language binds to, and the run path deserializes plan JSON against it.
- `inline-step-body` — A `step` body other than a connector reference raises `PipelineInlineStepBody`.
  *A-run*
- `control-flow` — A data-dependent conditional or loop in a workflow body raises `PipelineUndeclaredControlFlow`; branching travels as `branch` and fan-out as `parallel`.
  *A-run*
- `node-id-collision` — A node id repeated in one plan raises `PipelineNodeIdCollision`, naming the id and both positions.
  *A-run*
- `lowering` — A plan lowers node by node onto {{run.journal.substrate-port}}.
- `unjoined-parallel` — A `parallel` node no `join` node names, or a node other than its join reading it or one of its bodies, raises `PipelineParallelUnjoined` at compile.
  *because an output read around its join reaches a partial result no run record marks as partial*
- `branch-arm` — A `branch` node records, once, its predicate node's output trimmed of surrounding whitespace as the arm label, so every replay takes the arm the first attempt took; the other arms' nodes are skipped.
- `branch-scope` — Each body of a `parallel` node runs under step labels prefixed by the parallel node's id, and its outcome reaches only the join, as {{run.journal.fan-out-join}} states.

## transform

The declarative chain that rewrites a batch in place.

- `chain` — The chain is an ordered list of `select`, `rename`, `cast`, `extract` and a single-column `filter`, declared once per pipeline and bound to the root table.
- `arity` — A chain operation emitting more rows than it consumed raises `PipelineTransformArity`.
  *A-run*
- `filter` — A filter dropping a root row drops that row's nested children with it.
  *because a child landing under a parent id no surviving row carries is a dangling reference*
- `column-missing` — A value filter or cast naming a column the batch does not carry raises `PipelineTransformColumnMissing`, printing the column and the table.
  *because a cast over an absent column otherwise lands the batch untransformed*
- `cast` — A cast rewrites one column's type and keeps its name and position.
- `projection` — `select` fixes the outgoing column set by name, and `rename` maps an incoming name onto an outgoing one.
- `typed-cast` — A cast to `binary`, `binary(n)`, `float32[n]` or `float16[n]` keeps a value that type's JSON form reads, nulls any other, and lands the column in that type.
- `type-carry` — A `rename` carries a pulled column type to the new name, a `select` drops it with its column, and a cast to a scalar type drops it.
- `extract` — `extract` copies the value at an RFC 6901 pointer into a named column, null where the pointer names nothing, keeps the source column, and lands the new column with no pulled type.
- `presence-filter` — A `filter` naming `absent` keeps the rows whose column is missing or null, and one naming `present` keeps the rest; neither requires the batch to carry the column.

unsettled: Does the chain grow past these five operations, or does richer work stay post-landing SQL? owner: pipeline affects: run.transform

## normalize

Canonical nested form, late relational shredding, injected identity columns and recursion depth.

- `host-stage` — Type inference over deferred-typing JSON, struct flattening, list-to-child extraction and id assignment run in engine code; no connector reimplements them and no dataframe library participates.
- `normalized-form` — The canonical form is nested Arrow structs and lists; relational shredding is a late projection at the sink, so a source landing in two sinks normalizes once.
- `mode` — `native` keeps nesting up to the sink's capability and explodes the rest with {{run.record.schema-diff-home}}; `relational` flattens structs into parent-child column names and shreds lists into child tables joined by a foreign key.
- `mode-resolution` — Mode resolves per stream per sink: explicit declaration, then sink capability, then `native`.
- `mode-unknown` — A mode outside the two raises `PipelineNormalizeModeUnknown`, printing both spellings.
  *A-run*
- `identity-columns` — Normalize injects a content-hash row id on every table, a load id on the root, a parent id and list index on each child, and a root id on a child nested deeper than one level.
- `field-collision` — A relational source field flattening onto an occupied column takes the first free `source_`-prefixed name, preserving injected identity columns and the source value.
- `row-id` — The row id hashes the row's own content, so a re-run of one input emits byte-identical ids.
- `list-index-missing` — A relational child table emitted without the list index that makes its projection reversible raises `PipelineListIndexMissing`, naming the parent and the list.
  *A-run*
- `native-store` — On the store sink, `native` lands each undeclared column of objects and arrays as a struct or list column inferred over the batch; a column mixing kinds, or one `schema.json` holds as a scalar, lands as `Json`.
  *A-store*
- `nesting-depth` — Recursion stops at the declared `depth`, default 5 levels, landing a deeper subtree as one `Json` value.

#### Scenarios

- `run.normalize.mode`: WHEN a relational stream holds a struct and a two-item list, THEN the store holds flattened parent columns and two indexed child rows joined to that parent.
- `run.normalize.mode`: WHEN a native sink cannot retain a nested column, THEN the committed table's run manifest carries that column's downgrade event.

## guard-secrets

The write-time mask over credential-shaped spans in pulled batches and registered body results.

- `placement` — The secret guard runs at the one pull path streaming and backfill share, ahead of the recorded pull and the land path, so a replay reintroduces no credential.
- `matchers` — Matchers are linear-time, regex-free forward scans, each anchored on a literal prefix; the credential catalogue lives in code under a precision and recall fixture test.
  *A-run*
- `llm-candidate` — An LLM candidate starts with `sk-` at input start or after a byte outside ASCII alphanumerics, `-` and `_`; its tail spans that alphabet. Admission masks the whole candidate.
  *A-run*
- `llm-admission` — An LLM candidate needs 32 B of tail: a `proj-`, `svcacct-`, `service-`, `admin-` or `ant-` prefix permits the whole tail; otherwise its first 32 bytes are ASCII alphanumerics.
  *A-run*
- `assignment-key` — An assignment key qualifies when it is a keyword or ends in one after `_`, `-` or `.`, reading `-` as `_`, so `client_secret`, `x-api-key` and `db.password` qualify and `clientsecret` does not.
- `mask-span` — The replacement covers only the matched byte ranges, widened to character boundaries; overlapping spans merge under the higher-priority pattern.
- `mask-replacement` — The replacement is the fixed `[REDACTED:secret]` marker, never a shape-preserving transform; an assignment keeps its `key=` prefix.
- `mask-only` — The guard is on by default and blocks no run; each pull logs the count of masked cells per column.
  *A-run*
- `coverage` — The guard reads pre-normalize string cells for plaintext shapes; encoded material and a credential split across two cells pass through.
- `recorded-body` — Registered body results pass through the same guard before shaping and canonical preparation; the engine reports per-column counts once per fresh paid preparation, excluding counts from the recorded wire and replay.
  *A-authority*

#### Scenarios

- `run.guard-secrets.assignment-key`: WHEN a cell holds `x-api-key: zK9s8d7f6g5h`, THEN it lands as `x-api-key: [REDACTED:secret]`.
- `run.guard-secrets.assignment-key`: WHEN a cell holds `token_type=abcdefgh12`, THEN it lands unchanged.

## land

The stage order from pull to commit, the one destination, the ingest tally and containment of unreadable input.

- `stage-order` — A batch passes pull, the secret guard, the recorded pull, normalize, the transform chain, write-path redaction, shredding and batch write; the run then commits rows and position together through {{run.advance.commit-with-rows}}.
- `unknown-destination` — The local context store is the only destination, and synthesized artifacts write back through it; any other `destination` raises `PipelineUnknownDestination` at assembly, before any row moves.
  *A-topology*
- `no-host-arm` — The destination world declares no host arm, so a guest supplies no destination.
  *A-topology*
- `batch-write` — A landing table is created on first sight of its schema, {{store.reconcile.first-sight}}; each batch is written durably in its own call, optionally carrying its ordinal as the join key onto the run's request ledger.
- `typed-pull` — A pull's `types` object maps a column to a type spelled as {{store.reconcile.typed-landing}} reads it, and the run commit lands that column in it; an unreadable spelling fails the pull as {{store.reconcile.incompatible}}.
- `late-type` — A pull declaring a type for a column an earlier staged batch of its run carried undeclared raises `PipelineTypeDeclaredLate`, deterministic, naming the column; the run commits nothing and retires its owner.
  *because that batch's part already holds the column in its inferred type, and a staged part is immutable*
- `irreconcilable-schema` — An arriving schema the store cannot reconcile fails the batch as {{store.reconcile.incompatible}}.
- `commit-visibility` — A commit makes a run's rows visible for one table in one step; a crash before it leaves a recoverable partial run.
- `session` — A landing session holds one store, node and table declaration across runs and commits each micro-batch as its own run; a later run rewrites no earlier run's parts or manifest.
  *because a writer flushing every second otherwise pays a process start, a store open and a node resolution per batch*
- `session-input` — `contextful context land --session` reads one JSON object per stdin line — `run_id`, `rows`, optional `now` — lands each as its own run through one landing session, and prints that run's outcome before reading the next line.
  *because a host running no serving process still needs one session across its batches*
- `session-schema` — A landing session parses `schema.json` again only when its version — length, modification instant, file identity — moves; a land adding no column or type replaces no `schema.json` and takes no schema lock.
  *because a land per second otherwise rereads and rewrites the schema under the lock every writer of the table waits on*
- `receive-entry` — A delivered batch enters at the pull position of {{run.land.stage-order}}: each declared table it touches lands as one run whose single pull yields that table's rows.
  *because a pushed row needs the guard, the recorded pull and redaction a pulled row passes*
- `receive-ack` — `contextful pipeline receive <id> --key <key>` reads one batch on stdin and answers after every touched table commits, in declaration order, naming the batch id and each table's run id; a failed table answers retryable only under {{run.retry.retryable-classes}}.
  *because no commit spans two tables, so a sender learns success only once the last table commits*
- `receive-batch-key` — A batch key fixes each touched table's run id stem `receive-<digest>.<table>`, a later attempt taking `.2`, `.3`, and its owner scope; a table whose run under the key committed replays it and lands nothing.
  *because a sender retrying after a lost answer must not duplicate committed tables*
- `receive-resume` — A table under a batch key whose run did not commit resumes the key's pending owner, recorded pull included, and no other key's.
  *because a sender retrying after a lost answer must not lose uncommitted tables*
- `receive-batch-too-large` — A body past the source's `max_batch_bytes` raises `ReceiveBatchTooLarge`, non-retryable; the reader stops one byte past the bound and no table lands.
  *because an unbounded body is an unbounded allocation the sender controls*
- `receive-queue` — Batches of one pipeline land one at a time; up to `max_queue_depth` batches hold a queue slot, waiting or landing, from before the body is read until the answer.
  *because concurrent senders share each table's single-writer cursor lease*
- `receive-backpressure` — A batch arriving while every queue slot of its pipeline is held raises `ReceiveBackpressure`, retryable, before reading its body.
  *because a sender told to wait sheds load the receiver cannot absorb*
- `receive-retry-after` — A {{run.land.receive-backpressure}} answer carries a retry interval of 1 s.
- `receive-write-not-granted` — A credential lacking `write` over any declared table of a `receive` pipeline raises `ReceiveWriteNotGranted` before the body is read, and no table lands.
  *because a sender whose grant falls short learns it before spending the transfer*
- `receive-key-expired` — A batch key replays or resumes only within the source's `key_retention_secs` of its first attempt; a later batch under it raises `ReceiveBatchKeyExpired`, non-retryable, before the body is read, and no table lands.
  *A-run*
- `receive-endpoint` — `POST /receive/:pipeline` on the served face takes the batch key in the `Contextful-Batch-Key` header, admits the credential before reading the body, and answers `200` with the {{run.land.receive-ack}} answer.
  *because a sender refused for its credential or grant must not have spent the transfer*
- `receive-endpoint-status` — The receive route answers `403` for {{run.land.receive-write-not-granted}}, `413` for {{run.land.receive-batch-too-large}}, `410` for {{run.land.receive-key-expired}}, `503` with `Retry-After` for {{run.land.receive-backpressure}} or a retryable failed table, and `422` otherwise.
- `ingest-tally` — A fire reports `fetched`, `kept`, `skipped`, `failed` and `dropped_low_quality`, the rows its declared `filter` operations drop, in total and per table; a non-zero `failed` exits non-zero.
  *because a fire pulls one declared source through its tables, and a `filter` is the one stage that drops a fetched row*
- `unreadable-input` — Input a parser cannot read raises `PipelineUnreadableInput`, naming the path and the position inside it, permanent against the retry schedule and failing one table's pull.
  *A-connector*
- `partial-parse` — A reader stopping partway through a multi-part input raises `PipelinePartialParse` over the whole input and lands none of its parts.
  *A-connector*
- `parse-boundary` — A decode that can die runs outside the serving process, which bounds its wall clock and memory and makes it killable.
  *A-connector*
- `parse-child` — Each input whose decoder can die runs in its own child process; that process decodes no second input.
  *A-run*
- `parse-deadline` — A decode runs for at most 60 s from child spawn; expiry kills its process group and raises {{run.land.parse-crashed}}.
  *A-run*
- `parse-memory` — A decoder child receives at most 512 MiB of memory; exceeding the bound fails that input under {{run.land.parse-crashed}}.
  *A-run*
- `parse-crashed` — A non-zero exit or fatal signal from the decode process raises `PipelineParseCrashed` naming the input; no input ends the serving process.
  *A-connector*
- `table-failed` — A table whose pull fails raises `PipelineTableFailed` carrying the table, the error kind and the run id; the fire then follows `on_table_error`.
  *because a failure is answerable by name only when it carries its table and run*
- `observed-schema` — Planning fetches no source schema; each observed batch is typed when staged under {{run.own.stage-schema}}, including validation against declared columns.
  *A-run*

```mermaid
flowchart LR
  subgraph RUNC["run contract"]
    SRC["connector source"] -->|"pulled batch"| G["secret guard"]
    G -->|"guarded batch"| J[("recorded pull")]
    J -->|"normalize"| T["transform chain"]
    T -->|"transformed rows"| R["write-path redaction"]
    R -->|"redacted rows"| S{"sink capability"}
    S -->|"native"| W["batch writer"]
    S -->|"relational"| X["child-table shredder"]
    X -->|"child rows"| W
  end
  subgraph STORE["store contract"]
    C["commit marker"]
    K[("catalog cache")]
  end
  W -->|"rows + position"| C
  C -->|"cached position"| K
```

#### Scenarios

- `run.land.parse-child`: WHEN two PDF inputs decode in one fire, THEN each input starts a separate child process.
- `run.land.parse-deadline`: WHEN a decoder holds its output pipe past 60 s, THEN its process group dies and that input fails by name.
- `run.land.parse-memory`: WHEN a decoder exceeds 512 MiB, THEN that input fails and the serving process continues.
- `run.land.observed-schema`: WHEN a source exposes no schema before its first batch, THEN planning succeeds and staging checks that batch against the declared table columns.
- `run.land.session-input`: WHEN stdin carries `run-1`, `run-2` and `run-1` again, THEN the session prints two commits and one replay, one line each.
- `run.land.session-schema`: WHEN a session lands 300 single-row runs under one schema, THEN it parses `schema.json` once and leaves the file's bytes and identity unchanged.
- `run.land.receive-ack`: WHEN one `jsonl` batch carries rows for two declared tables, THEN the answer follows both commits and names the batch id and each table's run id.
- `run.land.receive-entry`: WHEN a delivered row carries a credential-shaped cell, THEN the table lands it masked.
- `run.land.receive-batch-key`: WHEN a batch whose tables all committed arrives again under its key, THEN the answer names the original run id and no row lands.
- `run.land.receive-batch-too-large`: WHEN a body passes `max_batch_bytes` by one byte, THEN the receiver raises `ReceiveBatchTooLarge` and no table lands.
- `run.land.receive-backpressure`: WHEN two batches hold both slots of a depth-2 pipeline, THEN a third raises `ReceiveBackpressure` with a 1 s retry interval.
- `run.land.receive-resume`: WHEN the receiver dies after recording a delivered batch and the batch is resent under its key, THEN every landed cell but the run id is identical to the uninterrupted run's.
- `run.land.receive-write-not-granted`: WHEN a credential holds `write` over one of two declared tables, THEN the receiver refuses before reading the body and no table lands.
- `run.land.receive-key-expired`: WHEN a key's first attempt is 3601 s old under a 3600 s window, THEN the resend raises `ReceiveBatchKeyExpired` and no row lands.
- `run.land.receive-endpoint`: WHEN a request carries no credential and a body past `max_batch_bytes`, THEN the route answers `401` without reading the body.

## export

The outbound copy of a landed table to an operator-declared target: the export block, the post-commit read, the delivery batch, the cursor and at-least-once delivery.

- `export-block` — An OTLP `[[export]]` block declares `name`, one landed `table`, an HTTP `endpoint`, `signal = "logs"` and optional `headers`; `contextful export run <name>` delivers every row its cursor has not passed.
  *A-topology*
- `signal-unknown` — A `signal` other than `logs` raises `ExportSignalUnknown` before any row is read.
  *because the one built-in arm maps a row onto a log record, and a span or metric needs a column mapping no block declares*
- `post-commit-read` — Export reads committed runs only, through the read face under the admitted credential, so its grants, row policy and masks hold; no landing waits on an export.
  *A-topology*
- `commit-order` — Export reads the rows past its cursor in `_commit_seq`, then `_row_seq`, order and never by `_ingested_at`, so {{store.reserve.commit-seq}} keeps a run committing late from being skipped.
  *A-topology*
- `commit-seq-missing` — A table holding rows whose `_commit_seq` is null, from parts landed without the column, raises `ExportCommitSeqMissing` naming the export and the row count, before any batch leaves.
  *because a null never passes a cursor, and a row an export skips in silence is a row its mirror loses*
- `batch-rows` — One delivery batch holds at most 500 rows.
  *because a batch that size sits below the face row ceiling and the request limits common collectors apply*
- `log-record` — Each row becomes one OTLP log record: `timeUnixNano` from `_ingested_at`, each non-null column outside the `_` namespace an attribute, beside `contextful.table`, `contextful.run_id`, `contextful.commit_seq` and `contextful.row_seq`.
  *because the attributes a target deduplicates on travel with the record, and the engine's own columns stay its own*
- `delivery` — A batch leaves as one OTLP/HTTP JSON `POST` through the mediated client, its allowlist the endpoint's host alone, each header hydrated as {{connector.resolve.hydration-is-just-in-time}} states.
  *A-topology*
- `secret-preflight` — A header reference no adapter answers refuses at preflight as {{connector.resolve.unresolved-name}}, before any row is read.
  *because a target the export cannot authenticate to is known before a batch is built*
- `cursor-after-ack` — The cursor, the last delivered `_commit_seq` and `_row_seq`, commits only after the target answers 2xx for the batch.
  *because an unacknowledged batch remains eligible for delivery after a restart*
- `local-state` — The local storage profile keeps log cursors in `.contextful/exports/<project>/<name>.json`, outside corpus sync, and typed delivery state in `machine.sqlite`.
- `remote-state` — Under {{run.journal.container-profile}}, PostgreSQL holds typed outboxes, acknowledged keyed state and log checkpoints in the execution database; a replacement reoffers identical pending event ids before advancing its acknowledged frontier.
  *A-run*
- `d1-state` — Under {{run.journal.d1-profile}}, D1 commits export outboxes, acknowledged keyed state and log checkpoints; a sink commit without its D1 acknowledgement reoffers the same delivery identity.
  *A-run*
- `writer-fenced` — A PostgreSQL exporter whose durable writer lease has transferred raises `ExecutionWriterFenced` before dispatch or state publication; its lease row stays locked throughout an admitted outbound call, and takeover waits for that call.
  *A-run*
- `at-least-once` — A run ending between a target's acknowledgement and the cursor commit resends that batch on the next run; a target deduplicates on `contextful.commit_seq` and `contextful.row_seq`.
  *A-topology*
- `delivery-refused` — A target answering other than 2xx, or unreachable, raises `ExportDeliveryRefused` naming the export and the answer, and the cursor stays where it stood.
  *because an unacknowledged batch is resent from the cursor, never skipped*
- `typed-block` — A typed `[[export]]` block declares `format = "changes-v1"`, nonempty `key` columns and `schedule`, beside `name`, `table`, `endpoint` and optional `headers`.
  *because a consumer needs stable row identity and a delivery cadence*
- `typed-state` — A typed export compares the latest stable committed table read through {{run.export.post-commit-read}} with its last acknowledged state; commits between reads coalesce into one publication.
  *because a scheduled state export promises the observed frontier rather than every intermediate write*
- `typed-view-recheck` — A typed export re-reads its admitted table even when the source commit frontier is unchanged, and stages a publication when its key, schema, policy or visible state changes.
  *because a commit hash alone cannot identify the view delivered to a consumer*
- `typed-publication-id` — A typed publication identifier covers the source frontier, read identity and visible keyed state, so distinct delivered views carry distinct identifiers without requiring another source commit.
  *because a receiver must not mistake a changed view for a retry of the prior publication*
- `typed-events` — Each changed key yields a versioned upsert with its visible row or a deletion with its key; unchanged keys yield no event.
  *because a generic consumer applies the same keyed operations regardless of table shape*
- `typed-order` — Typed events order by encoded key and carry consecutive sequences and stable ids; a repeat delivery carries the same bytes and ids.
  *because acknowledgement can fail after the target applies a batch*
- `typed-complete` — A publication-complete event follows every change, including an empty set, and states that publication's change count.
  *because a consumer needs a boundary after which its keyed state represents one observed frontier*
- `typed-outbox` — Events and their next keyed state stage together in the configured execution store; a pending publication blocks staging a later frontier.
  *because a failed send must retain its exact payload and order across restarts*
- `typed-identity-changed` — Pending events under a changed read identity, or an export redirected to another target under the same name, raise `ExportIdentityChanged` without delivery; watch exits.
  *because an old outbox cannot prove authorization under a changed view or establish a new target's baseline*
- `typed-ack` — A typed export advances its durable acknowledgement sequence only after HTTP 2xx; acknowledging the completion event promotes its staged state in the same transaction.
  *because a receiver acknowledgement and local state promotion have one durable order*
- `typed-watch` — `contextful export watch <name>` fires the typed export on its schedule and retries a refused send with capped exponential delay from the pending outbox.
  *because delivery resumes without another operator invocation*
- `typed-byte-limit` — One typed JSON request holds at most 65536 B.
  *because a row-count bound alone admits an unbounded request body*
- `typed-event-too-large` — A typed event unable to fit one request raises `ExportEventTooLarge`; watch stops rather than retrying that event.
  *because an unchanged oversized event cannot make progress on retry*
- `group-block` — A local `[[publication]]` declares `name`, producer `job` and typed export `members`; members cover every producer output exactly once and share one destination and header binding.
  *A-run*
- `group-source` — A successful store-driven job or materialized model build records one immutable completion binding its run identifier, pinned input frontier and all output table frontiers, including empty outputs.
  *A-run*
- `group-delivery` — `contextful export group <name>` reads every completed producer member through {{run.export.post-commit-read}}, rechecks every frontier, and stages one ordered outbox with explicit member-complete events followed by one group-complete event.
  *A-run*
- `group-identity` — A group's identity binds its store incarnation, destination, namespace, read identity, predecessor, publication order, input frontier, membership, delivered base and next state; each event identifier binds that group and its sequence.
  *A-run*
- `group-activation` — A receiver stages member events transactionally and activates every member together only after validating the complete group; interrupted delivery preserves the active group, and acknowledged retries preserve its bytes and visibility.
  *A-run*
- `group-checkpoint` — `contextful export checkpoint` saves delivery state; `contextful export restore` replays compatible immutable events while the receiver retains its accepted groups and active frontier, so an older checkpoint reverses no deletion or later publication.
  *A-run*
- `group-rebuild` — `contextful export rebuild <name> --namespace <epoch>` exports the completed producer's full admitted state into a fresh namespace without executing its effects; the receiver keeps its active namespace until the replacement group completes.
  *A-run*
- `group-incompatible` — Membership drift, incarnation mismatch, a changed pending read identity, an incompatible checkpoint, an incomplete producer frontier, or a predecessor/order mismatch raises `PublicationIncompatible` before delivery or activation.
  *A-run*
- `group-storage` — A coordinated publication under `postgres` raises `PublicationStorageUnsupported` naming the coordinated-group-outbox capability before execution, staging or delivery; a `cloudflare-d1` sender state over 8 MiB raises it before staging.
  *A-run*
- `group-d1-state` — Under {{run.journal.d1-profile}}, D1 holds each group's producer frontier, sender outbox and active namespace; one D1 batch commits a completing acknowledgement with its promoted state and active namespace.
  *A-run*
- `group-d1-lease` — A `cloudflare-d1` group writer holds one durable D1 lease per publication, and every sender write predicates on that lease and the row's prior version.
  *A-run*
- `group-d1-spill` — A `cloudflare-d1` sender row over 1 MiB holds the SHA-256 of an R2 object written before the row commits; the blob sweep keeps every object a publication row references.
  *because D1 bounds each row's size, and an R2 object reached only through a committed row is never half visible*
- `group-d1-recovery` — After an empty-disk replacement, `contextful export group` resumes a staged or partially acknowledged `cloudflare-d1` group from D1, reoffering identical group and event identifiers before staging a later frontier.
  *A-run*

#### Scenarios

- `run.export.group-d1-recovery`: WHEN the container stops after D1 commits a staged group and before any delivery, THEN its replacement delivers that group's exact bytes and the receiver activates every member together.
- `run.export.group-d1-state`: WHEN a replacement loses the acknowledgement of the group-complete batch, THEN it reoffers that batch, and the receiver's accepted group absorbs the retry without a second activation.

unsettled: Does the arm map a table onto the OTLP span or metric signal, and through which column declaration? owner: pipeline affects: run.export

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
- `available-instant` — A backfill lands each row's availability column as {{store.available-time.declaration}} resolves it from the source, while `_ingested_at` stays the commit instant {{run.own.stage-instant}} writes.
  *because a 2026 backfill of 2014 history stamped with its commit instant answers no availability bound before 2026*

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
- `available-instant` — A seeded row lands its availability instant and `_ingested_at` as {{run.backfill.available-instant}} lands a backfilled row's.
- `declaration-missing` — A seeded table without `primary_key`, or with `order_by` left at the ingest stamp, raises `PipelineSeedDeclarationMissing` at plan and validate.
  *A-run*
- `ceiling-breached` — A seeded row whose ordering stamp reaches `below` raises `PipelineSeedCeilingBreached` naming the value; its chunk lands nothing, and the stamp is neither clamped nor dropped.
  *A-run*
- `ceiling-point` — The ceiling is evaluated on the root batch after normalize and the chain, before the write.
- `ceiling-unevaluable` — A batch missing the ordering column, or a stamp unorderable against the ceiling, raises `PipelineSeedCeilingUnevaluable`; a text stamp orders only when `Z`-suffixed at the ceiling's fractional width.
  *A-run*
- `scope` — Seed chunk and cursor state live under a `<table>#seed` scope apart from the live pipeline's.
- `connector-pin` — A seeding run is exempt from {{run.own.pinned-plan-changed}} and records its connector identity as provenance.
- `ceiling-binding` — On seed commit, an unset `monotonic` live position takes the ceiling when `below` parses as an integer; under the other cursor kinds the live position stays unset and starts from the source's beginning.
- `fingerprint` — Each run probes the seed source for a cheap fingerprint, a stat or a HEAD and never a download, and compares it with the one stamped at commit.
- `source-changed` — A differing fingerprint raises `PipelineSeedSourceChanged` naming both values; an equal one skips the load, and an unavailable one skips with a warning.
  *A-run*
- `commands` — `seed status` prints each seeded table's ceiling, commit state and fingerprint match; `seed reset <pipeline> [--table T]` clears the seed's chunk plan and commit stamp, so the next run loads it again.
- `parity` — Over keys the vendor still returns, seeding then running live across an overlapping window yields a pure live backfill's row count and per-key winners; a key the vendor stopped returning keeps its seeded value.
- `parity-audit` — `validate` audits the seed ordering over every landed row, not over the deduped view.
- `compaction-cadence` — A seeded table that no job meeting {{store.declare.fold-job}} covers raises `PipelineSeedCompactionMissing` at validate and before a fire, naming the table.
  *A-store*

## model

The `[[model]]` block: a table defined by SQL over store tables, its contract, freshness and tests, and the `build` verbs that materialize, publish and hold it.

- `model-block` — A `[[model]]` block carries `id`, {{run.model.statement-source}}, optional `materialized`, `unique_key`, `publish`, `[model.contract]`, `[model.freshness]`, `[[model.test]]`, {{disclosure.release.model-policy}} and {{disclosure.set-mode.opt-out-record}}; an unknown key refuses as {{run.declare.spec-invalid}}.
  *A-run*
- `statement-source` — A model declares exactly one of inline `sql` and `sql_file`; the latter reads UTF-8 text relative to the manifest containing the model.
  *because a manifest-relative statement file keeps a model portable across project roots*
- `top-level-block` — A manifest's top-level key outside the set the engine enumerates raises `PipelineUnknownBlock`, naming the key, the file and the accepted set.
  *because a misspelled block parses as nothing, and its declaration silently never runs*
- `model-id` — A model's `id` names the store table it builds; an id declared twice, equal to a pipeline destination table, or naming a table a landing wrote refuses as {{run.declare.table-name-collision}}.
- `sql` — The selected model statement is one read-only `SELECT` over store tables, admitted as {{read.guard.whole-tree-walk}} before any row is read; a model reads another model through its last published table.
- `validate-statements` — `pipeline validate` admits each model's `sql` and every test's statement as a build does, counting every relation but the model's own id in `sql` as registered, holds each declared input to {{run.model.restricted-input}}, and raises the error the build raises.
- `validate-undeclared` — `pipeline validate` names on stderr each relation a model's `sql` or test reads that no manifest table, pipeline destination or model declares, and still validates the model; `build` resolves that relation against the store.
  *because a table landed without a manifest declaration is a legal input, while a misspelled name otherwise surfaces only at `build`*
- `restricted-input` — A build reading a table that declares `class`, `policy` or `visibility` raises `ModelInputRestricted`, naming the table and the declared keys.
  *because a build reads its inputs unmasked, so the model's table serves their withheld cells to every reader*
- `materialized` — `materialized` is `table`, the default: each build replaces the model's rows whole.
- `unique-key` — `unique_key` is the model's grain; a build whose rows repeat one grain value, or hold a null in a grain column, refuses as {{run.publish.contract-mismatch}}.
- `contract-block` — `[model.contract]` declares `version`, a `<major>.<minor>.<patch>` string, and `columns`, each a `name`, a `type` spelled as {{store.reconcile.typed-landing}} reads it, and `nullable`, default true.
- `contract-required` — A model whose `publish` is true, the default, and which declares no `[model.contract]` raises `ModelContractUndeclared` at validation.
  *because a published table without a declared contract has no identity a reader can pin against*
- `unpublished` — A model declaring `publish = false` commits its rows with no manifest section, so no build entry, hold or contract history names it.
- `contract-major` — Under the published build's major, a contract that drops or retypes a published column, makes a non-null one nullable, adds a non-null column or changes the grain refuses as {{run.publish.contract-mismatch}}, naming the column and the major to bump.
  *A-run*
- `contract-minor` — Under the published build's major, a contract that only adds nullable columns or makes nullable ones non-null publishes once its minor exceeds the published one, else refuses as {{run.publish.contract-mismatch}}, naming the added or tightened column and the bumped minor.
  *A-run*
- `injected-columns` — A build lands `_ingested_at` as its start instant, `_run_id` as its build id, `_row_seq` as the row's position, `_commit_seq` as its commit's value and `_site_id`, replacing any injected column the SQL selects; `semantics_version` is 2.
- `freshness-block` — `[model.freshness]` declares `max_lag` as an integer followed by `s`, `m`, `h` or `d`; a model declaring none publishes a null `max_lag` and never computes stale.
- `test-block` — A `[[model.test]]` carries a `name` and one `SELECT` over the staged rows, registered under the model's id, and the store's tables; a test returning any row fails.
  *because a test must inspect the candidate build before its rows become visible to readers*
- `test-failed` — A failing test raises `ModelTestFailed`, naming the test and its row count; the build publishes nothing.
  *because a test exists to stop a build before readers trust it*
- `build-verb` — `contextful build <model>` materializes the model into staging, checks its contract, runs its tests, then commits; `--json` prints the build id, row count and watermark.
- `build-boundary` — A guarded model build admits its caller's execution boundary after staging and before changing the persistent schema or published pointer; a refusal discards staging and preserves prior schema, membership and watermark.
  *because a stopped execution must not publish its staged model*
- `unknown-model` — `build` naming no declared model raises `ModelUndeclared`, naming the declared models.
  *because a build of a misspelled id otherwise reports nothing to do*
- `build-frontier` — `build` reads each input at its committed frontier and fires no pipeline; a build ordered after a landing is a `build` job naming that pipeline in `after`, under {{surface.fire.build-dependency}}.
  *because `after` already orders builds behind pipelines, and a verb firing sources needs their credentials and quota, and repeats paid calls on every retry*
- `model-cycle` — A model whose `sql` reaches its own id through one or more other models raises `ModelCycle` at `pipeline validate`, import, apply and `build`, before any store is read, naming every model on the cycle.
  *because no member of a cycle has a first build, and once one exists each build folds in the other's previous output*
- `build-id` — A build's id is the snapshot id it publishes.
  *A-run*
- `watermark` — A build's watermark is `{at, inputs}`: per input table the snapshot id and the committed runs it omits, and `at` the newest commit instant among them.
- `build-horizon` — `contextful build <model> --available-as-of <t>` reads every input at `available_as_of` equal to `t` and records `t` as the watermark's `horizon`; a build without the flag records a null horizon.
  *because a build is reused across horizons only when its own bound is recorded beside it*
- `hold-verb` — `build hold --for <n>[smhd] <model> <build>` commits a hold until now plus the duration and prints `Held`, or `Renewed` over an unexpired hold; `--json` prints the receipt as an object.
- `status-verb` — `build status <model>` reports the last attempt separately from the published build; `--json` carries their ids, status, freshness and staleness computed at the requested instant.
- `hold-unknown-build` — A hold naming a build no committed manifest of the model records raises `ModelBuildUnknown`.
  *because a hold on a build that does not exist protects nothing and reads as protection*
- `hold-manifest` — A hold commits as the hold manifest `holds/<build id>.json` in the model's table directory, replacing any earlier hold on that build.
  *A-run*
- `log-regeneration` — The history logs sit in the model's table directory; each build and hold rewrites them, replacing an entry a manifest disagrees with and keeping entries of collected builds.

unsettled: Does a model materialize incrementally, merging each build into its prior one on `unique_key`? owner: pipeline affects: run.model

unsettled: Does a model over a restricted table declare its own policy, or inherit the strictest policy among its inputs? owner: pipeline affects: run.model

#### Scenarios

- `run.model.test-failed`: WHEN a test `SELECT * FROM daily WHERE n < 0` returns 2 rows, THEN the build raises `ModelTestFailed` naming the test and 2, and `daily` reads its prior build.
- `run.model.hold-verb`: WHEN `build hold --for 7d daily <build>` runs twice, THEN the first prints `Held` and the second `Renewed`, each with the new expiry.
- `run.model.model-cycle`: WHEN model `a` reads `b` and `b` reads `a`, THEN `pipeline validate` raises `ModelCycle` naming `a`, `b`, and `build a` reads no row.
- `run.model.build-frontier`: WHEN the `http` pipeline feeding a model's input points at an unreachable endpoint, THEN `build` succeeds, records no new run, and its watermark names only runs already committed.
- `run.model.build-frontier`: WHEN a `build` job naming `orders` in `after` fires in the cycle that lands `orders` run `r`, THEN the published build's watermark lists `r` under `orders_items`.
- `run.model.contract-minor`: WHEN contract `1.0.0` gains nullable column `tag` as `1.1.0`, THEN the build publishes and the manifest section carries `1.1.0` and the new fingerprint; as `1.0.0` it refuses naming `tag` and `1.1.0`.

## publish

A published table's contract identity, build, freshness and holds, committed with its snapshot manifest.

- `manifest-commit` — A published table's contract identity, freshness and current build ride the snapshot manifest commit that publishes its data; no separate file is authoritative for any of them.
  *because separately written artifacts make a torn publication representable*
- `contract-identity` — A published table's identity is `contract_version` beside `schema_fingerprint`, taken over the declared columns, their types and the grain.
- `contract-mismatch` — A materialization whose columns, types or grain miss the declared contract raises `PipelineContractMismatch`, naming the column and the expectation.
  *P4*
- `staging` — A build materializes into staging and publishes through {{run.publish.manifest-commit}}; a refused build leaves the last published state serving.
  *P4*
- `history-logs` — `contract-history.jsonl`, `builds.jsonl` and `holds.jsonl` are append-only history derived from committed manifests and build-attempt records; a log disagreeing with a source record is regenerated from it.
- `build-entry` — A build entry carries build id, start and completion instants, a status of published, refused, partial or failed, the contract identity, and the partition values it left unfilled.
- `failed-attempt` — A published model records an attempt before materialization; a dead attempt without a committed manifest reports `failed`, keeps the prior published build serving, and contributes a failed build entry.
  *because a process killed before publication cannot write its own failure, while its attempt record and released lock survive*
- `freshness` — Freshness carries the newest publishing build id, its watermark, `max_lag`, the last build status and a withheld-cells flag; staleness follows {{run.publish.staleness}} and is never stored.
- `staleness` — A published model is stale at `t` when an input holds a committed run that neither its watermark snapshot nor listed runs include, committed before `t` minus `max_lag`; `build status --json` names each such input under `stale_inputs`.
  *A-run*
- `staleness-model-input` — An input that is a model also waits on each of its published builds newer than the watermark snapshot, aged by that build's `created_at`.
  *A-run*
- `staleness-collected-run` — A waiting run whose manifest retention collected counts as committed before `t` minus `max_lag`.
  *A-run*
- `hold` — A hold records build id, placing principal and expiry; collection skips a held build, and a hold confers no other authority.
- `manifest-section` — The manifest section carries `{contract_version, schema_fingerprint, build_id, build_started_at, last_built_at, watermark, max_lag, last_build_status, withheld_cells, disclosure_digest, partitions_failed?, semantics_version?, fingerprint_recipe?}` of the newest publishing build; `watermark` maps each input table to its snapshot and omitted runs.
- `semantics-version` — `semantics_version` advances when the engine adds an injected column, and `fingerprint_recipe` names the fingerprint's inputs, that column included.
- `disclosure-digest` — A build records a digest over the `class`, `policy` and `visibility` its table declares, set-valued fields sorted, in the manifest and the build log, and sets `withheld_cells` when any is declared.

```mermaid
flowchart LR
  subgraph RUNC["run contract"]
    B["pipeline build"] -->|"materialize"| ST[("staging tables")]
    ST -->|"staged tables"| CK{"matches the declared contract?"}
    CK -->|"no: PipelineContractMismatch"| KEEP["last published state"]
    HOLD["collection hold"]
  end
  subgraph STORE["store contract"]
    MC[("snapshot manifest")]
  end
  CK -->|"yes, commit"| MC
  MC -->|"derives"| LOGS[("history logs")]
  HOLD -.->|"collection skips"| MC
```

#### Scenarios

- `run.publish.staleness`: WHEN no input lands after a build and `max_lag` is `1h`, THEN `build status` at 100 days later reports `stale` false.
- `run.publish.staleness`: WHEN `events` lands a run 1 second past the build and `max_lag` is `1h`, THEN status reports fresh at 1 hour after the landing and stale, `stale_inputs` `["events"]`, 1 second later.
- `run.publish.staleness`: WHEN a fold folds the runs a build read into a new snapshot, THEN status still reports the build fresh.
- `run.publish.staleness-model-input`: WHEN `daily` rebuilds after `weekly` read it and `max_lag` is `1d`, THEN `weekly` reports stale, `stale_inputs` `["daily"]`, 1 day after the rebuild.

unsettled: Is a build with a non-null horizon stale by landings committed after its horizon? owner: pipeline affects: run.publish

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

[[job]]
name = "nightly-fold"
schedule = "0 3 * * *"
kind = "fold"
target = "meta_ads_insights"
```

A model over a landed table, and the verbs that build and hold it:

```toml
[[model]]
id = "daily_filings"
sql = "SELECT CAST(date_trunc('day', updated_at) AS VARCHAR) AS day, count(*) AS n FROM filings_records GROUP BY 1"
unique_key = ["day"]

[model.contract]
version = "1.0.0"
columns = [
  { name = "day", type = "utf8", nullable = false },
  { name = "n", type = "int64", nullable = false },
]

[model.freshness]
max_lag = "1d"

[[model.test]]
name = "counts-positive"
sql = "SELECT * FROM daily_filings WHERE n <= 0"
```

```sh
contextful build daily_filings --json
contextful build hold --for 7d daily_filings <build id> --json
```
