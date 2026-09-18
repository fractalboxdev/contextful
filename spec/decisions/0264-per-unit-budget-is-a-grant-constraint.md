# 0264 — A per-unit privacy budget is a catalog-tracked grant constraint that a release debits and stops drawing on

**Status:** accepted 2026-09-18
**Decides:** `disclosure.release.refusal.budget-exhaustion`

## Context

The release pipeline bounds each unit's contribution, suppresses small groups, and adds
noise calibrated to the bounded sensitivity. Every one of those steps is scoped to a single
release. None of them says anything about what happens when the same units are released over
repeatedly — a nightly refresh, a dashboard rebuild, a template run twice with slightly
different parameters.

That composition is where a bounded release stops being bounded. Noise that hides a unit in
one result averages out across many, and differencing across shifting parameters isolates a
contributor whom every individual release protected. A per-unit lifetime cap is the only
thing in the pipeline that speaks to the sequence rather than the instance.

So the budget has to be durable and shared. It survives process restarts, because a refresh
job that forgets what it spent yesterday has no cap at all. It is visible to a second
releasing principal, because two jobs drawing on the same units concurrently double-spend a
cap neither can see the other consuming.

The catalog already holds constraints of exactly this shape. The group-size floor and the
contributor-share ceiling are grant constraints read by every release, and the budget is the
same kind of thing with a running total attached — declared as a per-run spend and a
per-unit lifetime cap, recorded on the release provenance so a unit audits its own spend.

What remains is the behavior at exhaustion, and the wrong answer there is silence. A release
that quietly drops an exhausted unit returns a figure the caller reads as complete, and the
shortfall is invisible in both the result and the envelope.

## Decision

A per-unit privacy budget is a catalog-tracked grant constraint of the same shape as the
group-size floor: the release declares a per-run spend and a per-unit lifetime cap. The
runtime debits each contributing unit at the end of a release and stops drawing on an
exhausted one, raising `DisclosureUnitBudgetExhausted` where the release names that unit
explicitly. Each release records which units contributed and what each was charged, journaled
and hash-linked, so a unit audits its own inclusion and its own spend.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A catalog-tracked grant constraint, debited per release, refusing where an exhausted unit is named** *(chosen)* | The cap survives a restart and is seen by every releasing principal, and an exhausted unit is either absent by construction or named in a refusal. | A unit's releases go dark once its lifetime cap is spent, and raising a cap is an explicit grant edit rather than an operational retry. |
| An in-memory spend counter per release job | No catalog write on the release path; the cap costs nothing to maintain. | Loses on durability and on sharing: a restart resets it, and two concurrent releases over the same units double-spend a cap neither observes. |
| Silently exclude an exhausted unit from every release | Releases never fail; downstream jobs keep running. | Loses on truthfulness of the result: a release naming that unit returns a figure the caller reads as complete, with the shortfall absent from the output and the envelope. |
| No budget at all, relying on per-release bounding | Simplest pipeline; every guarantee is local to one run. | Loses on composition: repeated releases over the same units are exactly what a single-release floor is silent on, and they are the normal operating pattern. |
| Budget per releasing principal rather than per unit | Bounds how much any one consumer extracts, which is the attacker-shaped question. | Loses on what is being protected: the exposure belongs to the unit, and two principals releasing over the same unit spend its privacy jointly regardless of their own caps. |

## Criteria

1. **Whether the budget survives a restart and a second releasing principal.** *This
   criterion decides.* A cap exists to bound a sequence of releases, so any representation
   that a restart clears or a concurrent job cannot see is not a cap — it is a per-run
   number wearing the name of one.
2. **Whether an exhausted unit's absence is visible to the caller.** Silence turns a partial
   result into a complete-looking one.
3. **Whether the protected entity is the one being budgeted.** The unit, never the row and
   never the consumer.
4. **Cost on the release path.** A catalog read and a debit per run.

## Consequences

A unit's releases go dark once its lifetime cap is spent, and that is the intended behavior
rather than a failure. Where a release names the unit explicitly it refuses; where the unit
is one contributor among many it simply stops contributing, and the group it was in may fall
under the size floor as a result. Both outcomes are recorded in the release provenance, so a
unit reads its own history.

The accepted cost is operational rigidity at exactly the wrong moment. A recurring refresh
that exhausts its units stops producing, and the way forward is an explicit grant edit
raising the cap — a reviewed change, not a retry. A deployment that sets caps loosely to
avoid this has a budget in name only, and one that sets them tightly discovers the limit
when a dashboard goes blank.

Budget accounting is a write on the release path, so a release is no longer a pure read and
derive; it debits, and that debit has to be durable before the output is published.

## Revisit triggers

- A composition accounting method lands that charges less for correlated releases than the
  running total does, which would make a flat per-run spend the wrong debit.
- Deployments are observed setting caps high enough never to bind, which would mean the
  constraint is being satisfied by configuration rather than respected.
- A releasing pattern appears where units join and leave the contributing set continuously,
  so a lifetime cap per unit no longer corresponds to a bounded exposure.
