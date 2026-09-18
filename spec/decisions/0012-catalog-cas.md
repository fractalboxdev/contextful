# 0012 — Coordination is linearizable compare-and-swap behind the catalog port, for two single-writer operations

**Status:** accepted 2026-09-18
**Decides:** `topology.coordinate.refusal.catalog-backend`

## Context

Multi-node deployments need exactly two things coordinated, and the inventory is short
enough to state: a shard lease keyed by source partition, so one holder advances a given
partition at a time; and a cursor compare-and-swap for an opaque-token or snapshot-id
position. Nothing else in the tree reaches for a coordination primitive.

Both are cheap in the shape they need. A cursor advance is one conditional update
predicated on the stored version, read back through its affected-row count. A lease is one
row carrying an owner, an expiry instant and a fencing token, taken by the same conditional
update. Neither holds an interactive transaction open, and both run at low request rates —
a cadence tick's worth, not a request path's worth. Any backend that supplies a single
linearizable conditional statement supplies the whole primitive.

The deployment shapes constrain what can be required. Single-node and edge deployments run
air-gapped, reaching nothing outside themselves for coordination, so anything that must be
running beside the engine is disqualified for them. The cluster shape, meanwhile, is
already pointed at a shared database, and adding capacity there is copying the binary and
starting it.

The remaining risk is a backend that accepts the conditional statement and does not order
it linearizably — a read replica, an eventually-consistent store, a backend whose
conditional update is best-effort. That backend produces two lease holders and lost cursor
advances under exactly the conditions coordination exists for, and it does so without
error.

## Decision

Coordination needs one property: linearizable compare-and-swap behind the `Catalog` port,
and no component reaches for a stronger one. Single-node self-hosting uses a local catalog
file owned by one process; a self-hosted cluster points at Postgres; a managed edge
deployment uses the platform's per-object database primitive and a managed cloud deployment
a managed Postgres. The code above the port names no backend, so swapping one is a wiring
change at the binary's injection point. A backend whose conditional update is not
linearizable raises `CatalogCasUnsupported` at open, naming the backend — coordination
narrows to a weaker mode by refusal rather than by degradation. The tree ships no consensus
implementation and bundles no external coordination service, and availability in the
cluster shape is the shared database's availability.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Linearizable compare-and-swap behind the catalog port** *(chosen)* | Every deployment shape supplies the primitive with something it already runs: a file, a database, or a platform primitive. Air-gapped single-node and edge stay air-gapped. | Clustered availability is the shared database's availability, so an operator wanting multi-node without one has no supported path. |
| An embedded consensus implementation | Multi-node availability with no external database; the engine owns its own coordination. | Lost on operational cost for the request rate involved: a consensus cluster is a system to size, monitor, back up and recover, bought for two low-rate conditional statements. |
| A bundled external coordination service | A well-understood primitive with strong guarantees and mature tooling. | Lost on air-gappability: single-node and edge deployments would have to run a second process reachable over a network, which contradicts the deployment shape those profiles exist for. |
| An off-the-shelf consensus-over-SQLite catalog | Multi-node without a separate database, behind the same port. | Lost on operational familiarity: an operator already runs a file or a Postgres, and this backend is a third system to learn for a case no current deployment presents. It slots behind this port unchanged whenever one does, which makes it the option a reader should expect to revisit first. |

## Criteria

1. **Request rate the workload needs** — how much coordination throughput and what
   transactional shape. **This criterion decided.** A cursor advance is one conditional
   statement read back by affected rows and a lease is one row, which is orders below what
   a consensus cluster exists to provide; buying consensus for that load spends operational
   budget on capacity nobody uses.
2. **External dependencies per deployment shape** — whether a shape must run a process
   beside the engine. Air-gapped shapes must not.
3. **Operational familiarity** — whether an operator already runs the backend. A file and a
   Postgres are; a consensus cluster is not.
4. **Availability ceiling in the cluster shape** — the chosen option is the weakest here and
   lost this criterion deliberately.

## Consequences

The port is narrow enough that a new backend is judged by one question, asked at open, with
the backend named when the answer is no. No deployment shape gains a second distributed
system to operate, and the cluster shape's capacity story is copying a binary. Single-node
and edge deployments run with their sources reachable and nothing else.

The cost accepted: clustered availability is the shared database's availability, with no
replication or failover of the engine's own over it. An operator who wants multi-node
operation while refusing a shared database has no supported configuration — that case is
served by the deferred consensus-over-SQLite option or by nothing.

Reversing toward consensus is cheap by design, since the primitive is behind one port and
the code above it names no backend. The refusal, not the port, is what would need
rethinking.

## Revisit triggers

- A deployment needs multi-node operation and cannot run a shared database, which is the
  case the deferred option exists for.
- The two single-writer operations grow into a set large enough that one conditional
  statement no longer describes the workload.
- A backend a deployment depends on is found to be non-linearizable in practice despite
  documenting otherwise, making the open-time refusal unenforceable as stated.
