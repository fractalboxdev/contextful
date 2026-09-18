# 0039 — A table declaring sensitive columns replicates off by default and is read through the proxying face

**Status:** accepted 2026-09-18
**Decides:** `sync.replicate.refusal.sensitive-table-direct`

## Context

A table declares which of its columns are sensitive. Everywhere else in the system that
declaration drives runtime behavior: a column mask is applied inside the relation a caller's
connection registers, a row predicate narrows what that relation sees, and the zone gate
decides whether the relation is offered at all. Every one of those mechanisms runs on the
machine that holds the bytes, at the moment of the read.

A replica moves the bytes. Once a Parquet file carrying a sensitive column lands on a
consumer machine, every enforcement mechanism above it is running on that machine, under
that machine's configuration, against a file any process with filesystem access can open
directly. The engine is embedded and an external process opened against the same files reads
the same bytes with the engine uninstalled — which is a property the read face states
plainly, and which means the copy is the disclosure.

The copy is also irreversible. Erasure markers propagate to a replicated table on the next
refresh, but a refresh only reaches machines that are still refreshing. A consumer that
pulled once and disconnected holds the values indefinitely, and nobody enumerated that
machine.

The default direction therefore decides more than the configuration does. Whichever way it
points, the wrong setting is a mistake somebody makes; the question is what that mistake
costs.

## Decision

A table declaring sensitive columns carries replicate-off by default, so an ordinary refresh
moves no copy of it onto a consumer machine. A refresh requesting a replicate-off table
raises `ReplicaSensitiveTable`. A consumer needing those columns reads them through the
proxying face, which verifies the presented credential, returns projected rows rather than
file handles, and records each read.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Replicate-off by default; read through the proxying face** *(chosen)* | A mistake in configuration withholds data rather than distributing it. The sensitive columns exist on exactly the machines an operator named. | A consumer needing those columns takes a network round trip per read, and an offline machine cannot answer over them at all. |
| Replicate and filter at the consumer | The consumer answers offline, at full local speed. | Lost on failure direction of a configuration mistake: the bytes reached the machine before the filter ran, so the filter constrains a query and not the disclosure. |
| Replicate with the columns masked at the consumer | Masking is uniform with the canonical store's behavior. | Lost on failure direction of a configuration mistake: the mask is applied by the copy that already holds the unmasked values, which is the same defect wearing the vocabulary of enforcement. |
| A per-consumer allowlist maintained centrally | One place states who gets what, auditable in one read. | Lost on failure direction of a configuration mistake: an entry omitted by mistake is fail-open — the table replicates and nobody learns it did. |

## Criteria

1. **Failure direction of a configuration mistake** — whether the wrong setting withholds or
   discloses. Both consumer-side options and the central allowlist fail this.
2. **Reversibility** — whether the effect of the mistake can be undone. A copy on a machine
   that stopped refreshing cannot be recalled, so this criterion binds hardest here.
3. **Offline availability for the consumer** — whether the consumer answers with no network.
   This is the criterion the chosen option loses on.
4. **Auditability** — whether each read of a sensitive column leaves a record. The proxying
   face satisfies this; every replicating option abandons it, since a local file read leaves
   no trace anywhere.

Failure direction decides it, sharpened by reversibility. An over-restrictive default costs a
configuration change somebody notices immediately, since the consumer's read refuses. An
over-permissive default costs a copy on an unenumerated machine that nobody notices at all.

## Consequences

A read of a sensitive column is mediated and recorded wherever it happens, which makes the
question "who has seen this column" answerable from one place rather than from an inventory
of machines. Withdrawing access is a credential change, and it takes effect on the next read.

The accepted cost is latency and availability. Every read of a sensitive column crosses the
network, so a consumer workload mixing sensitive and non-sensitive columns has two very
different per-read costs in one query surface, and an air-gapped consumer is simply out of
scope for those columns.

Reversing this is cheap in one direction and expensive in the other: turning replication on
for a table is one setting, and turning it back off does not recall the copies already distributed.

## Revisit triggers

- The proxying face's per-read latency is measured as the dominant cost in a consumer
  workload that is otherwise entirely local.
- Sensitive columns acquire an at-rest encryption scheme whose key is held by the canonical
  store and released per read, which would make a local copy inert without the key.
- Operators are observed routinely flipping tables to replicate-on, which would mean the
  default is fighting the workload rather than protecting it.
