# D42 — Erasure is a forced rewrite, a bounded cascade and a measured receipt

**Status:** accepted

## Context

A tombstone constrains a query, not the bytes: an object-store credential reads a tombstoned value until its file is rewritten. Derived facts synthesize over derived facts, so one hop leaves conclusions standing.

## Decision

`disclosure.erase` removes every reachable copy under one audited verb and claims exactly what it measured.

- A subject erasure against a table declaring no subject column raises `ErasureSubjectUndeclared`. The tenant path rewrites columnar files under the complement of the tenant grant filter, parameterized, over a string-typed partition key.
- The cascade walks recorded provenance as a transitive closure to 16 hops; a deeper chain raises `ErasureCascadeUnbounded` and the erasure commits nothing. A fail-closed gate refuses fact reads with `ErasureRestagingRequired` until attested re-synthesis.
- Every read filters tombstones, and every file holding an erased row is rewritten or collected within 24 h of the erasure's commit.
- A purge presented with a capability token raises `PurgeRequiresOwner`, checked before anything reveals which tenants exist.
- `disclosure.receipt` attests rewrite-and-exclude over the canonical store, names its exclusions in a `coverage` block, and widens only with `receipt_version`. A re-run rescans, signs afresh and links `prior_request_id`.
- The ledger and chain hold HMAC-SHA256 pseudonyms under the project audit key; the receipt's `tenant_hash` is SHA-256 over a per-receipt salt and the identifier.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Forced rewrite, bounded closure, measured receipt *(chosen)* | — | Cost scales with the table; fact reads go down until re-synthesis; a no-op re-run costs a full scan. |
| A one-hop cascade | Completeness | A fact derived from a derived fact would outlive its evidence. |
| An unbounded transitive walk | Termination | Termination would rest on provenance the store never proves acyclic. |
| Crypto-shredding a per-subject key | Fit with the substrate | Every write path would need a per-subject key held forever. |
| A broad "data destroyed" receipt | Assertability | The object interface has no delete; the claim would be false when signed. |

## Consequences

- A holder asserts that the canonical store returns no row of the tenant, and nothing wider.
- Superseded files persist until collection, inside the 24 h bound; replicas and the replication bucket sit outside the claim.

## Revisit

- The substrate gains a delete, allowing a receipt version claiming physical destruction.
- Request-to-last-replica erasure within 72 h becomes an engine bound rather than an operator objective.
