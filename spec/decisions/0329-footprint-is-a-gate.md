# 0329 — Each profile's compressed size and dynamic dependency set are asserted per change rather than measured periodically

**Status:** accepted 2026-09-18
**Decides:** `build.gate.refusal.footprint-over-budget`

## Context

Three profiles carry declared compressed and idle-resident budgets, and those budgets are
what the product rests on rather than decoration: the read replica's budget is what
makes it deployable into a function-class environment with a cold-start bound, and the
dynamic dependency set is what makes an artifact runnable on a host that offers nothing but
the platform C library.

Footprint moves through ordinary work. A dependency added for one feature pulls a
transitive tree into every profile that enables it. A feature unification changes which
code is reachable. A crate that declares an engine dependency decides the feature list
every profile receives, so a change in one place moves bytes in three.

The assertions available split cleanly by cost. Building for the static-linked target,
compressing, reading a byte count and enumerating dynamic dependencies is a bounded amount
of work per profile — an ordinary cross-build and a compression pass. The cold-start bound
needs a function-class environment and repeated cold invocations. The resident-set soak
holds the seventh day's figure within 5 percent of the first day's, which is a seven-day
observation by definition.

The question is which of those run per change. A budget checked on a slower cadence permits
drift to accumulate between checks, and when it finally reds, the growth belongs to
whatever set of changes landed in the window rather than to one of them.

## Decision

Per change and per profile, the footprint step builds for the static-linked Linux target,
compresses the artifact, holds its byte size at or under that profile's declared budget,
and holds its dynamic dependency set to the platform C library. An artifact over budget, or
carrying a dependency beyond that library, raises `FootprintBudgetExceeded` and names the
profile. The read-replica profile additionally carries a function-class cold-start bound,
and the residency soak holds the seventh day's resident set within 5 percent of the first
day's; both run on their own cadence.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Per-change assertion for the cheap checks, a separate cadence for the expensive ones** *(chosen)* | Byte growth and a new dynamic dependency attribute to the single change that introduced them. The expensive observations still happen, without charging every change for a seven-day window. | Each change pays a static-linked cross-build and a compression pass per profile, and the expensive assertions keep a drift window whose width is unresolved. |
| A nightly or weekly footprint job covering everything | One cadence, cheapest per change, and the expensive observations fit naturally. | Lost on drift window for the cheap assertions: days of accumulated growth red at once with no attribution to a change, which is the failure this gate exists to prevent. Retained for the expensive assertions, where the same objection is outweighed by their cost. |
| Review-time judgment against the budget table | Zero machine cost; a human reads the numbers alongside the change. | Lost on enforceability: a number nothing asserts is a number that moves. The reviewer has no measurement in front of them unless someone produced one. |
| Asserting uncompressed size instead, as a cheap proxy | Much faster — no compression pass, and the number tracks roughly with the compressed one. | Lost on fidelity: the budget is stated in compressed bytes because that is what a deployment transfers, and compressibility varies enough between a data-heavy change and a code-heavy one that the proxy misses in both directions. |
| Asserting only the profile with the tightest budget | One cross-build per change rather than three; catches most growth, since the read replica is the constrained profile. | Lost on coverage of the dependency check: a new dynamic dependency can appear in a profile the read replica does not build, and that artifact is the one that fails to start on a bare host. |

## Criteria

1. **Drift window** — how much growth can accumulate before the assertion notices, and
   whether the red attributes to one change.
2. **Cost per run** — build and compression work charged to every change.
3. **Enforceability** — whether the budget is checked by something that fails, rather than
   by someone who might look.
4. **Fidelity** — whether the measured quantity is the quantity the budget is stated in.

**Drift window decides it for the cheap assertions.** Where an assertion is inexpensive
enough to run per change, the only argument for a slower cadence is the cost it saves, and
that saving buys days of unattributed drift. The same criterion loses to cost for the
cold-start bound and the residency soak, which is why those are on a separate cadence
rather than deleted.

## Consequences

A change that adds bytes to a profile reds the run that carries it, naming the profile, so
the growth is discussed while the change is still in review and the person who introduced
it is the person looking at it. A new dynamic dependency is caught before an artifact ships
that will not start on a host offering only the platform C library.

The cost accepted: each change pays a static-linked cross-build and a compression pass per
profile, which is a fixed tax on every run including runs that touch no code that could
move a byte. And the split is honest about its own gap — the cold-start bound and the
seven-day residency soak run on a cadence whose drift window is unresolved, so a regression
in either is attributable to a window rather than to a change.

Reversing this is cheap: moving the cheap checks onto the slower cadence is a scheduling
change. What it costs is the attribution, which cannot be recovered retroactively for the
window it was absent.

## Revisit triggers

- The per-change cross-build and compression pass become a material share of a gate run's
  wall clock against the per-stage ceiling.
- A cheap proxy for the cold-start bound appears — a measurement correlating with cold
  start that needs no function-class environment — moving that assertion into the per-change
  set.
- The residency soak's drift window is named in the budget stage, resolving the open
  question about what a soak regression attributes to.
