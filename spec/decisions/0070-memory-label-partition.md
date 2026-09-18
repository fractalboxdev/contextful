# 0070 — The two label views partition the join, and no row is absent from both

**Status:** accepted 2026-09-18
**Decides:** `memory.settle.invariant.view-partition`

## Context

`outcome_labels` is the scored view. It joins `predictions` to `outcomes` on prediction id,
derives lead time in seconds, and keeps an observation from the prediction instant through
the deadline plus an inclusive grace of 86400 s. `outcome_labels_unresolved` is where
everything else goes — the settlements that arrived outside the window, the observations
whose verdict is null, the rows an audit needs to see precisely because nothing scored them.

Between them the two views are supposed to cover the join. That is a property of the two
predicates, not of the intent behind them, and it fails in two ways that both look like
ordinary SQL.

The first is drift. Two predicates authored independently encode one idea twice, and the
two encodings disagree at the boundary — at the grace instant, at the equality case, at
whichever clause one author wrote as `>` and the other as `>=`. A row landing exactly there
is either scored twice or scored by neither, and nothing in either view says so.

The second is three-valued logic. Every clause of the scored predicate compares instants
cast to epoch seconds. A null `observed_at`, a null `deadline_at`, a prediction whose
instant is absent — any of them makes a comparison null, and a null predicate is not true in
the scored view and not true in its negation either. The row is then invisible to scoring
and to audit at once, which is strictly worse than being scored wrongly: a double-scored row
shows up as a count that does not add up, and an invisible row shows up as nothing.

A third failure comes from the store rather than from the predicate. A view naming a column
a particular store does not carry errors out for every reader, so one mis-declared table
takes down scoring for the whole deployment.

## Decision

`outcome_labels_unresolved` negates the scored predicate verbatim, and every clause of that
predicate is null-guarded ahead of any comparison, so the predicate evaluates true or false
and no row is absent from both views. A column a store may not carry is projected as the
null literal, so a row missing it moves to the audit view instead of erroring the view out
for every reader.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Verbatim negation of one predicate, every clause null-guarded, absent columns projected as null** *(chosen)* | Total coverage of the join by construction; a row is scored or audited and never neither; one mis-declared column moves one row rather than breaking every reader | The unresolved view is derived syntactically from the scored one, so a change to scoring rewrites both together, and defensive null projection hides a genuinely mis-declared table behind an audit row |
| Two independently authored predicates | Each view reads naturally on its own terms; neither is a mechanical transform of the other | Lost on drift: two encodings of one idea disagree at the boundary, and the disagreement surfaces as a row scored twice or not at all |
| Unguarded comparisons in both views | Simplest SQL; the predicate reads as the rule reads | Lost on coverage under three-valued logic: a null instant makes the clause null, and the row falls out of both views with no signal |
| Error the view on a missing column | A mis-declared table is loud and gets fixed immediately | Lost on blast radius: one absent column breaks every reader rather than moving one row to audit |

## Criteria

1. **Total coverage of the join** — whether every joined row appears in exactly one view.
2. **Resistance to drift** — whether two statements of one rule can diverge.
3. **Blast radius of a malformed input** — how many readers one bad table affects.
4. **Legibility of the view text** — whether a reader can read the rule off the SQL.

Criterion 1 decided it, and the deciding comparison is between two failures rather than
between a failure and a success: a row scored twice against a row invisible to scoring and
to audit at once. Invisibility wins the argument because it leaves no artifact to notice —
a double count is a discrepancy somebody chases, a missing row is a smaller number nobody
questions. That is what forces the null guards, and criterion 2 then forces the verbatim
negation rather than a second authored predicate. Criterion 4 loses: the guarded predicate
reads worse than the rule it encodes.

## Consequences

Every joined row has a home. A reconciliation counting both views against the join returns
equality, and that equality is a property of the view text rather than a claim about it.

The unresolved view is derived from the scored one. A change to scoring rewrites both
together, and an author who edits one without the other breaks the partition — the
derivation is a discipline the text carries, and it is the cost this decision accepts.

Defensive null projection hides a genuinely mis-declared table. A store missing a column it
should carry produces audit rows rather than an error, so the misconfiguration is found by
somebody reading the audit view rather than by the view failing. That trade is deliberate
and it is the second cost.

The guarded predicate is harder to read than the rule. Anyone reasoning about the scoring
window reads through the null guards to reach it.

## Revisit triggers

- A generated view pair, where the unresolved predicate is produced from the scored one
  rather than maintained beside it, which removes the discipline the derivation rests on.
- Audit-view rows accumulating from null projections rather than from genuine unresolved
  settlements, which means defensive projection is masking a configuration problem at scale.
- A store contract that guarantees the column set, which removes the reason for defensive
  projection entirely.
