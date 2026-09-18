# 0231 — A surface declaring write-time removal and a public-cloud allow-set together is refused

**Status:** accepted 2026-09-18
**Decides:** `enforcement.place.refusal.removal-with-public-cloud`

## Context

Two of the three enforcement layers act on a surface from opposite ends. Write-time removal
runs inside the writer ahead of columnar encoding: what it drops leaves no cell anywhere
downstream, the run path's durable record included, and a stolen object-store credential
yields bytes with the value already gone. An inference-zone allow-set acts at the read: it
names where the model consuming a result may run, and a public-cloud entry says the values
that survive are fit for a vendor model.

An operator declaring removal on a column has stated that the column's values are too
sensitive to hold at all. An operator admitting a public cloud on the same surface has
stated that what that surface yields may be processed outside the organization. The two
declarations are about the same surface and point in opposite directions, and neither one
is a refinement of the other: removal does not narrow the allow-set, and the allow-set does
not constrain the writer.

Both readings of the pair are defensible in isolation and incompatible together. Read one
way, the operator meant the residue after removal is safe for a vendor model — but removal
was declared because the column's content was the problem, and the residue of a keyed
digest or a generalized band is a row whose content was that value and nothing else. Read
the other way, the operator meant the surface is generally cloud-eligible and forgot which
column carried the rule. Nothing in the manifest distinguishes them.

Unlike a runtime condition that a check can resolve one way or the other, this is a
property of the declaration itself, discoverable when the manifest loads and unchanged by
any subsequent request.

## Decision

A surface declaring write-time removal on a column and an allow-set admitting a public
cloud raises `EnforceRemovalWithPublicCloud`. The refusal is per surface, at the grain the
allow-set is declared on, and it reads the pair rather than either declaration alone. An
operator who wants the surviving columns available to a vendor model separates them from
the removed column onto a surface of their own.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the surface carrying both declarations** *(chosen)* | The contradiction is resolved by the party who wrote it, at the moment the manifest loads, rather than by a rule that guesses which declaration was meant. | A table whose removed column sits beside columns genuinely fit for a vendor model splits into two tables or narrows as a whole, which is schema work the operator did not ask for. |
| Accept both and apply each at its own layer | No configuration is refused; each layer does exactly what it says. | Loses on coherence: the result satisfies neither reading of the operator's intent, since the digest or band standing in for the removed value is precisely what reaches the vendor model. |
| Accept both and resolve the allow-set down to the fail-closed pair | Fail-closed, no refusal, no schema work. | Loses on legibility for the same reason a silent narrowing loses elsewhere — the operator's public-cloud entry is present in the manifest and inert at the read, and nothing says so. |
| Warn at validation and proceed | The operator is told, and nothing breaks. | Loses on kind: this is a declaration error rather than a runtime condition, so it is discoverable exactly once, cheaply, by the process reading the file, and a warning defers it to nobody. |
| Refuse only where removal is a drop, permitting it where removal is a transform | Narrower rule; a keyed digest is arguably not the original value. | Loses on the same coherence criterion in a weaker form — a transform's substitute is derived from the value the operator called too sensitive to hold, and admitting it to a vendor model re-opens the question the rule was declared to close. |

## Criteria

1. **Whether the pair states one coherent intent** — whether a single behavior can be
   derived that satisfies both declarations. *This criterion decides.* Every option that
   proceeds must pick one reading over the other, and the manifest contains no evidence for
   either; a refusal is the only outcome that does not attribute an intent to the operator
   that the operator did not write.
2. **Where the error is cheapest to discover** — a property of a static declaration belongs
   to the process that loads it.
3. **Legibility of the resulting configuration** — whether a declaration present in the
   manifest is in force at the read.
4. **Schema cost imposed on the operator** — how much restructuring the refusal forces.

## Consequences

A manifest that survives loading carries no surface whose two declarations disagree, so
placement resolution over any loaded surface answers a question with one reading.

The cost accepted is structural. A table holding a removed column beside columns genuinely
fit for a vendor model splits into two tables, or the operator narrows the whole table and
loses cloud processing for columns that never needed protecting. Splitting a table is not
free elsewhere in the system — it changes what a single statement can read without a join,
and the visibility binding, the removal rule set and the allow-set all follow the split.

The refusal also fires on the transform case, where the operator's reasoning may have been
that a keyed digest is safe to send. That argument is not available through this path; a
column intended to travel as a digest is declared as a digest column rather than as a
removal rule with a wide allow-set.

Reversing toward acceptance is expensive because the surviving behavior would have to pick
a reading, and any deployment configured under one reading breaks under the other.

## Revisit triggers

- A transform class appears whose output is provably independent of the removed value, at
  which point the pair stops being contradictory for that class.
- The cost of splitting a table is measured on real schemas and lands high enough that
  operators declare wide allow-sets on unprotected sibling tables instead, which would move
  the problem rather than solve it.
- Allow-sets become declarable per column at the same grain removal rules are, so the pair
  can be expressed without either declaration reaching the other's columns.
