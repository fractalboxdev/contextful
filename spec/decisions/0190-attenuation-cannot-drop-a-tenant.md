# 0190 — A derivation adds a tenant scope or repeats it unchanged, and dropping or re-aiming it is refused

**Status:** accepted 2026-09-18
**Decides:** `authority.attenuate.refusal.tenant-drop`

## Context

Every other grant dimension narrows by subtraction. Fewer actions is narrower. A smaller
set of table patterns is narrower. A shorter expiry is narrower. Absence on the child
means "no further constraint here", and for actions, tables and templates that reads
correctly as narrowing.

Tenant scope inverts that. The scope is a constraint added below the table, binding the
grant onto one value of a table's outermost partition column. Removing it does not
subtract reach — it restores the child to the whole table, which is the broadest thing
the parent could have held. A rule that treated absence uniformly as narrowing would
make "drop the tenant scope" a legal derivation, and the derivation that most obviously
widens would be the one that passes.

Changing the value is a second, different failure. A child naming a tenant its parent
never named is not broader by any count of dimensions — it is the same shape, one
constraint, one value — but it is aimed at rows the parent never had authority over.
Comparison by size does not see it at all.

Both matter because derivation is offline. Nothing consults the issuer, so the legality
of a tenant transition is decided entirely by a rule the deriving holder and the
checkpoint both apply to the two values in front of them.

## Decision

Adding a tenant scope to an unscoped parent narrows and is admitted. Repeating the
parent's scope unchanged is admitted. Dropping the scope, or naming a different tenant,
raises `AttenuationTenantDropped`. Tenant is the one dimension where absence on the child
is refused rather than inherited.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Add or repeat; refuse a drop and refuse a change** *(chosen)* | The per-query scoped child works from a broad parent, and no derivation reaches data the parent never held. | A service holding a scoped parent cannot mint a child for a sibling tenant without returning to the issuer, even where that would be legitimate. |
| Treat any tenant edit as narrowing, uniformly with other dimensions | One rule for every dimension; nothing special to encode. | Loses on direction of reach, twice: a drop widens the child back to the whole table, and a change aims it at another tenant's rows. Both pass a size comparison. |
| Refuse adding a scope to an unscoped parent, admitting only exact repetition | Absolutely conservative; the child's tenant always equals the parent's. | Loses the per-query child pattern entirely — a service holding one broad parent could never derive a scoped child, which is the pattern the whole tenant design rests on. |
| Admit a drop, and rely on the read path to re-derive scope from the caller | Simple derivation rule. | Loses on where the guarantee lives: scope would then be supplied by the request rather than carried by the credential, and a request that omits it reads the table. |
| Allow a change within a set of tenants the parent enumerates | Sibling-tenant derivation without an issuer round trip. | Loses on the grant's shape: a scope is one value bound to one partition key, not a set, so this needs a different grant dimension before it needs a different derivation rule. |

## Criteria

1. **Direction of reach** — whether the child can touch rows the parent could not.
   *This criterion decides.* Every dimension's narrowing rule exists to make that
   question answerable by comparing two values offline, and for tenant the answer does
   not follow from the sizes of the two sides — so the rule encodes the direction
   explicitly rather than inferring it.
2. **Whether the per-query scoped-child pattern remains expressible** — the pattern
   depends on adding a scope to a broader parent locally.
3. **Where the scope is enforced** — carried by the credential rather than supplied by
   the request.
4. **Reach of legitimate derivations** — sibling-tenant children. Given up.

## Consequences

A service that holds an unscoped or table-wide parent derives per-query scoped children
freely, which is the intended path. A service that has already been narrowed to one
tenant is terminally narrowed to it — correct, and occasionally inconvenient.

The accepted cost is that second case: minting for a sibling tenant requires the issuer,
so a multi-tenant consumer either holds a broad parent and scopes per query, or holds
one parent per tenant. There is no middle position, and choosing wrongly at deployment
time is only discovered when a second tenant appears.

Tenant becomes the one dimension whose narrowing rule cannot be derived from a general
comparison, so any future dimension with the same inverted polarity needs its own rule
rather than falling out of the framework.

Reversing to admit drops is expensive in the worst way: it would silently widen every
scoped credential in circulation, with no mint and no record.

## Revisit triggers

- A grant dimension appears that enumerates several tenants, which would make a change
  within the enumerated set a decidable narrowing rather than a re-aiming.
- A table gains multiple bindable partition columns, so "the tenant scope" stops being a
  single value and the add-or-repeat rule stops covering the cases.
- Issuer round trips for sibling-tenant minting are observed to be a real operational
  cost rather than a rare one.
