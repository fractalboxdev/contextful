# D52 — A differentially tested reference model is the standing link between proofs and binary

**Status:** accepted

## Context

A theorem about a model says nothing about the binary unless something connects them. Refinement proves equivalence over a four-link trust chain — compiler lowering, an unpinned translator, hand-written external models, the build configuration — and a broken link yields a green proof about the wrong object with no artifact left. Differential testing yields a weaker statement that fails legibly.

## Decision

`assurance.differential-test` drives an executable reference model and the engine's decision functions over the same generated case. A disagreement shrinks to a minimal case and raises `ReferenceModelDrift` with both decisions; a run reporting one without writing a corpus entry raises `CounterexampleDiscarded`. Minimized cases replay before fresh ones, and the generator seed is recorded. `assurance.scope-claim` limits refinement to the pure decision functions — inclusion and grant narrowing; reaching cryptography, parsing adapters, database calls or concurrency raises `RefinementScopeExceeded`.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Differential reference model, refinement scoped to pure functions *(chosen)* | — | Evidence, not equivalence; a case outside the generator's classes goes uncovered; two implementations move together. |
| Refinement proof alone | Survivability of evidence | A broken translator would pass about the wrong object, leaving nothing to inspect. |
| The model alone | Relevance to the binary | No statement would reach the artifact that runs. |
| Translating cryptography, adapters, database calls and concurrency | Yield | The hand-written model set would grow with no guarantee gained. |
| Reporting disagreements without retaining them | Regression retention | A fixed divergence would return under another seed. |

## Consequences

- A unilateral change to either artifact surfaces as a disagreement on the next run.
- The report names the case classes it drew from: malformed, boundary and well-formed.
- The evidence holds whether or not anything is translated.

## Revisit

- A pinned, reproducible translation toolchain exists.
- A production divergence falls outside all three case classes.
- Keeping the two artifacts in step costs more than the divergences caught.
