# 0220 — Truncation is the one secondary admitted behind a hash or a tokenization

**Status:** accepted 2026-09-18
**Decides:** `enforcement.mask.refusal.combine-without-generalization`

## Context

A complete mask is a single value carrying a primary half and a secondary half, and both halves
are private. A consumer holding that value has no route to apply the primary alone — discarding
a mandated secondary fails to compile — and the same value drives the write-time layer and the
query-time layer, so a declared combine cannot reach one and miss the other. The combine is
therefore the mechanism by which a guardrail over an exhaustible domain is actually enforced
rather than merely recommended.

Six strategies exist: drop, keyed hash, format-preserving tokenization, truncation to a
leading character count, bucketing into a generalized band, and numeric range generalization.
Four of them are candidates for a secondary position, and they do not all compose with a
digest-producing primary. A keyed hash and a tokenization both emit hexadecimal text.
Bucketing and numeric range generalization both act on numbers.

That mismatch does not announce itself. Declared as a pairing, it is accepted by whatever layer
tolerates the type coercion and silently ignored by the other, or it lands on a string that
happens to parse and produces a band with no relationship to the original value's magnitude.
The operator reads the manifest and sees two strategies stacked, which presents as a stronger
mask; the digest underneath is untouched, and the exhaustible-domain guardrail the combine was
meant to satisfy is not satisfied.

## Decision

Bucketing or numeric range generalization layered behind a hash or a tokenization raises
`EnforceCombineWithoutGeneralization`. Truncation is the one secondary that generalizes a
digest: cutting to a leading hexadecimal run collapses many inputs onto one output, so an
exhaustive search over an enumerable domain returns a crowd rather than a person. An operator
wanting both a generalized band and a joinable key declares two columns rather than one mask.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Admit truncation alone behind a digest-producing primary; refuse the numeric secondaries** *(chosen)* | A declared combine always changes the primary's output, so the guardrail it satisfies is actually satisfied. | An operator wanting a band and a joinable key declares two columns instead of one mask. |
| Accept the pairing and apply the secondary where the type permits | Nothing refused; the operator's declaration is honored where it can be. | Loses on whether the pairing changes the primary's output: it succeeds at write time and fails at read time, giving the operator a mask that works in one layer. |
| Widen the secondaries to include a numeric fold over the digest's bytes | A numeric secondary composes with a digest, so the declared vocabulary stays uniform. | Loses on legibility: the result is neither joinable nor a band anyone can reason about, so it satisfies the guardrail's form and not its purpose. |
| Refuse every secondary behind a digest and require truncation as part of the primary | One strategy, no combine machinery for this case. | Loses on uniformity: the combine mechanism exists for the other strategies, and a second declaration shape for digests is a rule with no reason a reader can see. |

## Criteria

1. **Whether the pairing changes the primary's output** — the only thing a secondary is for.
   This is the criterion that decided it.
2. **Whether the two layers agree** — a combine that one layer applies and the other ignores
   is worse than no combine, since the operator believes it applies everywhere.
3. **Legibility of the result** — whether the masked value is something a consumer can join on
   or reason about as a band. The numeric fold fails this.
4. **Uniformity of the declaration vocabulary** — whether digests are declared like everything
   else. This is the criterion the chosen option trades against by refusing rather than
   adapting.

Whether the pairing changes the primary's output decides it. A mask's strength is a property of
its output, and a secondary that leaves the output identical contributes nothing while reading
as though it contributes something. That gap between appearance and effect is exactly what the
exhaustible-class guardrail is trying to close, so admitting a no-op secondary there would
defeat the rule it was declared to satisfy.

## Consequences

The set of legal combines is small enough to state exhaustively, and a reader of the manifest
can tell what a stacked declaration actually does to the stored value. Because one value drives
both layers, a combine that passes validation applies in both, so there is no configuration
where the write-time bytes and the query-time expression disagree about a masked column.

The accepted cost: an operator who wants both a generalized band and a joinable key declares
two columns rather than one mask. That widens the table, and the two columns must be kept in
step — the band and the key are derived from the same original value, and nothing in the
manifest states that relationship.

Reversing this is cheap: admitting a numeric secondary behind a digest would validate, and the
resulting masks would be no weaker than a bare digest. What it would cost is the guarantee that
a declared combine means something, which is the only reason the refusal exists.

## Revisit triggers

- A secondary strategy is added whose output type composes with hexadecimal text and whose
  result is joinable or bandable.
- A digest-producing primary is added that emits numbers rather than hexadecimal text, which
  would make the numeric secondaries compose.
- Operators are found routinely declaring paired columns for the band-and-key case, which would
  make a first-class two-output mask worth the declaration complexity.
