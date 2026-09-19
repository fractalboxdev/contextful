# D46 — Every access to data traverses the enforcement stack, and no configuration disables it

**Status:** accepted

## Context

A path that reads a row around the enforcement stack returns rows that look correct — same columns, plausible values — with masking unapplied, the visibility semi-join unrun and no audit entry. A disabled stack starts, passes its health check and answers. Neither failure announces itself, and every surface holding a convenient handle to data has a locally reasonable excuse to read beneath the stack.

## Decision

Complete mediation is a property of the composition, not a setting. `topology.compose` makes every function returning or releasing a stored row take the enforcement stack's admission value, so a row path skipping the stack does not type-check; `assurance.gate` holds that signature and raises `CrateGraphViolation` on a run-path crate reaching read-path crates outside the crossings. No flag, environment variable or build feature turns the stack off.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Mediation as a build property, no disable switch *(chosen)* | — | A surface holding the only handle to data writes an adapter through the stack: a network hop, or a generated artifact with a staleness check. |
| An operator flag marking a trusted path | Failure mode | A flag set once for one job would stay set, and the deployment would look healthy. |
| Per-surface opt-in enforcement | Number of places the property holds | Every new surface would be a fresh chance to omit it. |
| Enforcement asserted by code review alone | Detectability over time | A bypass introduced in a refactor would read as a call-site move. |

## Consequences

- The audit states the property as one question about the dependency graph, naming a code path.
- A fast bulk export or an offline diagnostic is slower than a direct read.
- Adding a disable path later would force re-establishing every layer's guarantee under both settings.

## Revisit

- An adapter through the stack cannot meet a stated throughput requirement.
- Audit bypasses cluster in one tooling category, showing the stack lacks an interface it needs.
- A layer proves to be a no-op on some path.
