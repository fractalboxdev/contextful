# 0167 — Permanence is its own column rather than an encoding of the attempt count

**Status:** accepted 2026-09-18
**Decides:** `derive.land.refusal.settled-unit-revived`

## Context

A marker row records two facts about a unit that produced nothing: how many times it has been
tried, and whether trying again could ever help. These read as the same fact because the
outstanding-set filter consults only one of them — a unit is re-selected while it is
unsettled — and the cheapest implementation makes "settled" mean "attempts have reached
the ceiling".

That encoding is wrong in a specific and dangerous way. The ceiling is `max_attempts`, an
operator-facing knob whose purpose is tolerating flaky publishers: a feed host that times out, a
service that rate-limits under load. An operator raising it is reasoning about transport, and the
change is routine. But the same ceiling would carry every permanent refusal in the table. The
pre-socket guards of 0161 — an unsupported scheme, an address literal, a loopback name, a host
absent from the operator's list — settle a unit because the machine declined to make a request,
not because a request failed three times. Under the encoding, raising the ceiling from 3 to 5
brings all of them back to life, re-attempts every address the machine refused to dereference, and
tells nobody it did so. The character-set refusals and every typed permanent engine error come
back with them.

The two facts also differ in who writes them. The attempt count is arithmetic the tier performs:
the prior count plus one, what was actually tried. Permanence is a judgement about the outcome,
made where the outcome is classified — the guard, the status mapping, the engine's typed error
kind. Collapsing them makes a judgement into a side effect of a counter.

Markers can also be several per unit, since each failed attempt writes one. The pair that governs
is taken from the marker with the highest attempt count, because that count rises monotonically per
unit while file ordering does not.

## Decision

`retryable` is its own column. It is null on a successful row. On a marker, `false` means settled
for good and is what the outstanding-set filter reads; a marker carrying no value at all reads as
unsettled, so nothing is stranded. `attempts` is the prior count plus one — what was actually
tried — rather than an encoding of permanence. A unit whose marker says settled re-entering the
outstanding set raises `DeriveSettledUnitRevived`. Raising an operator-facing attempt ceiling
revives no security refusal.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A separate permanence column, read by the outstanding-set filter** *(chosen)* | An operator-facing ceiling cannot revive a refusal that has to stay settled; the classification lives where the outcome is classified | Two columns carry one unit's fate, and a marker written with no permanence value re-attempts those units once |
| Encode permanence as an attempt count equal to the ceiling | One column, one filter, no new schema | Loses on exactly the deciding criterion: every permanent refusal — the scheme, address-literal, loopback and host-list refusals among them — comes back to life the moment somebody raises the ceiling, silently |
| Encode permanence as a sentinel attempt count outside the ceiling's range | One column, and the ceiling cannot reach the sentinel | Loses on honesty and on the same criterion at one remove: `attempts` stops meaning what was tried, so nobody can count real attempts, and a future ceiling validation that clamps the range re-opens the revival |
| Keep permanence in the status value alone | No new column; the status already distinguishes settled outcomes | Loses on separability: `failed` covers both a 5xx worth retrying and a refused scheme, so the filter would need a second table mapping status to permanence, maintained apart from the classification |
| Derive permanence from the error kind at filter time | Nothing extra is stored; one source of truth | Loses on stability: the mapping would be re-evaluated against current code for historical rows, so changing an error's classification silently revives or strands units landed under the old one |

## Criteria

1. **Whether raising an operator-facing cap can revive a refusal that has to stay settled.**
   Measured by asking what changing `max_attempts` does to units the guards refused.
2. **Honesty of the attempt count** — whether `attempts` states what was tried.
3. **Stability of a landed verdict** — whether a row's meaning survives a code change.
4. **Schema and consumer cost** — how many columns carry one unit's fate.

Criterion 1 decides alone. Every other criterion has a defensible answer under more than one
option; this one eliminates the entire family of encodings, because the coupling it describes is
invisible at the point of use — an operator turning a transport knob has no reason to suspect they
are also turning a security one, and nothing in the system would tell them. Criteria 2 and 3 then
independently rule out the sentinel and the derived variants, which is confirmation rather than
the deciding argument.

## Consequences

Easier: the outstanding-set filter reads one boolean and the attempt ceiling reads one integer,
and neither is a proxy for the other. A classification change at the guard or in the status
mapping is expressed where it is made. `DeriveSettledUnitRevived` makes the invariant testable
rather than aspirational.

Harder: two columns must agree in the row builder, and a marker that lands without a permanence
value is a real shape rather than an impossible one. Where several markers exist for a unit the
folding rule has to be stated, and it resolves ties toward the settled reading, which cannot loop.

Accepted cost: two columns carry one unit's fate, and a marker written with no permanence value
reads as unsettled, which re-attempts those units once. That is a deliberate direction — an extra
attempt is cheap and a stranded unit is not — but it means a builder defect costs one round of
engine calls per affected unit rather than being refused outright.

Expensive to reverse: the permanence column is what the guards' permanence rests on. Collapsing
it back into the attempt count would re-introduce the revival coupling for every settled unit in
the table at once, retroactively.

## Revisit triggers

- Markers are observed landing with no permanence value at any material rate, which means the
  builder's two columns are drifting and the tolerance is being spent.
- A legitimate need appears to re-open settled units — a host-list correction, a re-derive signal
  for a changed parent — which requires an explicit mechanism rather than a ceiling change, and
  would be decided on its own terms.
- The attempt ceiling stops being operator-facing, which would remove the coupling this record
  exists to prevent.
