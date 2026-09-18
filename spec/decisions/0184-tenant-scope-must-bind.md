# 0184 — A tenant scope with nothing to bind onto refuses the read rather than widening to the table

**Status:** accepted 2026-09-18
**Decides:** `authority.grant.refusal.unbindable-tenant`

## Context

A grant scopes below the table onto a table-and-tenant pair. The tenant is an opaque
byte string, and it binds onto that table's bare outermost partition column. The
comparison is a bound-parameter equality against the partition key written on disk, with
no collation in the loop: the grant's value, the key on disk and the consumer's own
identifier are the same bytes, or they are different tenants.

The binding is a property of the store, not of the credential. A table declares its
partition columns when it is built, and a credential is minted against a name. Nothing
ties the two together at the mint, and the table's declaration can change after the
credential exists. So a credential carrying a tenant scope can arrive at a read against
a table that declares no bare outermost partition column at all.

The direction of the failure is what makes this a decision rather than an oversight. The
scope exists to narrow. If the engine cannot bind it and proceeds anyway, the grant that
said "this table, this tenant" delivers the whole table — the widest possible reading of
a clause whose only purpose was to be narrow. And it does so with a credential that
inspects as correctly scoped.

## Decision

A tenant-scoped grant against a table that declares no bare outermost partition column
raises `GrantTenantUnbindable` at admission. A scope with nothing to bind onto does not
widen to the whole table, and does not fall back onto a filter applied by the query.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse at admission when the scope cannot bind** *(chosen)* | The failure is loud, named, and impossible to mistake for a working grant. No unbindable scope ever produces rows. | The refusal is a runtime condition. A credential that worked against one build of the table refuses after a rebuild that drops the column, and the holder learns it at the read. |
| Ignore an unbindable scope and serve the table | Availability: reads keep working through a table rebuild. | Loses on fail direction. The one grant dimension whose entire job is narrowing, silently widening — a privilege escalation with a credential that reads as scoped and an audit record that shows a normal read. |
| Bind onto any column whose values look like tenants | Keeps scoped reads working where the partition layout changed. | Loses on the identity guarantee: the tenant comparison is defined as byte equality against the partition key, and matching on an inferred column makes the scope mean something the grant never said. |
| Apply the scope as a predicate the query carries | Simple; no relation to build. | Loses on enforcement: a grant reaches rows through the relation the engine builds for the caller, so a predicate in the query text is a filter the caller's own request supplies and can omit. |
| Refuse at the mint instead | The error lands on the operator who wrote the grant. | Loses on drift: the table's declaration outlives no credential in particular and can change after any mint, so a mint-time check proves nothing about the read that follows it. |

## Criteria

1. **Fail direction** — when the engine cannot honor an element, whether the result is
   narrower or broader than what the credential says. *This criterion decides.* The
   options that keep reads flowing all do so by serving more rows than the grant names,
   and there is no artifact anywhere that distinguishes that from an ordinary read.
2. **Where the guarantee lives** — whether the scope is enforced by the relation the
   engine builds or by something the request supplies.
3. **Truth of the condition at the moment it matters** — whether a check made at one
   time still holds at the read.
4. **Availability through a store change** — whether scoped reads survive a rebuild.
   This is the criterion given up.

## Consequences

Store shape and credential shape are now coupled at read time, and the coupling is
visible: dropping a table's outermost partition column takes every tenant-scoped grant
over it out of service immediately, by name, rather than quietly converting them into
whole-table grants.

An operator gains a reliable statement: a tenant-scoped credential that returns rows
returned only that tenant's rows. There is no third case where the scope was present but
inert.

The accepted cost is availability during store change. A table rebuild that alters the
partition declaration is a breaking change for every scoped credential over it, with no
grace period and no degraded mode, and the first party to notice is a reader rather than
the operator who made the change.

What is expensive to reverse is the guarantee callers build on. Code and policy written
against "scoped means scoped" would be quietly wrong, not loudly wrong, if an inert
scope were ever admitted later.

## Revisit triggers

- A legitimate per-tenant need arises against a table that declares no partition key at
  all, with no way to express the scope — this is the open question the grant clauses
  already carry, and answering it may add a binding target rather than a refusal.
- Partition declarations become versioned alongside the table, so a credential can name
  the declaration it was minted against and the condition becomes decidable before the
  read.
- Store rebuilds that change partition columns become frequent enough that the
  availability criterion outweighs the fail-direction one for a specific class of table.
