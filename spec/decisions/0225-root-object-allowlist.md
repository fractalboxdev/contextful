# 0225 — Root-level objects ship by enumeration while any table is withheld

**Status:** accepted 2026-09-18
**Decides:** `enforcement.bound-redistribution.refusal.root-object-outside-the-allowlist`

## Context

The redistribution bound runs at push. A per-table flag cleared holds back every object of that
table and keeps the table's name out of the manifest written to the bucket, and the exclusion
applies before the object diff is computed so no later push catches the withheld objects up.
That covers the table's own columnar files and the verbatim record each pull writes — the two
envelopes rows travel in.

It does not, by itself, cover the root of the store. A store root holds objects that are not
any one table's: the catalog database, the durable-record blob directory, the memory database,
the mirrored permission tables, the stale-memory index. Some of those carry row values from
every table in the store. A bound that withholds a table's files while shipping a catalog
database containing that table's rows has withheld nothing.

The root object set is not fixed. New root-level artifacts appear as the system grows — an
index, a cache, a summary — and each is added by someone solving a different problem, who has
no reason to read a rule about redistribution. The rule's real subject is therefore not the
objects that exist now but the object somebody adds later.

That makes the direction of the default the whole decision. A rule enumerating what stays
behind is silent about anything new: the new object ships, and if it carries the withheld
table's rows, the bound is broken with no signal. A rule enumerating what ships is silent in
the other direction: the new object stays behind, and somebody notices because replication of
that artifact does not work.

## Decision

While any table is withheld, a root-level object ships when it is proven free of row data. The
mirrored permission tables and the stale-memory index ship; the catalog database, the
durable-record blob directory and the memory database stay behind. A root-level object absent
from that allowlist raises `EnforceRootObjectNotAllowlisted` at push. The list enumerates what
ships rather than what stays, so an object added by someone who never read this rule stays
behind.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Enumerate what ships; refuse at push on anything else** *(chosen)* | A new root object cannot carry a withheld table's rows into the bucket, because it does not travel until someone has looked at it. | A new root-level artifact does not replicate until someone adds it to the allowlist, and the push refuses rather than shipping it. |
| Name the objects that stay behind | Replication keeps working for anything added later, with no list to maintain. | Loses on failure direction: a new object ships by default and carries the withheld table's rows, and nothing reports it. |
| Inspect each root object for row values at push | No list at all; the decision is made from the bytes each time. | Loses on cost and on coverage — every push pays for a scan, and a record blob's contents are not statically decidable, so the inspection cannot be sound. |
| Withhold every root object while any table is withheld | Maximal safety with no enumeration to get wrong. | Loses on the replica remaining usable: without the mirrored permission tables and the stale-memory index, the replica cannot resolve permissions or staleness at all. |

## Criteria

1. **Failure direction when someone adds an object later** — whether the unknown case ships or
   stays. This is the criterion that decided it.
2. **Whether the replica remains functional** — a replica that cannot resolve permissions is
   not a restricted replica, it is a broken one. Withholding everything fails this.
3. **Soundness of the judgment** — whether "free of row data" is decided reliably.
   Per-push inspection fails this on record blobs.
4. **Friction on ordinary development** — how often adding an artifact interrupts someone. This
   is the criterion the chosen option loses on.

Failure direction decides it. The other three options are each defensible on the day the rule is
written, when the root object set is known and correct. They differ entirely in what the rule
does six months later, against a contributor who never encountered it — and a redistribution
bound whose correctness depends on every future contributor having read it is not a bound. An
enumeration of what ships costs one review; an enumeration of what stays costs a leak nobody
observes.

## Consequences

The bound's claim is checkable by reading one list against the store root, and the two objects
that do ship are both provably free of row values rather than assumed to be. Because the
exclusion applies ahead of the object diff, a withheld root object is never read-then-skipped,
so the push side does no work on data it will not send.

The accepted cost is friction, paid by whoever adds a root-level artifact: the push refuses,
naming the object, and replication of that artifact waits for an allowlist entry and the review
that precedes it. In a deployment with no withheld tables the rule is inert, so the friction
lands only where the bound is actually in force — which also means it lands unpredictably, on a
contributor who may not know why.

Reversing this is cheap to write and removes the only property that made the bound durable: a
stay-behind list is correct exactly until the next artifact is added.

## Revisit triggers

- A root-level object is added whose freedom from row data is decidable statically, which would
  make per-push inspection sound for at least that class.
- The store root stops accumulating new object kinds, so the enumeration becomes stable and the
  friction disappears on its own.
- A replica is found to need a root object that cannot be proven free of row data, forcing the
  allowlist and the bound to be reconciled rather than one overriding the other.
