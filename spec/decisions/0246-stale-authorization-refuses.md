# 0246 — A read whose mirrored authorization is older than its table's budget refuses

**Status:** accepted 2026-09-18
**Decides:** `visibility.bound-staleness.refusal.malformed-budget`, `visibility.bound-staleness.refusal.over-budget-read`, `visibility.bound-staleness.refusal.never-swept-source`, `visibility.bound-staleness.refusal.budget-below-cadence`

## Context

Mirrored authorization is a copy, and a copy ages. The source withdraws a share at one
instant; the sweep observes the withdrawal at a later one; between those two instants the
mirror answers reads with an allow the source no longer honors. The gap is not an
implementation defect to be engineered away — it is the structural cost of holding
permission state as data rather than asking the source per request — so the question is
not whether aged authorization is served but how old it is allowed to be before a read
stops being defensible.

The age is computable because an observation carries its own clock: every access row
holds `_acl_observed_at`, the instant the permission state was read at the source, held
separately from the instant the row landed in the store. A content pull at one hour
reusing a sweep from an earlier hour serves aged authorization over fresh content, and
the separate clock is what makes that age a number rather than an impression.

How old is acceptable is not one number for a deployment. A conversation corpus where
people are added and removed from private channels hourly, and a code corpus whose
repository access changes when someone changes team, differ by an order of magnitude in
what an aged allow costs. The tolerable figure belongs to the corpus.

A budget is also a claim about something the deployment does not fully control. The
watermark moves when a sweep run finishes having read every governed resource, so the
smallest achievable lag is the source's sustainable full-run cadence. A source with a
per-item permission model under request limits moves its watermark in hours. A budget of
fifteen minutes declared against it is not a tight posture; it is a table that refuses
every read, discovered at the first request rather than at the moment someone wrote the
number.

Finally, a budget's text has to parse into one figure. `90s`, `15m`, `1h` and `2d` are
unambiguous; a compound form, a bare number or an unrecognized suffix each admit more
than one reading, and a default applied to an ambiguous declaration silently picks one.

## Decision

`max_acl_staleness` is a positive integer followed by one suffix from `s`, `m`, `h`, `d`,
declared per table, with the operator's manifest value taking precedence over a pack
default. A compound form, a bare number, or a suffix outside the four raises
`VisibilityBudgetMalformed`, naming the table and the text it read. A read whose source
lag exceeds a touched table's budget raises `VisibilityAccessStale` (HTTP 503, in band on
the tool surface) carrying the source, the lag in seconds, the budget in seconds and the
last observation instant. A source with no completed full run raises the same identifier
whatever budget its table declares, carrying a lag the mirror cannot bound. A budget
tighter than the sweep cadence its source sustains raises `VisibilityBudgetUnreachable` at
diagnose, naming both figures. A statement spanning several bound tables is checked
against each of their budgets, and the tightest governing figure decides.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A per-table budget, a typed refusal past it, unswept treated as maximally aged, and the budget checked against the cadence at diagnose** *(chosen)* | Aged authorization produces a loud, typed, in-band failure carrying every figure needed to diagnose it, and a budget that cannot be met fails before it is deployed rather than at a reader's first request. | Availability drops when a sweep falls behind: a limping sweep takes the whole source offline for reads. Every servable table needs a budget an operator can defend against a measured cadence. |
| Serve anyway with a best-effort freshness figure in the envelope | The system stays up through every sweep failure, and a careful reader can see the age. | Loses on detectability: a revoked grant keeps answering, and the only thing marking the answer wrong is a field nobody downstream is obliged to read. The freshness figure is advisory in a place where an advisory is worth nothing. |
| Return zero rows past the budget | No refusal to handle, no error path in consuming surfaces, and nothing over-wide is ever served. | Loses on detectability: an empty result is an ordinary outcome in this system — a quiet repository, a narrow reader — so a shortened set reads as healthy indefinitely and nobody investigates. |
| A single deployment-wide budget | One number to reason about, one number to defend, and no per-table accounting. | Loses on fit: the cost of an aged grant differs per corpus by an order of magnitude, so a deployment-wide number is either too loose for its most sensitive table or too tight for its slowest source. |
| Exempt a never-swept source, since it has no lag to compare | A source can be landed and made useful before its sweep is built. | Loses on the same detectability criterion in its worst form: the source with no permission observations at all would be the one source with no staleness check, so the least-known corpus would be the most freely served. |
| Accept a budget tighter than the cadence and let reads fail | No diagnose-time check to build; the runtime refusal already exists. | Loses on where the failure lands: an unreachable budget is a declaration defect, discovered by a reader instead of by the operator who wrote it, with a message about staleness rather than about the number. |

## Criteria

1. **Detectability of the failure downstream** — whether an aged-authorization outcome is
   distinguishable from a healthy one by whoever receives it. *This criterion decides.*
   Both alternatives that keep the system available do so by making the failure quiet, and
   a quiet failure in this contract is a revoked grant that keeps answering. A refusal is
   the only outcome that cannot be mistaken for a correct narrow answer.
2. **Whether the freshness claim is checkable against the cadence that produces it** —
   whether a declared budget is a posture or an aspiration.
3. **Fit to the corpus** — whether the tolerable age can differ where the cost of an aged
   allow differs.
4. **Availability under a degraded sweep** — what fraction of reads survive when a sweep
   falls behind. This criterion is the one the decision spends.

## Consequences

Every servable table carries a number an operator declared and can defend, and the pair of
that number with its source's cadence is read together at diagnose, so the operator
defends the pair rather than the number alone. The refusal carries the source, the lag,
the budget and the last observation instant, which is enough to diagnose without opening
the store.

The accepted cost is availability. A sweep that falls behind takes the whole source
offline for reads rather than degrading, and because the watermark moves only on complete
coverage, a partially failing sweep produces exactly that outcome. On a source whose full
run is slow, the defensible budget is long, and an operator who wants it shorter has to
make the sweep faster rather than the number smaller.

Declaring budgets is ongoing work: a new table needs one, and a source whose cadence
changes invalidates the numbers declared against it.

Reversing toward best-effort serving is expensive because it is invisible — every
deployment would keep its declared budgets while none of them refused anything.

## Revisit triggers

- Measured sweep reliability on real sources produces refusal rates high enough that
  operators disable bound tables rather than repair sweeps.
- A source's sustainable cadence improves enough that budgets declared against the old
  figure are now needlessly loose.
- The in-band representation of the refusal on the tool surface is found to be swallowed
  by a consuming agent, which would make the failure quiet by a different route than the
  one this decision closed.
