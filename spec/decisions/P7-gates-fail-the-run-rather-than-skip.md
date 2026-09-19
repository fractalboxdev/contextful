# P7 — Gates fail the run rather than skip

**Status:** accepted

## Context

A gate that skips what it cannot evaluate disables itself silently: a renamed baseline path protects nothing, an exclusion asserted over an empty collection passes, and a size figure nobody gates on grows until a deploy fails. Each reads green on the change that broke it.

## Decision

A check the gate cannot evaluate fails the run, and a gated number is asserted, never logged. A guard proves itself by firing: the state it stands against reds the suite.

- `assurance.test` requires an absence assertion to construct and verify the condition it names first, and refuses an exclusion over an empty collection.
- `assurance.gate` starts a stage only with 2 GiB free disk, holds build size to 12 GiB, and asserts each profile's compressed size and dynamic dependencies per change; the multi-day residency soak runs on its own cadence.
- `assurance.baseline` refuses an unresolvable path, an empty sample, a malformed entry and a drifted run stamp, ahead of the suite wherever the check needs no report.
- `assurance.automate` puts control flow in typed subcommands the gate and a contributor invoke identically; surviving shell passes `shellcheck`.
- `assurance.gate` checks every TypeScript surface in one stage; a surface declaring no script for a check skips that check, a declared absence rather than an unevaluable input.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Fail the run on anything unevaluable *(chosen)* | — | A typo in a baseline or a full disk reds an unrelated change until fixed. |
| Log the figure or warn | Diagnosability | A number nobody gates on is read by nobody, and the first signal is an outage. |
| Check on a nightly cadence | Drift window | The failure arrives detached from the change that caused it. |
| Review assertions for vacuity by eye | Detectability | A vacuous assertion is textually identical to a real one. |
| Auto-update a baseline when a path stops resolving | Self-disabling | The gate rewrites its own bar in response to the regression. |

## Consequences

- Every gate outcome is reproducible locally with the same command.
- Cheap assertions run per change; expensive ones run on a cadence and name the change range they cover.
