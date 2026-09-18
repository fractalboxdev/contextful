# 0280 — The purge resolves its tenant column from the model contract and refuses a non-string type

**Status:** accepted 2026-09-18
**Decides:** `accountability.erase.refusal.non-string-tenant-column`, `accountability.erase.refusal.undeclared-tenant-column`

## Context

No partition segment separates tenants on disk. A tenant's rows are interleaved with every
other tenant's inside the same columnar files, so a purge is a rewrite: every file the read
path can reach is rewritten retaining the rows outside the named tenant.

That rewrite is defined by a predicate, and the predicate has a twin. A tenant-scoped grant
compiles a row filter that admits exactly the rows belonging to one tenant; the purge keeps
`NOT COALESCE(CAST(<tenant_col> AS VARCHAR) = '<tenant>', FALSE)`, byte for byte the
complement of that filter. The two are written to be complements because their correctness is
joint: any row the filter admits and the purge also retains is a row the tenant can still
read after their own erasure, and any row the filter excludes and the purge removes is
somebody else's data destroyed.

Complementarity is fragile at exactly one point — the comparison's type. Comparing a column
against a string literal requires a type decision, and a cast can run on either side. An
integer tenant column compared as a string and compared as an integer agree on well-formed
values and disagree on the ragged ones: a value with leading zeros, a value with padding, a
value at the edge of numeric range. Two code paths that each pick a cast direction on their
own will eventually pick differently, and the disagreement shows up as a silent read of
purged data rather than as an error.

The column's location is the same problem one level up. A store may carry several tables with
several candidate columns, and a purge that takes the column name from its own command-line
flag can name a column the grant does not use.

## Decision

The purge resolves where a tenant lives from the model's outermost partition key; no model
declaring one raises `PurgeTenantUndeclared`. A tenant column of a non-string type raises
`PurgeTenantColumnType` in place of an implicit cast, since a guessed cast direction is where
the rewrite and the read filter part company. The rewrite's predicate is the textual
complement of the filter a tenant-scoped grant compiles, and the coalesce retains a
null-tenant row that a bare inequality would have dropped.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Resolve from the model's outermost partition key; refuse a non-string type** *(chosen)* | One declaration feeds both the grant's filter and the purge's predicate, so the two cannot name different columns or compare at different types. | A deployment whose tenant column is an integer cannot run the purge until the model declares a string key, and a store with no declared partition key has no tenant path at all. |
| Cast on a guess and proceed | Every deployment gets a purge path with no authoring work. | Implicit-cast direction is precisely where the two predicates part. The divergence surfaces as rows readable after their own erasure, on the ragged values, silently. |
| Take the column from a flag on the purge command | The operator states it explicitly at the moment of use, with no schema change. | The flag and the grant then name columns independently, and nothing compares them. A purge that ran against the wrong column reports full success. |
| Default to a conventional column name | Zero configuration, and the convention is usually right. | Same divergence as the flag, with the additional property that nobody typed the wrong name, so there is nothing to review. |
| Normalize both sides to text at compile time in the grant as well | Keeps the purge working on integer columns by making the filter agree with it. | Changes the read path's hot filter for the benefit of a rare maintenance operation, and text comparison over an integer column defeats any index the filter would otherwise use. |
| Compare structurally rather than textually, per column type | No cast anywhere; each type compares natively. | Two independent implementations of comparison, one in the grant compiler and one in the rewrite, is the drift this decision exists to prevent — now with more surface. |

## Criteria

1. **Predicate agreement** — whether the rewrite's predicate and the grant's read filter can
   ever disagree about a row. The guess, the flag, the convention and the structural
   comparison all fail this.
2. **Single source for the column** — whether both sides read one declaration rather than two
   that happen to match.
3. **Failure visibility** — whether a mismatch surfaces as a refusal or as a wrong result.
   Only a refusal is detectable here, since a purge produces no output a reader could check.
4. **Read-path cost** — whether the decision changes the filter the read path runs on every
   query. Normalizing the grant side fails this.
5. **Deployment reach** — whether every existing schema can run a purge. This is the
   criterion the chosen option loses on.

Byte-for-byte agreement between the two predicates decides it over convenience of an implicit
cast. The other criteria describe how a disagreement would be found; this one describes
whether one can exist. A purge is unverifiable after the fact — its output is the absence of
rows — so correctness has to be structural rather than tested, and that means one declaration
and one comparison type.

## Consequences

Tenant scope is stated once, in the model contract, and both the operation that admits a
tenant's rows and the operation that removes them read it from there. A schema change that
moves the tenant column moves both behaviors together.

The accepted cost is a class of deployments with no tenant erasure path: integer tenant keys
and stores with no declared outermost partition key are refused at the point of the purge,
which is the point at which a deadline is already running. That refusal is deliberately not
recoverable by a flag, so the remedy is a model change and a rewrite of the affected tables'
key column — real work, scheduled under pressure.

Reversing the string requirement later is straightforward. Reversing the resolution source is
not, since deployments would by then have declared partition keys that a flag-driven purge
would be free to contradict.

## Revisit triggers

- The grant compiler and the rewrite come to share one predicate builder, making agreement a
  property of the code rather than of two texts that match.
- A deployment with an integer tenant key is blocked on a live erasure deadline, which prices
  the refusal against the migration.
- The store gains tenant-partitioned layout on disk, at which point a purge is a prefix
  operation and the predicate question largely disappears.
