# 0221 — The validator refuses only a truncation width at or past the primary's own output width

**Status:** accepted 2026-09-18
**Decides:** `enforcement.mask.refusal.truncation-that-cuts-nothing`

## Context

Truncation behind a digest-producing primary is the secondary that collapses an enumerable
domain into a crowd. Its strength is entirely a function of one number: how many leading
characters survive. Cut to a short run and many original values map onto one masked value; cut
to a long run and the mapping stays near-unique, which leaves an exhaustive search returning a
single person rather than a set.

The declared widths bound the obvious end of that range — a truncation behind a keyed hash
carries a width below 32 chars, and one behind a tokenization a width below 20 — but a width
inside those bounds can still be far too wide to collapse anything meaningful.

Whether a given width collapses enough depends on the size of the column's real value space,
and the manifest does not state that. It declares a column class, a strategy and a width. It
does not say how many distinct phone numbers this deployment holds, how many of them share a
country prefix, or how the values are distributed. A validator enforcing a minimum collapse
would be enforcing an assumed domain size, and the assumption would be invisible to the
operator whose declaration it refused.

There is one case that needs no assumption. A width at or past the primary's own output width
cuts nothing: the truncation is a no-op, the mask is a bare digest wearing a combine, and the
exhaustible-class guardrail it was declared to satisfy is not satisfied. That is decidable from
the declaration alone.

## Decision

A declared width at or past the primary's own output width raises
`EnforceTruncationCutsNothing`. How narrow a width collapses enough against the column's real
value space stays the operator's judgment, since the manifest declares no domain size. The
validator refuses the widths that are unambiguously no-ops and judges nothing beyond that.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse only a width at or past the primary's output width** *(chosen)* | Every refusal is decidable from the declaration alone, with no assumed domain size behind it. The one unambiguous failure is caught. | A too-wide truncation passes validation and leaves a near-unique mapping, and nothing in the tree catches it. |
| Refuse any width above a fixed threshold | Catches the merely-too-wide case, not just the no-op. | Loses on what the manifest declares: the threshold is right for one domain size and wrong for every other, and the operator cannot see which assumption refused them. |
| Require the operator to declare a domain size and derive the width | The validator gets the missing input and can judge collapse properly. | Loses on honesty: the declared size is a guess, carried in the manifest as a constraint and treated downstream as a fact. |
| Refuse nothing and treat width entirely as operator judgment | No validator rule to argue about; the operator owns the whole number. | Loses on the one decidable case: a width at or past the digest's own width is unambiguously a no-op, and admitting it turns a mandated combine into decoration. |

## Criteria

1. **Whether the refusal is decidable from what the manifest declares** — or whether it rests
   on a size nobody stated. This is the criterion that decided it.
2. **Whether an unambiguous no-op is caught** — the case where the combine does literally
   nothing. Refusing nothing fails this.
3. **Honesty of the inputs a rule consumes** — whether the validator is fed a guess and treats
   it as a constraint. The declared-domain-size option fails this.
4. **Coverage of the real failure** — how many too-wide truncations the rule catches. This is
   the criterion the chosen option loses on, and it loses badly.

Decidability from the declaration decides it. A validator that refuses on an assumption it
cannot show the operator produces refusals that cannot be reasoned about: the operator reads
the error, does not know what domain size it assumed, and either widens a bound or picks a
number that passes. A rule whose refusals are all self-evident keeps the validator's authority
matched to its information.

## Consequences

Every truncation-width refusal names something visible in the declaration itself, so the fix is
never a guess. The validator claims no knowledge of data distribution, which keeps the manifest
honest about what it does and does not describe.

The accepted cost is large and worth stating plainly: a width that cuts something but not
enough passes validation, leaves a near-unique mapping, and nothing in the tree catches it. The
guardrail over exhaustible classes therefore guarantees that a combine is present and changes
the output, not that the resulting crowd is big enough to hide anyone. That judgment is the
operator's, and the size of the resulting exposure is unmeasured — it depends on a distribution
the system never sees.

Reversing this in the direction of a fixed threshold is cheap to implement and expensive to
live with: every manifest the validator admits would be re-judged against a domain size nobody
declared.

## Revisit triggers

- The store gains a reliable distinct-value estimate per column that the validator can read, so
  collapse becomes decidable from data rather than from an assumption.
- A deployment is found where a validating width left a mapping unique enough to re-identify
  rows, which would establish the real size of the accepted cost.
- The manifest gains an operator-declared domain size for another purpose, making the guess an
  input the system already carries rather than one this rule would introduce.
