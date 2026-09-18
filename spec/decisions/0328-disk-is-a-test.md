# 0328 — Free disk is a stage precondition and build size is a gated number, both failing the run rather than logging a warning

**Status:** accepted 2026-09-18
**Decides:** `build.gate.refusal.free-disk-precondition`, `build.gate.refusal.build-size-exceeded`

## Context

The gate runs inside one container with four finite ceilings, and disk is the one a cargo
build spends fastest. A single invocation that rebuilds the bundled SQL engine spends
2.2 GiB of build directory and emits a 1.1 GiB static library, against 18 GiB of usable
disk for the whole run. Stages build into their own directories and reclaim on pass, which
holds peak at one stage rather than at their sum — but the headroom that arrangement buys
is measured in a small number of gigabytes, not in orders of magnitude.

Disk exhaustion partway through a build does not announce itself as disk exhaustion. It
surfaces as a link error, a truncated artifact, a write failure inside a dependency's build
script, or a compiler message about a file it could not open. None of those name the
resource, and each of them reads as a defect in whatever code happened to be compiling at
the moment the space ran out. The same change is red on one run and green on the next
depending on what an earlier stage left behind.

A precondition check reads differently. Before a stage does any work, the free space is a
number, the floor is a number, and the comparison names the stage and both figures. The
diagnosis is complete before any compilation has happened, and the run costs nothing beyond
the check.

Total build size is the other half. It is the quantity that determines whether the free-disk
floor is reachable at all on the next run, and it grows through ordinary work — a
dependency added, a feature unified differently, a new stage. Growth is legitimate; growth
that nobody notices until a stage fails on an unrelated error is not.

## Decision

A stage begins work with 2 GiB of free disk available to it, and a stage starting below
that floor raises `BuildDiskPrecondition` and exits 28 before doing work. The budget stage
holds total build size to 12 GiB, printing that size, the free space remaining and the
largest artifacts on every run, and build size past that ceiling raises
`BuildSizeCeilingExceeded`, naming the artifacts at the head of the list.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A precondition that reds the run, and a gated size ceiling** *(chosen)* | The failure names its own resource, at the stage boundary, before any work. The ceiling makes growth visible as a red run attributable to one change. | A legitimate growth in build size reds the gate until the ceiling is raised deliberately, and a near-miss run spends time printing an artifact inventory. |
| Logging build size without failing on it | No false reds; the number is available to anyone who looks; growth is recorded run over run. | Lost on diagnosability: a figure nobody gates on is a figure nobody reads, and the first signal remains an unrelated link error several stages later. |
| Raising the container's disk allocation instead | Immediate headroom with no change to the tree, and no red runs from growth. | Lost on growth curve: it moves the same failure later without changing its shape, and the next exhaustion is diagnosed exactly as badly as this one. |
| Reclaiming aggressively inside a stage rather than at its boundary | Lower peak, so more stages fit in the same container. | Lost on reproducibility: peak then depends on when reclamation ran relative to compilation, so the same change occupies a different high-water mark run to run and the ceiling stops meaning anything. |
| Inferring exhaustion after the fact from the failing stage's logs | No preflight cost, and it catches the case where a stage exhausts disk mid-run rather than starting short. | Lost on diagnosability again: the inference is a heuristic over error text that names other things, so it reports a hypothesis where the precondition reports a number. |

## Criteria

1. **Diagnosability** — whether the failure names the resource that caused it, and how
   much work happens before it does.
2. **Reproducibility** — whether the measured peak depends on timing inside a stage.
3. **False-red rate** — how often the gate reds for growth that is legitimate.
4. **Cost per run** — time the checks add to a green run.

**Diagnosability decides it.** Every other criterion is about the comfort of the gate;
this one is about whether a red run can be acted on at all, and disk exhaustion is the
failure mode in this container that is worst at explaining itself. The ceiling is set
tight enough that real growth trips it and loose enough that ordinary variance between runs
does not — 12 GiB against 18 GiB of usable disk. Whether that margin is correctly placed is
unmeasured until the tree has accumulated enough growth history to say.

## Consequences

A stage that cannot complete says so before compiling anything, naming its floor and its
actual free space, and exits with a code that identifies the resource. Every run prints the
size picture whether or not it passed, so a near miss is readable from the log without a
local reproduction. Growth in build size attributes to the change that caused it, because
the red run is that change's run.

The cost accepted: a legitimate growth reds the gate for whoever is unlucky enough to cross
the line, and unblocking requires raising the ceiling deliberately rather than as part of
the change. That is the intended friction, and it is friction charged to the wrong person
about half the time — the change that crosses the line is rarely the change that caused
most of the growth. A near-miss run also spends time enumerating artifacts that a passing
run does not need.

Reversing this is cheap: both are numbers in a configuration, and relaxing either to an
advisory returns the previous behavior in one commit. What is expensive to recover is the
attribution — once a ceiling is advisory for a while, the growth that accumulated under it
has no single change to point at.

## Revisit triggers

- The 12 GiB ceiling reds runs for variance rather than growth, measured as reds that pass
  on an unchanged re-run.
- The container's ceilings change, making both the floor and the ceiling stale numbers
  against a different allocation.
- A build arrangement lands whose peak is bounded by construction rather than by
  measurement, at which point the precondition measures something the arrangement already
  guarantees.
