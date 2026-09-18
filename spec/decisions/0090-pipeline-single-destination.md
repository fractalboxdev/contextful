# 0090 — The local context store is the only destination a pipeline resolves

**Status:** accepted 2026-09-18
**Decides:** `pipeline.declare.refusal.destination`

## Context

A specification carries a `destination` block shaped exactly like its `source` block — a name
beside a free-form config object. The symmetry invites a general sink interface: land these
tables into the store, or into a file tree, or into an embedded database, or post them at an
endpoint.

What the run path can honor decides which of those are coherent. A run's guarantee is that rows
and position commit in one order: the batch becomes durable, then the position moves as its own
recorded step, so a process that dies between them re-reads from the position last committed.
Re-reading means re-landing, and re-landing is harmless precisely because the destination is a
store whose fold is idempotent over content-hashed row ids. That property is what turns
at-least-once execution into effectively-once data.

A destination that cannot be rewritten breaks the arrangement rather than extending it. Delivery
at an endpoint is at-least-once messaging with its own retry, dedupe and ordering rules; an
outage becomes a failed run, and a retry becomes a second send the receiver must reconcile. The
engine would then own a delivery semantic it makes no guarantee about, on a path whose crash
behavior is fixed.

The file-shaped destinations fail differently: they are not new capability. The store's canonical
layout already is a columnar file tree, and the query engine reads that tree in place. A
columnar-file destination copies what already exists into a second copy that immediately begins
to drift; an embedded-database destination re-imports rows the engine can already read.

## Decision

A `destination` naming anything other than the local store raises `PipelineUnknownDestination`
while the pipeline is assembled, ahead of any row movement. Synthesized artifacts — a derived
table, a summary, a model's output written back — travel through that same destination and
become queryable corpus like any landed table. The block stays in the specification and stays
shaped like a source, carrying config for the one destination it resolves.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **The local store, refused at assembly for anything else** *(chosen)* | One commit protocol, one idempotent fold, one place a run's rows can be. | A foreign layout another team's tooling owns has no supported path; something outside the engine reads the landed rows and writes it. |
| A columnar-file destination | Rows land in a tree other tools already read. | Lost on duplicating a path that already exists: the store's canonical layout is that tree, so this is a copy that drifts from its original with no compensating capability. |
| An embedded-database destination | A familiar local query surface over the landed rows. | Lost on duplicating a path that already exists, identically: the engine reads the canonical tree in place, so the import buys a second representation and a second schema to keep in step. |
| A webhook or message destination | Landed rows reach an external system without a second process. | Loses on delivery semantics: external delivery is at-least-once messaging with its own retry and dedupe rules, so an endpoint outage becomes a failed run and a crash-resume becomes a double-send the engine cannot make idempotent. |
| A general sink plugin interface | Any destination becomes someone else's problem to implement. | Loses on the same criterion, and adds a contract the engine would have to state and cannot: what a sink must guarantee for the commit-then-advance order to remain correct. |

## Criteria

1. **Whether a destination adds a delivery semantic the run model cannot honor.** *This is the
   criterion that decided it.* The commit-then-advance order is the run path's central promise,
   and it survives a crash only where re-landing is a no-op. A destination that cannot be
   rewritten makes the promise conditional on the sink, which means the engine would be
   guaranteeing something it does not control.
2. **Whether the destination duplicates a path that already exists** — the test the two
   file-shaped options fail.
3. **Surface area to specify and maintain** — every destination is a commit protocol, an error
   taxonomy and a set of pins.
4. **Reach of the landed data** — how much work sits between a landed row and a consumer outside
   the engine. This is the criterion the chosen option loses on.

## Consequences

Every run's failure modes are the store's failure modes, and a replay after a crash is provably
safe without asking where the rows went. A synthesized artifact is a table like any other, so
derived output is queryable, foldable and time-travelable with no second mechanism.

The cost accepted is that the engine delivers nothing outward. A team whose warehouse or
downstream service needs these rows writes a reader against the store and owns the delivery,
including its retries and its ordering. That is real work pushed outside the system, and for a
consumer that only ever wanted rows posted at an endpoint it is the whole integration. The
`destination` block's shape holds the door open, but nothing behind it is specified.

## Revisit triggers

- A destination appears whose write is genuinely idempotent under the same commit-then-advance
  order — an upsert against a keyed remote table, for instance — which removes the delivery
  objection rather than accepting it.
- Outward delivery becomes a first-class contract with its own stated at-least-once semantics,
  separate from the run path, at which point it is a consumer of the store rather than a
  destination of a pipeline.
- The canonical layout stops being directly readable by outside tooling, which would make the
  redundancy argument against a file destination false.
