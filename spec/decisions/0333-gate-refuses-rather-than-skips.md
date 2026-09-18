# 0333 — A baseline comparison refuses an unresolvable path, an empty sample, a malformed entry or a drifted run stamp rather than skipping it

**Status:** accepted 2026-09-18
**Decides:** `build.baseline.refusal.unresolvable-path`, `build.baseline.refusal.run-stamp-drift`

## Context

The quality gate is a comparison: a baseline file names metrics by the report's own field
paths — `retrieval.<modality>.<metric>`, `judge.<dimension>`, `slices.<tag>.<metric>`,
`latency_ms`, `n_cases` — and each entry gates the measured value against a committed one
within a dead band. The naming is deliberate; a path is the report's field path so that a
renamed field is visible to the comparison rather than invisible to it.

That visibility only exists if an unresolvable path fails. A comparison that skips what it
cannot find reads green while measuring nothing, and it does so most reliably in the case
that matters: a report field is renamed, every path referring to it stops resolving, and
the gate reports a pass over an empty set of comparisons. The same hole opens on a typo in
a hand-edited baseline, on a metric whose sample is empty, on a malformed entry, and on a
dead band that is not a finite number.

Two runs are also not automatically comparable. Recall@k is monotone non-decreasing in k,
so a baseline committed at one k is a lower bar at a larger one and the gate passes on a
change in configuration rather than on a change in quality. A rule-based judge fills the
same accuracy field a model judge fills, so a tier swap moves a number that names one thing
and measures another. Both are cheap to detect — the configuration is a block in the report
— and invisible in the number itself.

Cost matters in one direction here. The judged tier is the expensive part of a run. Every
condition that can be checked from the baseline file and the run's own configuration is
knowable before the suite starts.

## Decision

A path resolving to nothing, a metric measured over an empty sample, a malformed entry, or
a non-finite dead band raises `BaselinePathUnresolved` and fails the command. A run block
differing from the run's own configuration raises `BaselineRunStampMismatch`. Everything
checkable without the report is refused ahead of the suite, so a typo in a baseline costs
nothing in the judged tier.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refusing every unevaluable condition, and refusing ahead of the suite where possible** *(chosen)* | The gate cannot disable itself silently. A renamed field, a typo, an empty sample and a configuration drift each produce a named failure. | Adding a metric to the report requires a coordinated baseline edit, and a run stamp mismatch fails a run that may be otherwise healthy. |
| Skipping an unknown path with a warning | No coordination cost; the report and the baseline evolve independently; a warning is still in the log. | Lost on self-disabling: a typo then costs nothing and protects nothing, and a rename converts the whole gate to a green no-op with a warning nobody is gating on. |
| Comparing across differing k values or judge tiers | Runs stay comparable across configuration changes, so a baseline survives a tier swap. | Lost on comparability: recall@k is monotone non-decreasing in k and a rule-based judge fills the accuracy field a model judge fills, so the comparison is between two different quantities wearing one name. |
| Auto-updating a baseline when a path stops resolving | Self-healing; no coordinated edit; the file tracks the report. | Lost on self-disabling in its strongest form: the gate would rewrite its own bar in response to the change it exists to catch. |
| Refusing only after the suite, from the report alone | One code path; every condition checked in one place against real data. | Lost on cost placement: a typo in a baseline would then be discovered after the judged tier has run, which is the expensive part of the run and the part that was never in question. |

## Criteria

1. **Self-disabling** — whether a gate can report green while measuring nothing.
2. **Comparability** — whether two compared numbers measure the same quantity.
3. **Cost of a mistake** — how much of a run a typo or a rename consumes before it is
   named.
4. **Coordination cost** — work required to add or rename a metric.

**Self-disabling decides it outright.** A gate's only value is that it fails when the thing
it watches regresses; an arrangement where the gate can quietly stop watching has no value
that can be traded against convenience. Comparability is a second hard constraint rather
than a preference, since a comparison between two different quantities is a failure of the
same kind. Cost of a mistake then sets the placement, and coordination cost is what the
decision gives up.

## Consequences

A renamed report field surfaces as a refusal naming the path, so the baseline and the
report stay in correspondence by construction. A configuration drift is caught before the
numbers are read, so nobody reasons about a recall figure produced at a different k. A typo
in a hand-edited baseline costs a fast refusal rather than a judged run.

The cost accepted: adding or renaming a metric is a coordinated edit across the report and
every baseline file that names it, and there is no path where the tooling absorbs that for
you — by design, since absorbing it is the self-disabling behavior. A run stamp mismatch
also fails a run that may be otherwise healthy and whose numbers a reader would have found
useful, which is a real loss when the mismatch is a deliberate configuration change rather
than an accident.

Reversing this is cheap to do and hard to detect: relaxing a refusal to a skip is one
change, and after it the gate's greens no longer carry information about how many
comparisons stood behind them.

## Revisit triggers

- Coordinated baseline edits become frequent enough to be the dominant cost of adding a
  metric, measured against the metrics actually added.
- A comparison across k values or judge tiers becomes principled — a normalization with a
  stated error bound — at which point run-stamp equality is stricter than comparability
  requires.
- A baseline file format appears that names metrics by a stable identifier decoupled from
  the report's field path, which would separate a rename from an unresolvable path.
