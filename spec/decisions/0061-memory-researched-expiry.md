# 0061 — Only a researched claim expires, and citation or promotion is what retains it

**Status:** accepted 2026-09-18
**Decides:** `memory.revise.workflow.expiry`, `memory.revise.invariant.retention`

## Context

Claims arrive at three standings. A `curated` claim comes from a human or a
version-controlled file a human wrote. A `derived` claim comes from synthesis over rows
from a source the deployment declared, which means an operator chose the source and keeps
choosing it. A `researched` claim comes from synthesis over rows the system fetched on its
own initiative — nobody declared the source, nobody reviewed it, and the fetch answered a
question that has since been answered.

Those three populations age differently. A curated claim is as good as the human who wrote
it and goes stale only when that human says so. A derived claim is regenerated whenever the
declared source produces new rows, so its freshness is a property of the pipeline behind
it. A researched claim has no pipeline behind it: the fetch was a one-off, and the claim
accumulates in the table forever unless something removes it.

Removal has two questions. The first is what signal says a claim is still wanted. The
obvious answer — a claim nobody has recalled is a claim nobody needs — is unavailable: the
store keeps an outbound request ledger recording fetches it made, and keeps no ledger of
reads served. There is no place to ask whether a claim was ever returned to a caller.
Citation is the signal that exists, because an artifact's provenance names the claims it
was built from, and promotion is the other, because a human raising a claim's tier is an
explicit statement of standing.

The second question is what removal does to the row. Claim tables are append-only, carry a
validity pair, and are read at a vantage. A delete would make a point-in-time read of a
past day return a different answer than it returned on that day, and would leave an erasure
receipt unable to account for rows that vanished outside an erasure.

## Decision

A `researched` claim carries an expiry. A scheduled pass stamps a validity end on an
expired claim that no promotion covered, and the row itself stays in place — it falls out
of live serving through the liveness predicate every consumer already applies, and a
bounded read at an earlier vantage still returns it. An expired `researched` claim survives
when an artifact's provenance cites it or a human promotes it. Retention reads citation
rather than reads. A `curated` or `derived` claim carries no expiry at all.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Expiry on `researched` alone, stamped as a validity end, retained by citation or promotion** *(chosen)* | Bounded growth of the one population nothing maintains; append-only, time travel and erasure receipts all keep their meaning; retention reads a signal the store actually records | Curated and derived claims grow without bound; a claim many readers used but nothing cited expires |
| Expire a claim nothing has recalled | The signal a reader would name first — usage, not citation | Lost on availability: no read-side usage ledger exists, so the predicate has nothing to read |
| Delete the expired row | Smallest table; no liveness predicate work at read time | Lost on the append-only property: a bounded read at a past vantage changes answer, and an erasure receipt cannot account for rows that left outside an erasure |
| Expire every tier | One rule across the table; no tier branch in the scheduled pass | Lost on standing: a human-curated claim has no expiry semantics — nothing regenerates it, so an expiry is a silent deletion of authored content |

## Criteria

1. **Availability of the signal** — whether the predicate the rule needs can be evaluated
   against state the store records today.
2. **Preservation of the append-only property** — whether a bounded read at a past vantage
   returns what it returned then, and whether an erasure receipt accounts for every row
   that left.
3. **Standing** — whether the rule treats a human-authored claim and a self-directed fetch
   as the same kind of thing.
4. **Bounded growth** — how much of the table the rule keeps from accumulating.

Criterion 1 decided it. Growth pressure is real but slow, and every option bounds it
somehow; what separates the options is that the rule a reader reaches for first — expire
what nothing reads — cannot be written at all, because the read side keeps no ledger.
Criterion 2 then chose the stamp over the delete among the rules that can be written.

## Consequences

Growth of the researched population is bounded without any authorial action, and the
scheduled pass is a single-predicate sweep with no tier-by-tier configuration.

The table still grows, only from the other two directions: nothing bounds curated or
derived claims, and the sweep does not touch them. A deployment whose declared sources are
broad accumulates derived rows indefinitely, and that is accepted.

A researched claim that many callers read but no artifact cited expires the same as one
nobody ever saw. This is the cost the missing read ledger imposes, and it is visible: a
reader who wanted the claim finds it absent from recall and present at a past vantage.

Reversing the rule is cheap on the sweep — the pass stops stamping — but the stamped
validity ends on already-expired rows are ordinary supersession stamps, so restoring them
to live serving means writing new claims rather than un-writing old ones.

## Revisit triggers

- A read-side usage ledger lands, at which point the retention predicate can ask whether a
  claim was ever recalled rather than whether something cited it.
- The derived population outgrows the researched one in a deployment, which makes expiry
  aimed at `researched` alone the wrong target.
- Expired-then-cited claims appear in audit at a rate that suggests citation is lagging the
  sweep rather than leading it.
