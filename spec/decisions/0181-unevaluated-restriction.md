# 0181 — A restriction no read evaluator implements is refused everywhere it can appear

**Status:** accepted 2026-09-18
**Decides:** `authority.profile.refusal.unevaluated-restriction`

## Context

A grant carries more than a list of tables. It carries row restrictions and aggregate
bounds — a minimum group size, a maximum single-contributor share of the metric mass,
the permitted aggregate functions, a ceiling on groups per query. Each of those is a
narrowing the credential asserts about itself, and each is real only if something on the
read path evaluates it when rows are selected.

The credential format and the read path advance separately. The profile names the
restriction tuples the engine admits; the evaluator that applies a given restriction to
rows is engine code. Nothing about the shape of a credential prevents a mint from
producing a restriction tuple the profile names but no evaluator implements. The
question is what happens to that credential.

The failure direction is what makes this sharp. A restriction is a narrowing, so an
engine that ignores one it cannot evaluate admits a credential as broader than its
holder believes it to be. The holder reads the grant and sees a bound; the rows come
back unbounded. Nothing in the audit record distinguishes the two cases, because the
credential says the same thing either way.

## Decision

A grant carrying a row restriction or an aggregate bound for which no read evaluator
exists raises `ProfileRestrictionUnevaluated`. The refusal fires at four points: at the
mint, at derivation against the parent, at derivation against the proposed child, and at
admission. No verifiable credential in the system carries an unevaluated restriction, so
no read path ever has to decide what to do with one.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse at mint, on both derivation sides, and at admission** *(chosen)* | An unevaluated restriction never reaches a read. The credential a holder inspects and the authority a checkpoint yields say the same thing. | A restriction cannot be minted until its evaluator ships. The format cannot lead the engine. |
| Admit and ignore the element | Nothing blocks a credential format from moving ahead of the engine. | Loses on authority inflation: the ignored element is a narrowing, so ignoring it widens the credential silently and leaves no record that it did. |
| Admit and deny every row the restriction would touch | Fails closed, so no inflation. | Loses on diagnosability: the holder sees an empty result set rather than a refusal, and a mint that produced a credential nothing can use looks identical to one that works. |
| Refuse at the read alone, when a query first touches the restriction | Cheapest to implement — one check, at the point the evaluator is looked up. | Loses on time to discovery: the refusal arrives after the mint succeeded, after derivation succeeded, and after a holder has already built on the belief that the bound holds. |
| Refuse at the mint alone | Blocks the credential at its source. | Loses on completeness: a derivation that copies its parent's restriction, and a credential minted before an evaluator was withdrawn, both carry an unevaluated element past a mint-time check. |

## Criteria

1. **Authority inflation** — whether an element the engine does not implement can ever
   result in more reach than the credential names. *This criterion decides.* Every other
   property here is recoverable by a later fix; a credential that quietly read more than
   it claimed has already returned the rows, and nothing downstream can unread them.
2. **Time to discovery** — how far from the mint the condition surfaces. Early is worth
   paying for, but a late refusal that is still a refusal beats a silent widening.
3. **Diagnosability** — whether the party that hits the condition can tell what went
   wrong from what it is handed.
4. **Rollout independence** — whether the credential format and the engine can ship on
   separate schedules. This one is given up outright.

## Consequences

Minting is now coupled to engine capability: adding a restriction kind means shipping
its evaluator first, then admitting it to the profile. A deployment cannot prepare
credentials for an engine version it does not run.

Derivation gets slightly more expensive, since both the parent and the proposed child
are examined rather than only the appended block. That catches the case where a parent
minted under an older engine is narrowed under a newer one whose evaluator set has
changed underneath it.

The accepted cost is the lockstep: the restriction vocabulary can only grow at the pace
of the read path, and an operator who wants a new bound has no way to express it early
and have it start working when the engine catches up.

What is now expensive to reverse is the guarantee, not the check. Callers can build on
"every element of a verified credential is enforced" — code that assumes it will not be
obviously wrong if the guarantee is later relaxed, it will be quietly wrong.

## Revisit triggers

- A restriction kind becomes data rather than code — an evaluator registered at project
  load rather than compiled in — so "no evaluator exists" stops being a property of the
  build and becomes a property of a deployment's configuration.
- Evaluator coverage becomes per-table rather than per-element, making the refusal
  depend on which tables a grant names and turning one test into many.
- A deployment needs to mint credentials that outlive an engine upgrade cycle, making
  the rollout-independence criterion carry real weight for the first time.
