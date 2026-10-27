# P7 — Gates fail the run rather than skip

**Status:** accepted

## Context

A gate that skips what it cannot evaluate disables itself silently: a renamed baseline path protects nothing and reads green.

## Decision

A check the gate cannot evaluate fails the run, and a gated number is asserted, never logged. A guard proves itself by firing: the state it stands against reds the suite.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Fail the run on anything unevaluable *(chosen)* | — | A typo in a baseline or a full disk reds an unrelated change until fixed. |
| Log the figure or warn | Diagnosability | Nobody reads an ungated number, and the first signal is an outage. |
| Check every assertion nightly | Drift window | A cheap check's failure arrives detached from its change. |
| Auto-update a baseline when a path stops resolving | Self-disabling | The gate rewrites its own bar in response to the regression. |

## Consequences

- Every gate outcome is reproducible locally with the same command.
- Cheap assertions run per change. Only a check too costly per change runs in the nightly tier, against the default-branch head, naming the change range it covers.
