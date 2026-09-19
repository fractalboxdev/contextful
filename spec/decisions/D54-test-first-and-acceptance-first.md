# D54 — A source change carries a test that fails first, and a milestone carries its acceptance test before it progresses

**Status:** accepted

## Context

A pin computes from a definition: a test reads as demonstrating its clause whether or not it ever failed, and an ignored test reads like one that runs. A test written after its code specifies only what the code already does. A milestone's reach is an end-to-end claim that unit pins can all satisfy while it goes unexercised.

## Decision

`assurance.test.test-first` holds every change altering Rust source under `crates/` or `tools/` to a test under a package's `tests/` that fails when run against the base commit's source; the gate overlays the change's test files on the base commit and requires a red run, and the workspace stage then requires green.

`corpus.state.acceptance-first` requires each milestone's acceptance test, named on its `Acceptance:` line and driving a built binary through a public surface, to exist before any clause of the milestone is pinned. The test stays ignored while the milestone is open. `corpus.state.verdict` counts an ignored test as broken, so a pin never rests on a test that does not run.

Every gate stage runs remotely as its own required status check, invoking the subcommand a contributor runs locally.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Red-before-green over the diff, acceptance test before the first pin *(chosen)* | — | Every source change pays a second build at the base commit; a refactor declares itself by trailer. |
| A line-coverage threshold | Order | A test written after its code satisfies it, and coverage says nothing about which assertion fails. |
| A test file changed alongside source | Vacuity | A test green against the base specifies nothing the change adds. |
| Per-change mutation testing | Wall clock | Minutes per mutated function across an engine-sized crate graph. |
| Review discipline alone | Detectability | A test written second is textually identical to one written first. |

## Consequences

- A bug fix lands with the test that reproduced it, and that test is known to fail without the fix.
- Unit tests inline in a source file satisfy the workspace stage but not the red check, so specifying tests sit under `tests/`.
- An acceptance package that links no workspace crate observes only what a caller observes.

## Revisit

- The base-commit build exceeds the stage wall clock.
- The refactor trailer appears on changes that alter a public surface.
