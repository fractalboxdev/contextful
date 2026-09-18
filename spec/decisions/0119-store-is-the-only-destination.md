# 0119 — The local context store is the destination a pipeline resolves

**Status:** accepted 2026-09-18
**Decides:** `connector.resolve-destination.refusal.unknown-destination`, `connector.resolve-destination.refusal.irreconcilable-schema`

## Context

The connector contract is symmetric in shape: a source world and a destination world share
a types interface, and a destination exports `prepare`, `write`, `commit` and `abort`. That
symmetry invites a general destination catalogue — write to Parquet, write to a database
file, post to a webhook — of the kind every ingest framework grows.

What the symmetry hides is that the run model is not symmetric. A run is journaled, replayed
from its record, and committed per table with visibility following commit. Those properties
are properties of a store the engine owns. A destination that delivers somewhere else brings
its own delivery semantics, and the run model has no way to honor them: a crash between two
writes is recoverable against a store the engine can read back, and unrecoverable against a
party that only accepts pushes.

Two candidate destinations turn out to be the store again under another name. The canonical
layout is already Parquet, and a sync push copies that tree, so a Parquet destination writes
a second copy of bytes the store already holds in that format. A database-file destination
is the same observation with a different container.

A webhook destination is the one that is genuinely different, and it is different in the
direction the run model cannot follow. Delivery over a network is at-least-once messaging
with its own retry and dedupe rules. A receiver outage becomes a failed run. A run retry
becomes a double send, which the receiver deduplicates or does not. And the credential for
that egress moves inside the engine, where the connector contract's whole apparatus for
credential custody is built around inbound reads, not outbound publishes.

## Decision

The local context store is the destination a pipeline resolves. Every other destination name
raises `ConnectorUnknownDestination` when the pipeline is assembled, ahead of any row
moving. A schema change the destination cannot reconcile raises
`ConnectorSchemaIrreconcilable`. The destination world is declared with no host arm, so a
guest cannot supply a destination the runtime does not resolve. A synthesized artifact is
written back through this same destination and becomes queryable corpus; there is no
separate sink.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **The local store, and nothing else** *(chosen)* | One durability and visibility model, which the run model already honors; one place a row can be. | A foreign layout another team's tooling owns has no supported path; something outside the engine reads the landed rows and writes it. |
| A Parquet file destination | Direct output in a format other tools read. | Rejected as redundant: the canonical layout is already Parquet and a sync push copies that tree, so it duplicates an existing path. |
| A database-file destination | Output another process can query without the engine. | Rejected as redundant on the same ground: it is a second container for bytes the store already holds. |
| A webhook destination | Push semantics; downstream systems react as rows land. | Rejected on semantics: delivery is at-least-once messaging with its own retry and dedupe rules, an outage becomes a failed run, a retry becomes a double send, and an egress credential moves inside the engine. |
| A pluggable destination interface with a host arm | Any destination an operator writes. | Rejected on the same semantics, generalized: the engine would carry a delivery contract it cannot state, and a guest could name a destination the runtime never judged. |

## Criteria

1. **Whether a destination adds a delivery semantic the run model cannot honor.** *This
   criterion decided it.* Journaling, replay and commit-then-visible are the properties
   every other contract in the system reads the run path for. A destination that cannot be
   read back breaks all three at once, and no amount of careful retry policy inside a
   connector restores them.
2. **Whether the destination duplicates an existing path.** Whether the same bytes are
   already reachable another way.
3. **Where an egress credential lives.** Whether adding the destination moves outbound
   publishing authority inside the engine.
4. **Nameability of the destination set.** Whether a guest can name a destination the
   runtime did not resolve.

## Consequences

Rows land in one place, and every downstream contract — the read path, visibility,
disclosure — reads one store with one commit semantic. A synthesized artifact takes the same
path as ingested data and becomes queryable corpus with no second mechanism. The refusal
fires when the pipeline is assembled, ahead of any row moving, so a misconfigured
destination costs nothing.

The accepted cost: a foreign layout another team's tooling owns has no supported path out of
the engine. An organization whose warehouse expects a particular directory shape, or whose
downstream service expects to be notified, writes something outside the engine that reads
the landed rows and does that work. That is a real integration burden and it falls on
exactly the deployments that already have infrastructure — which is also the argument that
they have somewhere to put it.

## Revisit triggers

- Every deployment writes substantially the same reader-and-publisher outside the engine,
  which would mean the burden is uniform enough to be worth carrying inside it.
- The run model gains a delivery semantic — an outbox with its own record, deduplicated and
  replayable — under which a push destination could be honored rather than approximated.
- The sync path's copy of the canonical tree stops covering the cases a file destination
  was wanted for, which would remove the redundancy argument against it.
