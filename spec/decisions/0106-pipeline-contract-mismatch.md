# 0106 — A materialization failing its declared contract never renames in

**Status:** accepted 2026-09-18
**Decides:** `pipeline.publish.refusal.contract-identity`

## Context

A published table is addressed by its contract identity — `contract_version` beside
`schema_fingerprint`, the fingerprint taken over the declared column set with each
column's type and the table's grain. A consumer binds to that identity and builds
derivations on top of it: a dashboard, a downstream model, an agent's retrieval over a
known column. The identity is the whole interface; the files beneath it are not part of
the contract.

A build materializes from upstream data the producer does not control. The upstream drops
a column, changes a type, or changes its grain — one row per account per day becomes one
row per account — and the materialization that results no longer matches what the
identity promises.

Consumers read the identity, not the record of what happened during the build. There is
no place in a consumer's read path where a note about drift appears: the identity is the
same string it was yesterday, the table is there, the query runs, and the numbers are
wrong or the column is missing. Every consumer's derivation breaks at the same instant,
and each of them diagnoses it independently against a producer who reported success.

A build materializes into a staging location and renames in on success, so there is a
point at which the result exists and is not yet serving. That is where the contract is
enforceable without the prior state ever being disturbed.

## Decision

A materialization whose columns, types or grain fail the declared contract raises
`PipelineContractMismatch`, naming the column and the expectation it missed. The build
does not rename in: the last published state keeps serving, and the failed
materialization never becomes readable.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the rename, leave the prior state serving** *(chosen)* | The identity means exactly one thing for its whole lifetime; a drifting upstream breaks the producer's build rather than every consumer at once | A producer adding a column ships a contract version bump in the same change, and a blocked build leaves the table's freshness advancing no further |
| Publish and record the drift | Freshness keeps advancing; the fact is in the build log | Lost on what the identity guarantees: a consumer reads the identity rather than the record, and no consumer read path consults a build log |
| Widen the contract automatically to admit the materialization | No build ever blocks; the declared contract stays true by construction | Lost on stability of the identity: a contract that adapts to whatever was produced means nothing across builds, and a consumer holding it can assume nothing |
| Publish the conforming columns only | Partial data keeps flowing | Lost on what the identity guarantees, in the same way: the identity names a column set, and a table published under it missing a column is a table that lies about its own shape |

## Criteria

1. **What a consumer holding the contract identity may assume about the columns behind
   it.** *(decided it)*
2. **Blast radius of a failure** — whether it lands on one producer or on every consumer.
3. **Freshness continuity** — how long the table can stop advancing.
4. **Producer effort per legitimate schema change.**

The first criterion decided it because the identity is the only thing a consumer holds.
Every rejected option keeps the build moving by weakening what that string promises, and
the moment it promises less than a fixed column set, types and grain, a consumer cannot
build anything durable on it and the contract artifacts stop earning their existence.
Blast radius reinforces it: a refused build costs one team a fix, while a published
drifted table costs every consumer a debugging session against a producer reporting
success.

## Consequences

Consumers can treat the identity as a real interface: pin it, derive against it, and read
`contract-history.jsonl` to resolve what an older identity moved to. The refusal is what
makes that history complete, since no build publishes under an identity it did not
satisfy.

A build that fails the contract also leaves a build-log entry carrying the refused status,
so the producer's diagnosis names the column and the expectation rather than a diff.

The cost accepted is on the producer's side, twice. A legitimate schema change — adding a
column, widening a type — is not a one-sided edit: the producer ships a contract version
bump in the same change, and consumers resolve the move through the history. That is
friction on exactly the changes producers make most often.

The heavier cost is freshness. A table blocked on its contract stops advancing entirely,
and stays stopped until a human intervenes. For an upstream that drifted at the start of a
weekend, the consumers reading that table read data that is correct, coherent, and days
old — and a staleness derived against `max_lag` is the only signal that says so.

## Revisit triggers

- A drift class appears that is provably additive and consumer-safe, which is the case an
  automatic widening would serve without weakening the identity for existing columns.
- Contract-blocked builds become a common cause of staleness rather than a rare one,
  meaning the freshness cost is being paid routinely rather than exceptionally.
- Consumers are observed reading the build log or the freshness record to check for drift
  before querying, which means the identity is no longer carrying the guarantee on its own.
