# 0262 — A digest over an exhaustible identifier class carries a generalizing truncation, and an unrecognized class is refused

**Status:** accepted 2026-09-18
**Decides:** `disclosure.classify.refusal.class-vocabulary`, `disclosure.classify.refusal.digest`, `disclosure.classify.refusal.combine`

## Context

Classification attaches to a named column: a `pii_type` naming what the column holds and a
masking `strategy` naming what happens to it. Every downstream consequence keys on that
tag — placement floors, write-time removal, query-time transformation, and the protections
a release owes a key it joins on.

Some classes name a value space that is generable offline end to end. A social security
number, a telephone number, an email address, a medical record number: a digest over any of
them is invertible by enumerating the domain and hashing each candidate. For those columns
a `hash` strategy standing alone is a mask in name only, and the column reads back at full
confidence to anyone willing to spend the computation.

What actually generalizes a digest is truncating it, so many source values collide onto one
output. That makes the requirement a pairing rather than a single strategy — a primary and
a secondary that have to arrive together.

Two pairings look like they generalize and do not. A truncation declared at or past the
primary's own output width removes nothing: a digest is 32 chars wide and a token is 20, and
a truncation at those widths is the identity. A numeric generalization layered over a
hexadecimal digest returns the digest unchanged at write time and fails its cast at read
time, so the column is unmasked in storage and broken in use.

An unrecognized class value is the quiet failure. Treated as unclassified, a typo in a
`pii_type` downgrades a protected column to an ordinary one, and the manifest that made the
mistake passes review.

## Decision

A `pii_type` outside the registered class set raises `DisclosureUnknownSensitivityClass` at
manifest check. A `hash` strategy standing alone over an exhaustible class raises
`DisclosureDigestUnqualified`; the secondary that satisfies it is `combine = "truncate:<n>"`.
A truncation at or past the primary's own output width, and a numeric generalization layered
over a hexadecimal digest, each raise `DisclosureCombineDoesNotGeneralize`. A column's
complete mask carries the primary and the secondary as one value whose halves do not
separate, and dropping a mandated secondary is a compile error.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **The pairing compiles into one value; a bad class and a non-generalizing combine both refuse at manifest check** *(chosen)* | The declared protection is the applied protection, checked once, with no call site able to apply the primary alone. | The operator still chooses the width against their own domain, so the engine accepts a width that is technically a generalization while remaining practically reversible. |
| A runtime assertion at each enforcement point | Catches the same cases without changing how a mask is represented. | Loses on where the guarantee lives: the assertion is repeated at every call site, and a new enforcement point skips it silently. |
| Refuse `hash` entirely over exhaustible classes | Nothing to pair; the failure mode disappears. | Loses on usefulness: a truncated digest is a legitimate join key, and removing it removes the reason those columns are hashed at all. |
| Check that the truncation width is small enough | Closes the practically-reversible gap the chosen option leaves open. | Loses on decidability: sufficiency depends on the column's value space, which the engine does not hold. Only widths that provably remove nothing are refusable. |
| Treat an unrecognized class value as unclassified | Tolerant of vocabulary drift and of packs written against a newer class set. | Loses on direction of failure: a typo would downgrade protection rather than fail the manifest, and the downgrade is invisible in review. |

## Criteria

1. **Whether the declared protection is the applied protection.** *This criterion decides.*
   A guardrail a call site bypasses by applying the primary alone buys nothing, so the
   pairing is represented as one inseparable value rather than as two fields a consumer
   might read independently.
2. **Whether the check is decidable from what the manifest holds.** Output widths are
   known; value spaces are not, which fixes where the engine stops.
3. **Direction of failure on malformed input.** A wrong tag fails the manifest rather than
   weakening the column.
4. **Whether a legitimate join key stays expressible.** Rules out refusing `hash` outright.

## Consequences

A classified column's mask is one value with two halves that do not separate, so a consumer
holding it has no path back to the primary and a mandated secondary cannot be dropped
without a compile error. The vocabulary becomes a hard boundary: adding a class is a
registry edit, and a pack written against a class the deployment does not know fails at
manifest check rather than landing degraded.

The accepted cost is the width. The engine refuses only the widths that provably remove
nothing and accepts the rest, so a truncation of, say, twenty-four characters over a digest
passes the check while remaining reversible for any domain small enough to enumerate. That
judgment rests with the operator against their own column, and nothing in the check helps
them make it.

Reversing the unrecognized-class refusal is the cheaper-looking edit and the more damaging
one: every typo in circulation becomes an unclassified column, silently.

## Revisit triggers

- A class set entry gains a declared value-space size, which would make width sufficiency
  decidable and move the width check inside the engine.
- A masking primary appears whose output width varies, which breaks the equality test the
  combine refusal rests on.
- Packs are observed failing manifest check on classes that are legitimate but unregistered,
  which would mean the vocabulary is lagging its sources rather than bounding them.
