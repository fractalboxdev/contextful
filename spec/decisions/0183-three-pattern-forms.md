# 0183 — A table pattern takes three forms, and one implementation of them serves every consumer

**Status:** accepted 2026-09-18
**Decides:** `authority.grant.refusal.malformed-pattern`

## Context

A grant names the tables it covers by pattern rather than by enumeration. The same
pattern language is read by four consumers that have nothing else in common: derivation
legality, where a child's patterns are compared against a parent's; view registration,
where a pattern becomes the relation the engine builds for a caller; tool visibility,
where a pattern decides what appears in a projected listing; and replica object
selection, where a pattern decides which objects a replica pulls.

Derivation is the constraint that shapes the language. A holder derives a child offline,
with no round trip, and the check that the child is no broader than its parent runs both
in the deriving process and again at admission. That check is pattern containment: is
every table this child's patterns match also matched by the parent's. For containment to
be a cheap, total, obviously-correct comparison, the pattern language has to be small
enough that containment is decided structurally rather than by search.

The other pressure runs the other way. An operator who legitimately holds the whole
store should be able to say so once, rather than enumerating tables and silently losing
authority over each table created after the mint.

## Decision

A granted table pattern takes exactly three forms: a bare star, covering every table; a
prefix followed by a star, covering the prefix itself and everything beneath it; and any
other string, matched exactly. A star anywhere but the final position raises
`GrantPatternMalformed`. One implementation of the three forms answers derivation
legality, view registration, tool visibility and replica object selection, so the four
consumers cannot disagree about what a grant covers.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Three forms — bare star, trailing star, exact literal** *(chosen)* | Containment decides in constant work by comparing prefixes. Whole-store authority is one token. One matcher serves four consumers. | No way to express an exclusion. "Everything under a prefix except one subtree" is written as a set of concrete patterns. |
| Full glob, with stars, character classes and internal wildcards | Natural exclusions and flexible selection. | Loses on containment decidability: deciding whether one glob's language is contained in another's is neither cheap nor obviously total, and derivation needs that answer offline, in the holder's process, on every child. |
| Regular expressions | Maximal expressive reach. | Loses on the same criterion, and again on the evaluator bound, which admits no regular-expression predicate — the matcher would have to live outside the profile's evaluation rules. |
| Exact names only, no patterns | Containment is set inclusion. Nothing to parse. | Loses on expressive reach in the one case that matters operationally: an operator holding the store loses each newly created table, silently, with no signal that authority has narrowed. |
| Prefix patterns only, no bare star | Uniform structure, one rule. | Loses on expressive reach: whole-store authority would need a sentinel empty prefix, which reads as an accident rather than as a deliberate grant in an audit record. |

## Criteria

1. **Containment decidability** — whether "is this child's pattern set no broader than
   its parent's" is answerable structurally, in constant work, with no search. *This
   criterion decides.* Derivation happens offline, in an untrusted holder's process, at
   per-query grain, and is re-run at every admission; a containment test that is
   expensive or partial makes the whole offline-derivation model unaffordable, and a
   containment test that is merely *probably* correct makes narrowing unverifiable.
2. **One implementation across consumers** — whether derivation, view registration, tool
   visibility and replica selection can share a matcher, so a grant means the same thing
   at each.
3. **Expressive reach** — the selections an operator can state directly.
4. **Evaluator compatibility** — whether matching fits the bounded evaluator's rules.

## Consequences

Whole-store authority is an ordinary grant, written as one token, bounded by the
credential's actions, audience and expiry and visible in the audit record like anything
else — rather than a special case that has to be recognized and handled.

Table naming becomes load-carrying in a way it was not before: a hierarchy expressed in
names is the only hierarchy patterns can see. Deployments that want to grant along a
dimension other than the name prefix have to encode that dimension into names.

The accepted cost is exclusion. There is no pattern meaning "this subtree minus that
one", so a grant that excludes something enumerates what it includes, and a deep tree
makes that enumeration long and prone to omission as it grows.

Reversing toward a richer language is expensive because derivation legality is where the
language is read — widening it means finding a containment algorithm for the wider
language that a holder can run offline, not merely writing a matcher.

## Revisit triggers

- Deployments routinely hit the exclusion gap, visible as grants whose pattern lists run
  to many concrete entries and are amended whenever a table is added.
- A containment algorithm for a richer language is available with the same constant-work
  property, which would remove the criterion that decided this.
- A second selection dimension appears — a label or a tag on a table — making pattern
  matching over names insufficient regardless of how expressive the name language is.
