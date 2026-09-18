# 0267 — A contributor-share ceiling bounds concentration, and a group whose per-contributor masses are unavailable is withheld

**Status:** accepted 2026-09-18
**Decides:** `disclosure.suppress.refusal.dominance-evidence`

## Context

A distinct-contributor floor is the cheapest useful suppression rule and the easiest one to
mistake for a privacy boundary. It counts how many contributors stand behind a group and
withholds the group when too few do. What it cannot see is how the group's metric mass is
distributed among the contributors it counted. A group of six contributors clears a floor of
two comfortably, and if one of those six holds eighty percent of the mass, the published sum
nearly equals that contributor's own number and the published median approximates it. The
group is small-cell-clean and discloses a specific contributor's figure.

The floor's other blind spots are stated alongside it and are not what this decides: it says
nothing about composition across repeated releases, it presumes a fixed quasi-identifier set,
and it leaves a homogeneous cell's shared value readable at full confidence. Concentration is
the one of the four that a second suppression rule closes outright rather than bounds.

Closing it requires evidence the floor never needed. A contributor count is derivable from
the group alone; a share requires each contributor's own mass, which means the governed
statement has to emit one row per group and contributor before the rollup. That breakdown is
the only shape per-contributor mass reads out of, and therefore the only shape a ceiling is
enforceable over. When a model stops emitting it — the statement changes, the contributor key
drops out of the projection, an upstream table loses the column — the ceiling has no input.

At that moment the rule has two honest readings and one dishonest one. It can withhold the
group, treating unverifiable as failed. It can refuse the run. What it cannot do is publish,
because publishing under a declared share constraint with no evidence asserts compliance that
was never evaluated, and the resulting cell is indistinguishable from one that passed.

## Decision

A policy declares `max_contributor_share` as a fraction of a group's metric mass, and a group
where any single contributor's share of the sign-insensitive mass exceeds the ceiling is
suppressed even after clearing the size floor. The check reads the contributor breakdown the
governed statement emits, and suppression runs over that breakdown before the rollup. A group
carrying a share constraint whose per-contributor masses are unavailable at evaluation raises
`DisclosureDominanceUnverifiable` and is suppressed as unverifiable rather than published.
Withheld groups collapse into the same rule-free sentinel whichever rule fired.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A contributor-share ceiling over the breakdown, withholding when masses are unavailable** *(chosen)* | Closes the concentration hole with arithmetic already available in the breakdown, at no new dependency, and a missing input withholds rather than silently certifies. | A model that stops emitting the breakdown withholds every share-constrained group at once, so a projection change reads as a data outage. The composition hole the floor leaves is untouched. |
| Per-token noise budgets on numeric aggregates | A principled bound rather than a heuristic, covering concentration and repeated-release composition in one mechanism. | Lost on dependency for this scope: a differential-privacy library, cumulative per-token spend tracking, and a refuse-when-exhausted path, all before the first governed figure is publishable. |
| Neither mechanism — the size floor alone | Nothing to declare, nothing to compute, no new failure mode. | Lost on sufficiency: the six-contributor group with one holding eighty percent of the mass is a known, constructible disclosure that a floor of any value does not catch. |
| Publishing a share-constrained group when the masses are unavailable | Availability: a projection change degrades precision rather than emptying a report. | Lost on verifiability: compliance is then unverified rather than satisfied, and the published cell carries the same shape as one that was checked. |
| Refusing the whole run when masses are unavailable | Loudest possible signal; nobody reads a half-empty report as data. | Lost on blast radius: one ungoverned group takes down every other group in the model, including those whose evidence is present. |

## Criteria

1. **What each mechanism costs against what it closes.** **This criterion decided.** The share
   ceiling closes the concentration hole completely using values the breakdown already
   carries; noise budgets close more but require a library, a spend ledger and an exhaustion
   path that nothing else in the contract needs. At this scope the ratio is not close, and
   sufficiency alone would have chosen either mechanism over none.
2. **Sufficiency of the floor alone** — whether a known disclosure survives the existing rule.
   It does, constructibly.
3. **Verifiability** — whether a published cell means the check ran, or only that it did not
   fail loudly.
4. **Blast radius of a missing input** — how much of a report a single ungoverned group takes
   with it.
5. **Signal to the consumer** — whether a caller can tell which rule withheld a group. Every
   option here scores the same: the sentinel names no rule, by design elsewhere.

## Consequences

Concentration becomes a declared, checkable property of a published model rather than
something an analyst notices after the fact. The evidence requirement pushes the contributor
key into the governed statement, which is also what makes the audit record's per-reason
tallies meaningful — an operator tuning thresholds sees dominance and size counted apart.

The cost accepted: an unmeasured composition hole remains, undefended. An analyst composing
many floor-satisfying and ceiling-satisfying aggregates across shifting groupings can still
narrow one contribution, and no mechanism in this contract bounds how many such aggregates it
takes. Naming that is the whole of the response to it here. The second cost is operational: a
model that stops emitting the breakdown withholds every share-constrained group, which
presents to a consumer as data disappearing rather than as a schema problem. Reverting to
publish-on-missing-evidence is a one-line change and would silently invalidate every cell
published after it.

## Revisit triggers

- Composition attacks across repeated releases are demonstrated at a cost low enough that the
  stated gap is exploitable rather than theoretical.
- A differential-privacy dependency enters the tree for another reason, removing the cost that
  decided against noise budgets here.
- Unverifiable suppression fires often enough in practice that operators read an empty report
  as normal, which would mean the signal has stopped working.
