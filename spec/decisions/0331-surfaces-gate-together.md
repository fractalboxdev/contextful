# 0331 — The TypeScript surfaces run typecheck, unit tests and their framework build in one stage, optional by presence

**Status:** accepted 2026-09-18
**Decides:** `build.gate.refusal.surface-check-failed`

## Context

The tree carries several TypeScript surfaces — a shared component package, a deploy
emitter, the consoles, a headless client and a shared infrastructure module. They are not
independent. The component package is a dependency of the consoles, the shared
infrastructure module is a dependency of the deploy emitter, and the headless client is
built against the same route shapes the engine serves.

That dependency structure decides where a break appears. A change to a shared component
type-checks fine in its own package and breaks a consumer that passes it different props.
If each surface gates on its own pipeline, the component's pipeline is green, the change
merges, and the consumer's pipeline discovers the break on its next run — which may be a
later change by someone else, or may be the deploy. Either way the break is attributed to
the wrong change and diagnosed by the wrong person.

The surfaces are also unevenly shaped. Some have a framework build; the headless client
does not. Some carry unit tests; a thin emitter may not. A stage that requires every
surface to declare every script forces the surfaces without one to carry a placeholder
script that exits zero, which is a line of configuration that exists to satisfy a checker
and that silently starts lying the day the surface grows the real thing.

## Decision

The TypeScript surfaces run typecheck, unit tests and their framework build in one stage of
their own, so a change to a shared component reds review rather than surfacing at deploy.
Each surface's checks are optional by presence: a surface declaring no script for a given
check is skipped rather than failing the stage. A surface whose typecheck, unit tests or
framework build fails raises `SurfaceCheckFailed`, naming the surface and the script.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **One stage over every surface, checks optional by presence** *(chosen)* | A cross-surface break reds the review of the change that caused it. A surface joins the stage by existing, with no configuration to add. | One stage's failure covers several surfaces, so a red run names the surface and the script rather than isolating one package's pipeline. |
| A separate pipeline per surface | Clean isolation: a red pipeline names exactly one package, and each surface's cadence is its own. | Lost on timing: a cross-surface break appears after merge, in someone else's run or at deploy, and is attributed to whichever change happened to trigger the consumer's pipeline. |
| Requiring every surface to declare every script | Uniform configuration; the stage reads the same for all surfaces, and a missing script is a real signal. | Lost on churn: a surface with no framework build carries a placeholder that always passes, which is configuration with no content and which stops being a placeholder without anyone noticing. |
| Gating only the surfaces that consume a shared package | Cheapest per run; the isolated surfaces skip entirely. | Lost on coverage: the dependency structure changes as surfaces grow, so the set of consumers has to be maintained by hand and is wrong exactly when a new consumption is added. |
| Folding the surfaces into the workspace stage | One fewer stage, and one verdict for the whole change. | Lost on diagnosability: the workspace stage is the run's most expensive, and a TypeScript type error would then sit behind an engine build that has nothing to do with it. |

## Criteria

1. **Timing** — when a cross-surface break surfaces relative to the change that caused it.
2. **Configuration churn** — configuration a surface carries to satisfy the checker rather
   than to describe itself.
3. **Failure isolation** — how precisely a red run names what broke.
4. **Cost per run** — work added to a run that touches no TypeScript.

**Timing decides it.** The surfaces share types, so the defect class worth gating is the
cross-surface one, and any arrangement that lets each surface pass alone is structurally
unable to catch it before merge. Configuration churn then chooses presence-based
optionality over declared scripts, since the alternative satisfies uniformity with content
that is empty by construction. Failure isolation is the criterion the chosen option loses
on, and it is accepted.

## Consequences

A change to a shared component is checked against every consumer in the same run, so the
break is discussed in that change's review. A new surface is covered the moment it exists,
with no stage configuration to remember, and a surface that grows a framework build starts
having it checked by adding the script.

The cost accepted: one stage covers several surfaces, so a red stage names the surface and
the script rather than pointing at a package's own pipeline, and a failure in one surface
stops the stage before the others are checked — a run can therefore report one break while
a second is waiting behind it. The presence-based optionality carries its own quieter cost:
a surface that should have unit tests and declares no script is skipped silently, and
nothing in the run distinguishes "has no tests to run" from "has tests nobody wired up".

Reversing this is cheap — splitting the stage per surface is a configuration change — but
it returns the cross-surface break to post-merge, which is the condition the stage exists
to remove.

## Revisit triggers

- The stage's wall clock grows past what the run's per-stage ceiling allows, forcing a
  split regardless of the timing argument.
- Surfaces stop sharing packages, which removes the cross-surface defect class and with it
  the only reason they gate together.
- Silently-skipped checks are found to be hiding a surface with real tests nobody wired up,
  which would argue for a declared-script model for at least the check that was missed.
