# 0259 — The single-operator diagnostic fails closed on missing disclosure coverage and on an unreadable model file

**Status:** accepted 2026-09-18
**Decides:** `disclosure.set-mode.refusal.policy-coverage`, `disclosure.set-mode.refusal.model-file`

## Context

In the single-operator setting one party holds every contributor's rows, and the property a
published aggregate owes is that it exposes no individual contributor. That property is
carried by a disclosure policy attached to the model and enforced when the table is built.
A published aggregate-shaped model with no policy attached has nothing enforcing it.

The diagnostic that catches this runs over project files alone — the manifest plus the
referenced statement text of each published model, read from local disk, with no requests
against the object store. That offline property is what makes it runnable before a
deployment has credentials, which is when configuration mistakes are cheapest to fix.

Two findings sit at the boundary between a warning and a gate. The first is a published
aggregate-shaped model carrying neither a policy nor a recorded opt-out: the precondition is
simply absent. The second is a model whose statement text does not read from disk — a bad
path, a permissions error, a file that was never committed.

The second case is the sharper one. The aggregate-shape test is a scan over statement text.
When the text cannot be read, the scan produces no finding, and a file that cannot be read
is indistinguishable from a file that would have failed. Passing over it makes an unreadable
file the cheapest way past the check, and the cheapest path is the one that gets taken by
accident.

The diagnostic already distinguishes findings that warn from findings that fail. A table
declaring no subject column, and a declared subject column with no row-level rule, both
warn and leave it passing — so the question is not whether warnings exist but which of these
two belongs among them.

## Decision

A published aggregate-shaped model carrying neither a disclosure policy nor a recorded
opt-out fails the diagnostic with `DisclosurePolicyAbsent`. A published model whose
referenced statement text does not read raises `DisclosureModelUnreadable`. The diagnostic
issues no requests against the object store and validates a configuration with no network
reachable.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Both findings fail the diagnostic; validation stays offline** *(chosen)* | No unverifiable precondition reaches production quietly, and the check runs before credentials exist. | An operator whose model the shape scan misreads writes an explicit non-aggregate declaration before passing, and a transient filesystem error reds the check rather than degrading it. |
| Warn on a missing policy and pass | Deployments are never blocked by configuration the operator intends to finish. | Loses on whether an unverifiable precondition reaches production silently: a warning in a log is not a gate, and the published table is built either way. |
| Skip a model whose statement text does not read | Tolerant of partial checkouts and generated files that are not present at check time. | Loses on the same criterion, more sharply: an unreadable file becomes the cheapest way past the check, so the failure mode is reached by accident rather than by intent. |
| Validate against the live object store, reading what was actually published | Checks the deployed truth rather than the declared one. | Loses on the offline criterion: the diagnostic would stop being runnable before a deployment has credentials, which is exactly when configuration errors are cheapest to fix. |
| Infer a default policy where none is attached | Nothing is ever unprotected, and no operator action is needed. | Loses on reviewability: the thresholds governing a published table would be invisible in the manifest, and the recorded policy hash would name a value nobody chose. |

## Criteria

1. **Whether an unverifiable precondition reaches production.** *This criterion decides.*
   Both findings describe a state where the check cannot establish the property, and a
   check that passes what it could not verify reports a guarantee it does not have — which
   is worse than no check, since it is relied upon.
2. **Whether the diagnostic runs with no network.** The offline property is what puts the
   check before deployment rather than after it.
3. **Whether the governing thresholds stay legible in the manifest.** Rules out inference.
4. **Tolerance of transient and partial-checkout states.** Given up.

## Consequences

The shape scan is deliberately blind in one direction — a global rollup, a distinct
projection and a windowed partition spell no grouping clause and pass unflagged — so the
gate catches the models it recognizes and leaves the rest to the operator's own declaration.
A policy on such a model still governs its build; the declaration silences only the coverage
finding.

The accepted cost is friction on false positives. Where the scan reads a model as an
aggregate and the operator disagrees, the way forward is an explicit declaration that the
output is not a cross-subject aggregate, capped at one entry per model. That is a real edit
made to satisfy a check, and it is the price of the check being a gate at all.

A transient filesystem error reds the diagnostic. That is noise in a pipeline and the
alternative is the failure mode above, so the noise is accepted rather than smoothed.

Reversing toward warnings is cheap to implement and silent in effect: nothing in a built
table records that its policy was absent rather than satisfied.

## Revisit triggers

- The aggregate-shape test gains coverage of rollups, distinct projections and windowed
  partitions, which would change how much the operator's declaration is carrying.
- Non-aggregate declarations are observed accumulating across a deployment's models, which
  would mean the scan's false-positive rate is the real cost rather than a corner.
- A model source appears that is generated at build time and legitimately absent at check
  time, which the unreadable-file rule refuses by construction.
